extends SceneTree

# libvlc's own log, in GDScript.
#
# The console line is written where the message arrives -- on libvlc's thread -- and
# `log_message` is the script-visible half: a player's frame drains what the logging
# callback parked, which is also why this test needs a player at all. The two static
# calls are here as well, because they are the answer to "which VLC is this" that a
# bug report needs, and they work without an instance.
#
# The media is the demo's own, opened through the binding's in-memory callbacks: the
# mp4 demuxer logs warnings about the container it is reading, which is a rung above
# debug and therefore proves the level mapping rather than just the plumbing.
#
#   godot --headless --path demo --script res://tests/log_message.gd

const REPORT_TIMEOUT_MS := 20000

var lines: Array = []


func _init() -> void:
	if not VLCInstance.get_version().contains("4.0"):
		_fail("the runtime reports version %s" % VLCInstance.get_version())
		return
	if VLCInstance.get_changeset().is_empty():
		_fail("the runtime reports no changeset")
		return
	print("log_message: libvlc %s (%s)" % [
		VLCInstance.get_version(), VLCInstance.get_changeset()
	])

	# Everything, so that the warnings below are not filtered out before they reach
	# the signal; the setter takes effect without re-creating anything.
	VLCInstance.set_log_level(VLCInstance.LOG_LEVEL_DEBUG)
	if VLCInstance.get_log_level() != VLCInstance.LOG_LEVEL_DEBUG:
		_fail("set_log_level did not take effect")
		return

	VLCInstance.log_message.connect(
		func(level: int, module: String, message: String) -> void:
			lines.append({"level": level, "module": module, "message": message})
	)

	# The drain runs from a player's frame, so there has to be one: the signal is the
	# half of the log that needs a main-thread object, and an `Object` singleton has
	# no frame of its own.
	var player := VLCMediaPlayer.new()
	root.add_child(player)
	await process_frame

	player.media = VLCMedia.load_from_file("res://test.mp4")
	player.play()

	var deadline := Time.get_ticks_msec() + REPORT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline and not _named_line():
		await process_frame

	if lines.is_empty():
		_fail("no log_message arrived within %dms while a media played" % REPORT_TIMEOUT_MS)
		return
	print("log_message: %d lines, the first is %s" % [lines.size(), str(lines[0])])

	# Every level is one of this extension's rungs. libvlc's own enum numbers its
	# levels differently (its NOTICE is 2 and its ERROR is 4), so a line that came
	# through unmapped would show up here. Which rung a given runtime reports a given
	# failure on depends on libvlc, so this checks the range rather than a value --
	# `vlc_instance.rs` has the unit test for the translation itself.
	for line in lines:
		if (
			line["level"] < VLCInstance.LOG_LEVEL_DEBUG
			or line["level"] > VLCInstance.LOG_LEVEL_ERROR
		):
			_fail("a line reported level %d, which is not a rung: %s" % [line["level"], str(line)])
			return

	if not _named_line():
		_fail("no line named the module that logged it: %s" % str(lines))
		return

	print("log_message OK")
	quit(0)


# Whether a line has arrived that names the module which logged it.
func _named_line() -> bool:
	for line in lines:
		if not line["module"].is_empty():
			return true
	return false


func _fail(what: String) -> void:
	push_error("log_message FAIL: %s" % what)
	quit(1)
