extends SceneTree

# Per-media options are read once, when the media is handed to a player -- not when
# playback starts -- and the binding exposes that as VLCMedia.add_option. This drives
# it through the engine, which the Rust acceptance test cannot: there the calls are
# made from Rust, so a method that never reached GDScript would still pass.
#
# The clip is described by two options rather than one, so that the test does not
# have to play the whole demo media to see them: ":start-time=1.0" and
# ":stop-time=3.0" make a two-second clip out of the middle. Both are read from the
# length the player reports and from how long playback lasts, so the test is bounded
# by the clip rather than by the media.
#
#   godot --headless --path demo --script res://tests/media_options.gd

const PROBE_MS := 1000
const STOP_MS := 3000
const CLIP_MS := STOP_MS - PROBE_MS

const STARTUP_TIMEOUT_MS := 20000
const STOP_TIMEOUT_MS := 20000

# The demo media has to be longer than the clip asked for, with room to spare: the
# whole test rests on the clip being visibly shorter than the media.
const MEDIA_MIN_MS := PROBE_MS + CLIP_MS + 2000

var player: VLCMediaPlayer


func _init() -> void:
	# The two flag constants are libvlc's own values, and add_option is that pair
	# rather than 0.
	if VLCMedia.OPTION_TRUSTED != 2 or VLCMedia.OPTION_UNIQUE != 256:
		_fail("the option constants are %d and %d, not 2 and 256"
			% [VLCMedia.OPTION_TRUSTED, VLCMedia.OPTION_UNIQUE])
		return

	player = VLCMediaPlayer.new()
	root.add_child(player)
	await process_frame

	var path := ProjectSettings.globalize_path("res://test.mp4")

	# A playback with no option, which is where the media's own length comes from.
	player.media = VLCMedia.load_from_file(path)
	player.play()
	if not await _wait_for_playing():
		_fail("the playback without options never started")
		return
	var media_ms := player.get_length()
	if media_ms < MEDIA_MIN_MS:
		_fail("the demo media reports %d ms; this test needs more than %d ms to cut a clip out of"
			% [media_ms, MEDIA_MIN_MS])
		return
	player.stop_async()
	if not await _wait_for_stopped():
		_fail("the playback without options never stopped")
		return

	# The documented order: the options go onto the media, and only then is the media
	# handed to the player.
	var media := VLCMedia.load_from_file(path)
	media.add_option(":start-time=%.3f" % (PROBE_MS / 1000.0))
	media.add_option(":stop-time=%.3f" % (STOP_MS / 1000.0))
	# add_option_flag is the same call with the flags spelled out, and it has to be
	# callable from GDScript too. The same string with UNIQUE is what that flag means:
	# it is already on the media, so this changes nothing.
	media.add_option_flag(":stop-time=%.3f" % (STOP_MS / 1000.0),
		VLCMedia.OPTION_TRUSTED | VLCMedia.OPTION_UNIQUE)
	player.media = media
	player.play()
	if not await _wait_for_playing():
		_fail("the playback with the options never started")
		return

	var clip_ms := player.get_length()
	var began := Time.get_ticks_msec()
	if not await _wait_for_stopped():
		_fail("the clip never stopped; it was asked to end at %d ms" % STOP_MS)
		return
	var played_ms := Time.get_ticks_msec() - began

	print("media_options: media %d ms; with :start-time/%d and :stop-time/%d the player reports %d ms and played for %d ms"
		% [media_ms, PROBE_MS, STOP_MS, clip_ms, played_ms])

	# The length is the clip's, not the media's: that is the option having been read.
	if clip_ms > CLIP_MS + 500:
		_fail("the length is %d ms; :start-time=%d and :stop-time=%d make a %d ms clip"
			% [clip_ms, PROBE_MS, STOP_MS, CLIP_MS])
		return

	# And it is not only the bookkeeping: a playback that read no options would still
	# have to decode its way to the end of the media.
	if played_ms > MEDIA_MIN_MS:
		_fail("playback ran for %d ms against a media of at least %d ms: it played past the stop time"
			% [played_ms, MEDIA_MIN_MS])
		return

	print("media_options OK")
	quit(0)


func _wait_for_playing() -> bool:
	var deadline := Time.get_ticks_msec() + STARTUP_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		if player.get_state() == VLCMediaPlayer.STATE_PLAYING:
			return true
		await process_frame
	return false


func _wait_for_stopped() -> bool:
	var deadline := Time.get_ticks_msec() + STOP_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		if player.get_state() == VLCMediaPlayer.STATE_STOPPED:
			return true
		await process_frame
	return false


func _fail(what: String) -> void:
	push_error("media_options FAIL: %s" % what)
	quit(1)
