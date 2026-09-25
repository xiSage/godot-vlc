extends SceneTree

# The GPU half of media_switch.gd. Here the picture a media change drops is not in the software
# texture at all -- the control draws a render-device texture while the GPU output is driving --
# so what has to change is which texture the control draws with: the blank software one until
# the new video output hands the importer a texture of its own.
#
# The old frame is deliberately left in the render-device texture. The importer copies the
# shared texture into it on every render frame, so clearing it would only fill it again from
# the same shared texture, and it is not on screen while the control points elsewhere.
#
# The end of the blank is the new video output, and that is what the last assertion is about:
# the picture comes back, and frames keep landing in the texture it came back with.
#
#   godot --path demo --script res://tests/media_switch_gpu.gd     (windowed: D3D12 must load)

const VIDEO := "res://test.mp4"
const OTHER := "res://../test/media/h264_64x64_1s.mp4"
const WAIT_TIMEOUT_MS := 25000
const GPU_TIMEOUT_MS := 30000
const FRAMES_GPU_READY := 30

# A member rather than a local: a GDScript lambda captures a local by value, so a local counter
# would stay at zero while the lambda counted.
var cleared := 0


func _init() -> void:
	# A script error raised after an `await` kills this coroutine, and a windowed Godot whose
	# script never reaches `quit()` then hangs.
	create_timer(180.0).timeout.connect(func() -> void:
		push_error("media_switch_gpu FAIL: 180s without finishing")
		quit(1)
	)

	var player := VLCMediaPlayer.new()
	player.force_hardware = true
	player.frame_cleared.connect(func() -> void: cleared += 1)
	player.media = VLCMedia.load_from_file(ProjectSettings.globalize_path(VIDEO))
	player.autoplay = true
	root.add_child(player)

	if not await _wait_until(func() -> bool: return player._debug_gpu_active(), GPU_TIMEOUT_MS):
		_fail("the GPU backend did not initialize; is the rendering driver D3D12?")
		return
	if not await _wait_until(func() -> bool: return player._debug_frames_copied() >= FRAMES_GPU_READY, WAIT_TIMEOUT_MS):
		_fail("the GPU output copied no frames within %dms" % WAIT_TIMEOUT_MS)
		return
	if player._debug_gpu_picture_hidden():
		_fail("the picture is hidden while the first media plays")
		return

	# The change. The control has to be off the GPU texture by the time this returns: a caller
	# that draws in this frame would still be drawing over the old video.
	var copies_before := player._debug_frames_copied()
	player.media = VLCMedia.load_from_file(ProjectSettings.globalize_path(OTHER))
	if cleared != 1:
		_fail("assigning a different media emitted frame_cleared %d times" % cleared)
		return
	if not player._debug_gpu_picture_hidden():
		_fail("the control is still drawing the GPU texture after the media changed")
		return
	# The render-device texture keeps the previous video's pixels, which is what this readback
	# sees while the control is pointed away from it.
	print("media_switch_gpu: hidden, dst avg = %s, copies = %d" % [
		player._debug_dst_pixel_avg(), player._debug_frames_copied()
	])

	# The blank ends when the new video output announces itself with a texture of its own.
	var deadline := Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	var blank_frames := 0
	while player._debug_gpu_picture_hidden():
		if Time.get_ticks_msec() >= deadline:
			_fail("no new video output within %dms" % WAIT_TIMEOUT_MS)
			return
		blank_frames += 1
		await process_frame

	var handover := player._debug_frames_copied()
	print("media_switch_gpu: the blank lasted %d frames, dst avg at handover = %s, copies = %d" % [
		blank_frames, player._debug_dst_pixel_avg(), handover
	])

	# And the picture that came back has to be getting frames: the importer keeps copying into
	# whatever destination the new output bound.
	if not await _wait_until(func() -> bool: return player._debug_frames_copied() > handover, WAIT_TIMEOUT_MS):
		_fail("the new video output copied no frames within %dms" % WAIT_TIMEOUT_MS)
		return
	if player._debug_gpu_picture_hidden():
		_fail("the picture went back to hidden while it plays")
		return

	# Not a change either, on this path: neither makes libvlc build a new video output, so the
	# blank would never end.
	player.media = player.media
	player.media = null
	if cleared != 1:
		_fail("assigning the media the player already has, or null, emitted frame_cleared")
		return
	if player._debug_gpu_picture_hidden():
		_fail("the picture is hidden after assigning the media it already plays")
		return

	print("media_switch_gpu OK: a media change hid the GPU picture for %d frames, and the new video output brought it back (copies %d -> %d)" % [
		blank_frames, copies_before, player._debug_frames_copied()
	])
	player.stop_async()
	for _i in 30:
		await process_frame
	quit(0)


func _wait_until(predicate: Callable, timeout_ms: int) -> bool:
	var deadline := Time.get_ticks_msec() + timeout_ms
	while not predicate.call():
		if Time.get_ticks_msec() >= deadline:
			return false
		await process_frame
	return true


func _fail(what: String) -> void:
	push_error("media_switch_gpu FAIL: %s" % what)
	quit(1)
