extends SceneTree

# What a paused tree does to the picture.
#
# The engine pauses nodes, not libvlc: a media left running behind a paused node keeps
# decoding, and the frames its video output produces are handed to the node one per
# process frame. Before this test existed those frames queued up in the channel while
# the node was paused and came out after the resume at the engine's frame rate -- three
# seconds of pause replayed in about half the time, which reads as the video jumping
# forward -- and the media itself came back a pause's length further along. What has to
# hold instead:
#
#   - the media is paused with the tree, so nothing is produced while nothing shows it;
#   - a paused player takes no frames, and none queue up for it either;
#   - the picture and the clock come back where they stopped, not a pause ahead;
#   - a player that keeps its frame keeps its picture too.
#
#   godot --headless --path demo --script res://tests/video_pause.gd

const PLAY_MS := 2000
const PAUSE_MS := 3000
const WINDOW_MS := 1500
const ALWAYS_PAUSE_MS := 1500

var player: VLCMediaPlayer
var frames: Array[int] = []


func _init() -> void:
	player = VLCMediaPlayer.new()
	player.media = VLCMedia.load_from_file("res://test.mp4")
	# The software output is what hands frames to the node; the GPU one is a separate
	# path, and this test is about the channel the software path fills.
	player.force_hardware = false
	root.add_child(player)
	await process_frame
	player.video_frame.connect(_on_frame)
	player.play()
	if not await _wait_for_state(VLCMediaPlayer.STATE_PLAYING):
		_fail("playback never started")
		return
	await _wait_duration(PLAY_MS)
	if frames.size() < 10:
		_fail("the software output produced %d frames in %d ms" % [frames.size(), PLAY_MS])
		return
	var rate := frames.size() / float(PLAY_MS)

	# A paused tree freezes the media: the node takes no frames, and libvlc stops
	# producing them, so there is nothing to queue or to drop.
	frames.clear()
	var before := player.get_time()
	paused = true
	await _wait_duration(500)
	if player.get_state() != VLCMediaPlayer.STATE_PAUSED:
		_fail("the media is in state %d 500 ms into a paused tree, expected PAUSED" % player.get_state())
		return
	await _wait_duration(PAUSE_MS - 500)
	var during := frames.size()
	paused = false
	if during != 0:
		_fail("a paused player took %d frame(s)" % during)
		return

	# The first frame window after the resume is the one a backlog would show up in:
	# frames replayed at the engine's rate arrive about three times as fast as the
	# media produces them.
	frames.clear()
	var resumed := player.get_time()
	await _wait_duration(WINDOW_MS)
	var burst := frames.size()
	var allowed := int(rate * WINDOW_MS * 1.5) + 3
	if burst > allowed:
		_fail("the picture after the pause ran at %.1f fps for %d ms (%d frames, %d at most): the pause's frames were replayed" % [
			burst * 1000.0 / WINDOW_MS, WINDOW_MS, burst, allowed
		])
		return
	if burst == 0:
		_fail("the picture did not come back after the pause")
		return

	# The media was paused with the tree, so the picture comes back where it stopped
	# rather than a pause's length further along.
	if resumed > before + 800:
		_fail("the media kept playing through the pause: %d -> %d ms" % [before, resumed])
		return
	print("video_pause: %d ms paused, %d frame(s) taken, %.1f fps after resume, clock %d -> %d ms" % [
		PAUSE_MS, during, burst * 1000.0 / WINDOW_MS, before, resumed
	])

	# A player that keeps its frame while the tree is paused keeps its picture too.
	player.process_mode = Node.PROCESS_MODE_ALWAYS
	frames.clear()
	paused = true
	await _wait_duration(ALWAYS_PAUSE_MS)
	paused = false
	if frames.size() < 10:
		_fail("a player set to PROCESS_MODE_ALWAYS took %d frame(s) in %d ms" % [
			frames.size(), ALWAYS_PAUSE_MS
		])
		return
	print("video_pause: %d ms paused with PROCESS_MODE_ALWAYS, %d frames" % [
		ALWAYS_PAUSE_MS, frames.size()
	])

	if player.get_state() != VLCMediaPlayer.STATE_PLAYING:
		_fail("the player is in state %d after the pauses" % player.get_state())
		return
	print("video_pause OK: the media pauses with the tree and resumes where it stopped")
	quit(0)


func _on_frame() -> void:
	frames.push_back(Time.get_ticks_msec())


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
	push_error("video_pause FAIL: %s" % what)
	quit(1)
