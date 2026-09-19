extends SceneTree

# A teardown with players still playing.
#
# **This test does not reproduce the abort it was written for**, and says so rather than
# looking like coverage it is not. The window is this: Godot destroys a node's children
# before the extension instance behind the node, so between those two moments libvlc's
# audio thread still calls this extension's callbacks holding a `Gd` whose
# `AudioStreamPlayer` has been freed -- a use-after-free inside libvlc's own thread, which
# godot-rust reports as an abort (`AudioStreamPlayer::upcast_ref: access to instance ...
# after it has been freed`). The callbacks now check `is_instance_valid` first.
#
# Measured: freeing players by hand, or leaving them playing and calling `quit`, both
# **pass** on the unfixed build -- the window there closes inside one frame. What opens it
# wide is the engine's own shutdown, where the tree comes down on `--quit-after` and the
# extension instances wait for ObjectDB cleanup: the demo scene aborted in three runs out
# of four before the fix and in seven out of seven after it. So the demo is the
# reproduction, and this file is a smoke test: players left playing as the tree is torn
# down must not take the process with them for any other reason.
#
#   godot --headless --path demo --script res://tests/player_teardown.gd

const PLAYERS := 3
const PLAY_TIMEOUT_MS := 20000
const PLAYING_FOR_MS := 700


func _init() -> void:
	var players: Array[VLCMediaPlayer] = []
	for round in PLAYERS:
		var media := VLCMedia.load_from_file("res://test.mp4")
		if media == null:
			_fail("player %d could not load the demo media" % round)
			return
		var player := VLCMediaPlayer.new()
		player.media = media
		root.add_child(player)
		player.play()
		players.append(player)

	# Wait until they are all playing, and then let them play on: whatever the teardown
	# does, it does it with audio flowing.
	var deadline := Time.get_ticks_msec() + PLAY_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		var playing := 0
		for player in players:
			if player.get_state() == VLCMediaPlayer.STATE_PLAYING:
				playing += 1
		if playing == PLAYERS:
			break
		await process_frame
	if Time.get_ticks_msec() >= deadline:
		_fail("not every player reached STATE_PLAYING within %dms" % PLAY_TIMEOUT_MS)
		return
	deadline = Time.get_ticks_msec() + PLAYING_FOR_MS
	while Time.get_ticks_msec() < deadline:
		await process_frame

	print(
		"player_teardown OK: %d players were still playing as the tree came down and the process survived it"
		% PLAYERS
	)
	quit(0)


func _fail(what: String) -> void:
	push_error("player_teardown FAIL: %s" % what)
	quit(1)