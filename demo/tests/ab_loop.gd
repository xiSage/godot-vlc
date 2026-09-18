extends SceneTree

# The A to B loop has no event of any kind: from outside it shows up as time
# moving backwards, and it disappears with the input on every stop. This drives
# both through the engine -- the calls, the getters that have to survive libvlc's
# uninitialised out-parameters, and a wrap that really happens -- which the Rust
# acceptance test cannot: that one drives LibVLC directly.

const LOOP_B_MS := 400
const STARTUP_TIMEOUT_MS := 20000
const OBSERVE_MS := 3000

var player: VLCMediaPlayer
var drops := 0
var previous_ms := 0


func _init() -> void:
	player = VLCMediaPlayer.new()
	root.add_child(player)
	# The input arrives when the node becomes ready, and a loop set before that
	# has nothing to belong to.
	await process_frame

	player.media = VLCMedia.load_from_file(ProjectSettings.globalize_path("res://test.mp4"))
	player.play()
	if not await _wait_for_playing():
		_fail("playback never started")
		return

	if player.set_ab_loop(0, LOOP_B_MS) != 0:
		_fail("set_ab_loop(0, %d) was refused" % LOOP_B_MS)
		return

	if player.get_ab_loop_status() != VLCMediaPlayer.ABLOOP_B:
		_fail("the status is %d after setting a loop, expected ABLOOP_B" % player.get_ab_loop_status())
		return

	if player.get_ab_loop_a_time() != 0 or player.get_ab_loop_b_time() != LOOP_B_MS:
		_fail("the loop reads back as %d-%d, expected 0-%d" % [player.get_ab_loop_a_time(), player.get_ab_loop_b_time(), LOOP_B_MS])
		return

	if not await _observe():
		_fail("no wrap within %dms with a %dms loop" % [OBSERVE_MS, LOOP_B_MS])
		return

	# Clearing it works while it is playing -- and only then, which is why the
	# Rust test pins the other half and this one clears it here.
	if player.reset_ab_loop() != 0:
		_fail("reset_ab_loop() was refused while playing")
		return

	if player.get_ab_loop_status() != VLCMediaPlayer.ABLOOP_NONE:
		_fail("the loop is still reported after reset_ab_loop()")
		return

	# The position entry point is the other half of the same setting, and calling
	# it from GDScript is the one thing the Rust acceptance test cannot check: that
	# one drives LibVLC directly.
	if player.set_ab_loop_by_position(0.0, 0.4) != 0:
		_fail("set_ab_loop_by_position(0.0, 0.4) was refused")
		return

	if player.get_ab_loop_a_time() != -1 or player.get_ab_loop_b_time() != -1:
		_fail("a loop set by position reported times")
		return

	if player.get_ab_loop_a_position() != 0.0 or player.get_ab_loop_b_position() != 0.4:
		_fail("a loop set by position reported the wrong fractions")
		return

	player.reset_ab_loop()

	print("ab_loop OK: the loop wrapped %d times, cleared while playing, and takes positions" % drops)
	quit(0)


func _wait_for_playing() -> bool:
	var deadline := Time.get_ticks_msec() + STARTUP_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		if player.get_state() == VLCMediaPlayer.STATE_PLAYING:
			return true
		await process_frame
	return false


# Time only moves backwards when the loop wraps: the media is longer than the loop
# and nothing seeks in this test.
func _observe() -> bool:
	previous_ms = player.get_time()
	var deadline := Time.get_ticks_msec() + OBSERVE_MS
	while Time.get_ticks_msec() < deadline:
		await process_frame
		var now := player.get_time()
		if now < previous_ms - 100:
			drops += 1
		previous_ms = now
	return drops > 0


func _fail(what: String) -> void:
	push_error("ab_loop FAIL: %s" % what)
	quit(1)
