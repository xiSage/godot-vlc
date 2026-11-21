extends SceneTree

# §3.4 through the engine: what a stall does to the audio buffer.
#
# The ring buffer holds five seconds of stereo audio, and libvlc's audio callbacks
# fill it from their own thread. When nothing takes audio out of it -- a paused tree
# pauses the child that plays it -- the buffer fills, and from then on every block
# libvlc produces is dropped. Two things have to hold in that state:
#
#   - a player the tree pauses hands nothing to the buffer, so it never fills;
#   - a buffer that does fill is reported once for the stall, not once per block.
#
# The second half is what a debugger showed: one "audio buffer full" line every few
# tens of milliseconds for as long as the stall lasted. This counts those lines
# through a `Logger`, so the assertion is on what the debugger would have shown.
#
#   godot --headless --path demo --script res://tests/audio_overflow.gd

const PLAY_MS := 2000
const PAUSE_MS := 7000
const SETTLE_MS := 2000
const MESSAGE := "audio buffer full"

var player: VLCMediaPlayer
var errors := ErrorLog.new()


class ErrorLog:
	extends Logger

	var lines: Array[String] = []

	func _log_error(
		function: String,
		file: String,
		line: int,
		code: String,
		rationale: String,
		_editor_notify: bool,
		_error_type: int,
		_script_backtraces: Array[ScriptBacktrace]
	) -> void:
		lines.push_back("%s %s" % [code, rationale])


func _init() -> void:
	OS.add_logger(errors)

	player = VLCMediaPlayer.new()
	player.media = VLCMedia.load_from_file("res://test.mp4")
	root.add_child(player)
	await process_frame
	player.play()
	if not await _wait_for_state(VLCMediaPlayer.STATE_PLAYING):
		_fail("playback never started")
		return
	await _wait_duration(PLAY_MS)

	# A paused player: the buffer must not fill at all, so nothing is dropped and
	# nothing is logged.
	paused = true
	await _wait_duration(PAUSE_MS)
	paused = false
	await _wait_duration(SETTLE_MS)
	if not _overflow_lines().is_empty():
		_fail("a paused player logged %d overflow line(s): %s" % [
			_overflow_lines().size(), _overflow_lines()[0]
		])
		return
	print("audio_overflow: %d ms paused, no overflow logged" % PAUSE_MS)

	# A player that keeps its frame while the audio node alone is stopped by the
	# pause: the buffer fills for real, and that has to cost one line.
	player.process_mode = Node.PROCESS_MODE_ALWAYS
	for child in player.get_children(true):
		if child is AudioStreamPlayer:
			child.process_mode = Node.PROCESS_MODE_PAUSABLE
	var before := _overflow_lines().size()
	paused = true
	await _wait_duration(PAUSE_MS)
	paused = false
	await _wait_duration(SETTLE_MS)
	var found := _overflow_lines().size() - before
	if found != 1:
		_fail("%d ms of a drained-nothing buffer logged %d line(s), expected 1" % [
			PAUSE_MS, found
		])
		return
	print("audio_overflow: %d ms of stalled buffer logged once" % PAUSE_MS)

	# The stream is still the one that was playing: the stall dropped audio, it did
	# not end playback.
	if player.get_state() != VLCMediaPlayer.STATE_PLAYING:
		_fail("the player is in state %d after the stall" % player.get_state())
		return
	if not await _wait_for_time(PLAY_MS + PAUSE_MS):
		_fail("the clock did not move past the stall: %d ms" % player.get_time())
		return

	print("audio_overflow OK: a paused player logs nothing, a stalled buffer logs once")
	quit(0)


func _overflow_lines() -> Array[String]:
	var found: Array[String] = []
	for line in errors.lines:
		if line.contains(MESSAGE):
			found.push_back(line)
	return found


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


func _wait_for_time(ms: int) -> bool:
	var deadline := Time.get_ticks_msec() + 20000
	while Time.get_ticks_msec() < deadline:
		if player.get_time() >= ms:
			return true
		await process_frame
	return false


func _fail(what: String) -> void:
	push_error("audio_overflow FAIL: %s" % what)
	quit(1)
