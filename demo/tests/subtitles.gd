extends SceneTree

# Subtitles through the engine: the imported resource, the runtime MRL form, the two
# subtitle knobs, attaching one to a playback that is already running, and the one
# claim the Rust acceptance test cannot make -- that the subtitle is really in the
# frames LibVLC hands over, rather than merely attached.
#
# Subtitle pixels are part of those frames: LibVLC's video output composites them, so
# two frames taken at the same point of the same media differ by whether a subtitle was
# shown. The comparison is done on the bottom strip, where subtitles are drawn, with
# playback paused so that the video content is the same in both.
#
#   godot --headless --path demo --script res://tests/subtitles.gd

const WAIT_TIMEOUT_MS := 20000
const FRAME_TIMEOUT_MS := 5000

# The first cue of demo/subtitle.srt covers 1-4 s.
const CUE_MS := 2000

var player: VLCMediaPlayer


func _init() -> void:
	player = VLCMediaPlayer.new()
	root.add_child(player)
	await process_frame

	# The imported resource: res:// is handled by the importer, so this is what a
	# scene or a script gets back from load().
	var imported: Variant = load("res://subtitle.srt")
	if not (imported is VLCSubtitle):
		_fail("res://subtitle.srt did not load as a VLCSubtitle, but as %s" % imported)
		return
	var mrl: String = imported.get_mrl()
	if not mrl.begins_with("data:;base64,"):
		_fail("the imported subtitle carries %s instead of a data: MRL" % mrl.substr(0, 24))
		return
	print("subtitles: the imported resource carries %d characters of data: MRL" % mrl.length())

	# The runtime form keeps whatever MRL it was given.
	var remote: VLCSubtitle = VLCSubtitle.load_from_mrl("https://example.com/sub.srt")
	if remote.get_mrl() != "https://example.com/sub.srt":
		_fail("load_from_mrl did not keep the MRL it was given")
		return

	# The text scale belongs to the player, so it applies before playback -- and a
	# value libvlc would assert on is refused here instead of being handed over.
	player.set_spu_text_scale(2.0)
	if absf(player.get_spu_text_scale() - 2.0) > 0.001:
		_fail("the text scale did not take effect before playback")
		return
	player.set_spu_text_scale(6.0)
	if absf(player.get_spu_text_scale() - 2.0) > 0.001:
		_fail("an out-of-range text scale was applied anyway: %f" % player.get_spu_text_scale())
		return
	player.set_spu_text_scale(1.0)

	player.media = VLCMedia.load_from_file(ProjectSettings.globalize_path("res://test.mp4"))
	player.play()
	if not await _wait_for_state(VLCMediaPlayer.STATE_PLAYING):
		_fail("playback never started")
		return
	if _text_tracks() != 0:
		_fail("the media already has a text track, so this test would prove nothing")
		return

	var without := await _frame_at(CUE_MS)
	if without == null:
		_fail("the software output produced no frame")
		return

	# Attaching while playing is the half that has an input to attach to.
	if player.add_subtitle(imported, true) != 0:
		_fail("adding a subtitle to a playing player was refused")
		return
	if not await _wait_for_text_track():
		_fail("the subtitle never became a track")
		return

	var with := await _frame_at(CUE_MS)
	if with == null:
		_fail("no frame arrived after the subtitle was attached")
		return

	var before := _bottom_strip(without)
	var after := _bottom_strip(with)
	print("subtitles: the bottom strip is %d bytes, identical=%s" % [before.size(), before == after])
	if before == after:
		_fail("the frames are identical with and without the subtitle, so nothing was drawn")
		return

	# The delay is the input's: it reads back while playing and is gone after a stop.
	if player.set_spu_delay_us(250000) != 0 or player.get_spu_delay_us() != 250000:
		_fail("the subtitle delay did not read back while playing")
		return

	print("subtitles OK: imported MRL, mid-playback attach, a changed frame, and the two knobs")
	quit(0)


func _text_tracks() -> int:
	var tracks: Variant = player.get_tracklist(VLCTrack.TYPE_TEXT, false)
	if tracks == null:
		return 0
	return tracks.get_tracks().size()


func _wait_for_text_track() -> bool:
	var deadline := Time.get_ticks_msec() + FRAME_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		if _text_tracks() > 0:
			return true
		await process_frame
	return false


func _wait_for_state(wanted: int) -> bool:
	var deadline := Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		if player.get_state() == wanted:
			return true
		await process_frame
	return false


# Seeks while paused and returns the frame that follows, so that two calls with the
# same argument compare the same video content.
func _frame_at(time_ms: int) -> Image:
	player.set_pause(true)
	player.set_time(time_ms, false)
	var deadline := Time.get_ticks_msec() + FRAME_TIMEOUT_MS
	var frame: Image = null
	while Time.get_ticks_msec() < deadline:
		var candidate: Image = player.get_frame()
		if candidate != null:
			frame = candidate
			# A frame produced after the seek, rather than the one before it.
			if player.get_time() >= time_ms:
				break
		await process_frame
	return frame


func _bottom_strip(image: Image) -> PackedByteArray:
	var height := image.get_height()
	var strip: int = mini(200, height / 4)
	return image.get_region(Rect2i(0, height - strip, image.get_width(), strip)).get_data()


func _fail(what: String) -> void:
	push_error("subtitles FAIL: %s" % what)
	quit(1)
