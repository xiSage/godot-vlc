extends SceneTree

# §3.4 through the engine: the audio delay and the relative jump.
#
# The Rust acceptance covers the semantics -- where the value lives, what it answers
# when there is nothing to answer with, and the edges of a jump. What only this can
# show is the pair on a player that is actually producing sound: this project's
# instance has an audio output (mmdevice on Windows, pulse or alsa elsewhere), while
# the acceptance harness builds its own with `--no-audio`.
#
#   godot --headless --path demo --script res://tests/audio_delay.gd

const WAIT_TIMEOUT_MS := 20000
const JUMP_MS := 3000
const BACK_MS := 600000

var player: VLCMediaPlayer


func _init() -> void:
	player = VLCMediaPlayer.new()
	root.add_child(player)
	await process_frame

	# Before an input exists the call is accepted and dropped: 0 either way, so the
	# only evidence is that nothing was kept.
	if player.get_audio_delay_us() != 0:
		_fail("a fresh player reports a delay of %d" % player.get_audio_delay_us())
		return
	if player.set_audio_delay_us(250000) != 0:
		_fail("setting the delay was refused for a player with no input")
		return
	if player.get_audio_delay_us() != 0:
		_fail("a delay set before there was an input was kept somewhere")
		return

	player.media = VLCMedia.load_from_file("res://test.mp4")
	player.play()
	if not await _wait_for_state(VLCMediaPlayer.STATE_PLAYING):
		_fail("playback never started")
		return

	# While playing it reads back, with an audio output in the picture.
	player.set_audio_delay_us(-250000)
	if player.get_audio_delay_us() != -250000:
		_fail("the delay did not read back while playing: %d" % player.get_audio_delay_us())
		return
	print("audio_delay: -250000 us reads back while playing")

	# jump_time is relative to libvlc's own clock, so the caller does not read it first.
	var before := player.get_time()
	if player.jump_time(JUMP_MS) != 0:
		_fail("the forward jump was refused")
		return
	if not await _wait_for_time(before + JUMP_MS):
		_fail("the clock never reached %d ms after jumping forward from %d ms" % [
			before + JUMP_MS, before
		])
		return
	print("audio_delay: %d ms plus %d landed at %d ms" % [before, JUMP_MS, player.get_time()])

	# Backwards past the start: clamped to the start of the input, not refused.
	if player.jump_time(-BACK_MS) != 0:
		_fail("the backward jump was refused")
		return
	if not await _wait_for_time_at_most(1000):
		_fail("a jump past the start did not land at the start; the clock is at %d ms" % player.get_time())
		return
	print("audio_delay: %d ms back landed at %d ms" % [BACK_MS, player.get_time()])

	# The delay goes away with the input.
	player.stop_async()
	if not await _wait_for_state(VLCMediaPlayer.STATE_STOPPED):
		_fail("playback never stopped")
		return
	if player.get_audio_delay_us() != 0:
		_fail("the delay outlived the input it was set on: %d" % player.get_audio_delay_us())
		return

	print("audio_delay OK: the delay is the input's, and jump_time is relative")
	quit(0)


func _wait_for_state(wanted: int) -> bool:
	var deadline := Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		if player.get_state() == wanted:
			return true
		await process_frame
	return false


func _wait_for_time(ms: int) -> bool:
	var deadline := Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		if player.get_time() >= ms:
			return true
		await process_frame
	return false


func _wait_for_time_at_most(ms: int) -> bool:
	var deadline := Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		if player.get_time() <= ms:
			return true
		await process_frame
	return false


func _fail(what: String) -> void:
	push_error("audio_delay FAIL: %s" % what)
	quit(1)
