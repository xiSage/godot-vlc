extends SceneTree

# Changing the media must not show the old video while the new one starts (issue #17).
#
# The window is a second wide, and it is libvlc's: it tears the old input down, builds the new
# one, and only then does a video output announce itself. What the old output had already
# produced is in the extension's channel by then -- measured on res://test.mp4, exactly one
# frame of the old output arrives after the property is assigned, and the first frame of the
# new one lands about a second later. Before this, the texture kept the old frame for that
# second, which is the old video the issue is about.
#
# Assigning a different media drops the picture instead: the texture is blank (1x1 and
# transparent, as the signal documents), get_frame() answers null, and the frames already on
# their way from the old output are dropped rather than shown. The second media here is a much
# smaller video than the first on purpose: the frame that ends the blank has to be the new
# media's, which the sizes are what make visible.
#
# What does not clear is pinned too, because both would be one-way: assigning the media the
# player already has (libvlc is not asked to change anything, so no new video output would
# announce itself and a blank picture would stay blank), and assigning null (update_media has
# nothing to hand libvlc, which keeps playing what it has -- a blank picture there would be a
# black player over live audio).
#
#   godot --headless --path demo --script res://tests/media_switch.gd

const VIDEO := "res://test.mp4"
const OTHER := "res://../test/media/h264_64x64_1s.mp4"
const WAIT_TIMEOUT_MS := 25000

# Members rather than locals: a GDScript lambda captures a local by value, so a local counter
# would stay at zero while the lambda counted.
var cleared := 0
var frames := 0


func _init() -> void:
	# A script error raised after an `await` kills this coroutine, and a headless Godot whose
	# script never reaches `quit()` then runs forever instead of failing.
	create_timer(120.0).timeout.connect(func() -> void:
		push_error("media_switch FAIL: 120s without finishing")
		quit(1)
	)

	var player := VLCMediaPlayer.new()
	player.video_frame.connect(func() -> void: frames += 1)
	player.frame_cleared.connect(func() -> void: cleared += 1)

	# Nothing has been shown yet, so the first assignment is not a change: no signal.
	player.media = VLCMedia.load_from_file(VIDEO)
	if cleared != 0:
		_fail("assigning the first media emitted frame_cleared")
		return
	player.autoplay = true
	root.add_child(player)

	if not await _wait_for_frames(1):
		_fail("no frame arrived within %dms" % WAIT_TIMEOUT_MS)
		return
	var shown := _size(player)
	if shown.x <= 1 or shown.y <= 1:
		_fail("the picture is %dx%d, not the video's" % [shown.x, shown.y])
		return

	# The change. The picture has to be gone by the time the assignment returns, not a frame
	# later: a caller that draws in that frame would still be drawing over the old video.
	var before := frames
	player.media = VLCMedia.load_from_file(OTHER)
	if cleared != 1:
		_fail("assigning a different media emitted frame_cleared %d times" % cleared)
		return
	var blank := _size(player)
	if blank.x != 1 or blank.y != 1:
		_fail("the texture is %dx%d after the media changed, not blank" % [blank.x, blank.y])
		return
	if player.get_frame() != null:
		_fail("get_frame answered a frame after the media changed")
		return

	# And it has to stay blank until the new video output produces a frame: the frame the old
	# output had already sent must not reach the texture.
	var deadline := Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while frames == before:
		if Time.get_ticks_msec() >= deadline:
			_fail("the new media produced no frame within %dms" % WAIT_TIMEOUT_MS)
			return
		var held := _size(player)
		if held.x != 1 or held.y != 1:
			_fail("the texture showed %dx%d before the new video output produced a frame" % [held.x, held.y])
			return
		await process_frame

	var back := _size(player)
	if back == shown:
		_fail("the picture came back at the old media's %dx%d" % [back.x, back.y])
		return
	if back.x <= 1 or back.y <= 1:
		_fail("the picture came back as %dx%d" % [back.x, back.y])
		return
	if player.get_frame() == null:
		_fail("get_frame answered no frame while a picture is showing")
		return

	# The media the player already has is not a change: libvlc would be handed the media it is
	# already playing, so nothing would announce itself and the blank would stay.
	player.media = player.media
	if cleared != 1:
		_fail("assigning the media the player already has emitted frame_cleared")
		return
	if _size(player) != back:
		_fail("assigning the media the player already has changed the picture")
		return

	# Nor is null: libvlc keeps playing what it has, and the picture stays with it.
	player.media = null
	if cleared != 1:
		_fail("assigning null emitted frame_cleared")
		return
	if _size(player) != back:
		_fail("assigning null changed the picture")
		return

	print("media_switch OK: a media change blanked %dx%d into %dx%d, and the new video output brought the picture back at %dx%d after %d frames" % [
		shown.x, shown.y, blank.x, blank.y, back.x, back.y, frames - before
	])
	player.stop_async()
	for _i in 30:
		await process_frame
	quit(0)


func _size(player: VLCMediaPlayer) -> Vector2i:
	var texture := player.get_texture()
	return Vector2i(texture.get_width(), texture.get_height())


func _wait_for_frames(count: int) -> bool:
	var deadline := Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while frames < count:
		if Time.get_ticks_msec() >= deadline:
			return false
		await process_frame
	return true


func _fail(what: String) -> void:
	push_error("media_switch FAIL: %s" % what)
	quit(1)
