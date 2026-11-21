extends SceneTree

# What a stalled main loop does to the media.
#
# The editor's Pause button stops the main loop where it is: the debugger holds it
# until the game is continued. No node notification is sent for that, and no method of
# this node runs while it lasts -- but libvlc keeps decoding on its own threads, so a
# media that is not frozen comes back a stall's length further along. Nothing on the
# main thread can act then, which is why the player watches its own heartbeat from a
# thread of its own: a second without a processed frame pauses a playing media, and the
# first frame afterwards resumes it, and only a media that thread paused.
#
# OS.delay_msec stands in for the break here: it blocks the main loop the same way,
# without needing a debugger attached to the test. A stall shorter than the threshold
# must not pause anything, and a pause the caller made must survive a stall.
#
#   godot --headless --path demo --script res://tests/stall_freeze.gd

const PLAY_MS := 2000
const STALL_MS := 2500
const SHORT_STALL_MS := 100
# The media may play on for as long as the watch needs to notice the stall -- a
# quarter second -- and for the frames checked after it, but it must not have played
# the whole stall: 900 ms is the honest figure, 3100 ms is a media nothing froze.
const ALLOWED_MS := 1500

var player: VLCMediaPlayer


func _init() -> void:
	player = VLCMediaPlayer.new()
	player.media = VLCMedia.load_from_file("res://test.mp4")
	player.force_hardware = false
	root.add_child(player)
	await process_frame
	player.play()
	if not await _wait_for_state(VLCMediaPlayer.STATE_PLAYING):
		_fail("playback never started")
		return
	await _wait_duration(PLAY_MS)

	# The stall. Whatever the media reached before it is where it has to be after it:
	# the picture and the sound wait for the game rather than running on without it.
	var before := player.get_time()
	OS.delay_msec(STALL_MS)
	await _wait_duration(600)
	if player.get_state() != VLCMediaPlayer.STATE_PLAYING:
		_fail("the media is in state %d after a stall, expected PLAYING" % player.get_state())
		return
	var after := player.get_time()
	if after > before + ALLOWED_MS:
		_fail("the media kept playing through a %d ms stall: %d -> %d ms" % [STALL_MS, before, after])
		return

	# And it keeps playing through the frames that follow, rather than staying frozen:
	# the freeze is undone on the first frame, not merely stopped.
	await _wait_duration(900)
	var resumed := player.get_time()
	if resumed < after + 300:
		_fail("the media did not resume after the stall: %d -> %d ms" % [after, resumed])
		return

	# A stall shorter than the threshold is a frame that took its time, not a break:
	# the media plays straight through it.
	var short_before := player.get_time()
	OS.delay_msec(SHORT_STALL_MS)
	await _wait_duration(200)
	var short_after := player.get_time()
	if short_after < short_before + 180:
		_fail("a %d ms stall paused the media: %d -> %d ms" % [SHORT_STALL_MS, short_before, short_after])
		return

	# A pause the caller made is not the watch's to undo: a stalled game left a paused
	# media paused, and coming back must not start playing something it stopped.
	player.set_pause(true)
	if not await _wait_for_state(VLCMediaPlayer.STATE_PAUSED):
		_fail("the caller's pause never took effect")
		return
	OS.delay_msec(STALL_MS)
	await _wait_duration(600)
	if player.get_state() != VLCMediaPlayer.STATE_PAUSED:
		_fail("the stall resumed a media the caller had paused (state %d)" % player.get_state())
		return

	print("stall_freeze: %d ms stall played to %d ms, resumed at %d ms; %d ms stall played through to %d ms; caller's pause kept" % [STALL_MS, after - before, resumed - before, SHORT_STALL_MS, short_after - short_before])
	print("stall_freeze OK: a stalled game freezes the media and resumes it where it stopped")
	quit(0)


func _wait_duration(ms: int) -> void:
	var deadline := Time.get_ticks_msec() + ms
	while Time.get_ticks_msec() < deadline:
		await process_frame


func _wait_for_state(wanted: int) -> bool:
	var deadline := Time.get_ticks_msec() + 20000
	while Time.get_ticks_msec() < deadline:
		if player.get_state() == wanted:
			return true
		await process_frame
	return false


func _fail(what: String) -> void:
	push_error("stall_freeze FAIL: %s" % what)
	quit(1)
