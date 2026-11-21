extends SceneTree

# What a suspended tree does to the media.
#
# The editor's Suspend button does not set SceneTree.paused: it calls
# SceneTree::set_suspend, which stops processing and sends NOTIFICATION_SUSPENDED
# (9003) / NOTIFICATION_UNSUSPENDED (9004) to every node -- see set_suspend and
# Node::_propagate_suspend_notification in scene/main/scene_tree.cpp and
# scene/main/node.cpp. Nothing else reports a suspension, so a media that kept
# decoding behind it came back a suspension's length further along. What has to hold
# instead:
#
#   - the media is paused while the tree is suspended, and the clock stands still;
#   - the suspension's end resumes it from where it stopped, without a jump;
#   - a pause the caller made is not undone by either.
#
# The editor's button was checked against the real path -- a game started with
# --remote-debug, a stand-in debugger sending scene:suspend_changed -- so the pair
# below is what that button produces, not a guess.
#
#   godot --headless --path demo --script res://tests/tree_suspend.gd

const PLAY_MS := 2000
const SUSPEND_MS := 2500
const NOTIFICATION_SUSPENDED := 9003
const NOTIFICATION_UNSUSPENDED := 9004

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

	var before := player.get_time()
	# The engine propagates the pair to the whole tree; doing the same is as close as
	# a single process gets to the editor's button.
	root.propagate_notification(NOTIFICATION_SUSPENDED)
	await _wait_duration(600)
	if player.get_state() != VLCMediaPlayer.STATE_PAUSED:
		_fail("the media is in state %d 600 ms into a suspended tree, expected PAUSED" % player.get_state())
		return
	var frozen := player.get_time()
	if frozen > before + 300:
		_fail("the media kept playing into the suspension: %d -> %d ms" % [before, frozen])
		return

	# The clock stands still for the rest of the suspension.
	await _wait_duration(SUSPEND_MS - 600)
	var still := player.get_time()
	if still > frozen + 200:
		_fail("the clock moved during the suspension: %d -> %d ms" % [frozen, still])
		return

	root.propagate_notification(NOTIFICATION_UNSUSPENDED)
	await _wait_duration(800)
	if player.get_state() != VLCMediaPlayer.STATE_PLAYING:
		_fail("the media is in state %d after the suspension ended, expected PLAYING" % player.get_state())
		return
	var resumed := player.get_time()
	if resumed < still:
		_fail("the media went backwards when the suspension ended: %d -> %d ms" % [still, resumed])
		return
	if resumed > still + 1500:
		_fail("the media jumped when the suspension ended: %d -> %d ms" % [still, resumed])
		return

	# A pause the caller made belongs to the caller: suspending and resuming the tree
	# must not start playing something it stopped.
	player.set_pause(true)
	await _wait_for_state(VLCMediaPlayer.STATE_PAUSED)
	root.propagate_notification(NOTIFICATION_SUSPENDED)
	await _wait_duration(500)
	root.propagate_notification(NOTIFICATION_UNSUSPENDED)
	await _wait_duration(500)
	if player.get_state() != VLCMediaPlayer.STATE_PAUSED:
		_fail("the suspension resumed a media the caller had paused (state %d)" % player.get_state())
		return

	print("tree_suspend: clock %d -> %d -> %d ms, caller's pause kept" % [before, still, resumed])
	print("tree_suspend OK: a suspended tree freezes the media and resumes it where it stopped")
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
	push_error("tree_suspend FAIL: %s" % what)
	quit(1)
