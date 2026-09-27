extends SceneTree

# The arguments the LibVLC instance was started with, and the setting behind them.
#
# The instance is the engine singleton VLCInstance: the extension registers it while the
# scene layer is initialised, before this test runs, and reads vlc/log_level and
# vlc/arguments exactly once, there. That is why a test cannot start an instance with
# arguments of its own -- and why what is left to check is the contract that comes out of
# it: the instance reports the list it was actually started with, and editing the setting
# afterwards does not reach it. The rule that turns the setting into that list, including
# the one argument Android adds, is covered by unit tests in src/vlc_instance.rs, where
# both platforms can be asked.
#
#   godot --headless --path demo --script res://tests/instance_arguments.gd


func _init() -> void:
	if not VLCInstance.has_instance():
		_fail("the extension has no LibVLC instance")
		return

	var started_with: Variant = VLCInstance.get_arguments()
	if not (started_with is PackedStringArray):
		_fail("get_arguments() answered with a %s" % type_string(typeof(started_with)))
		return
	var live := started_with as PackedStringArray

	# The demo configures no arguments of its own, so this instance was started with
	# none -- and the setting it came from is the empty list the extension registers,
	# typed as an array of strings.
	var configured: Variant = ProjectSettings.get_setting("vlc/arguments")
	if not (configured is Array):
		_fail("vlc/arguments is a %s, not an Array" % type_string(typeof(configured)))
		return
	if (configured as Array).get_typed_builtin() != TYPE_STRING:
		_fail("vlc/arguments holds %s rather than Strings" % type_string((configured as Array).get_typed_builtin()))
		return
	if not (configured as Array).is_empty():
		_fail("the demo configures no arguments, and vlc/arguments holds %s" % configured)
		return
	if not live.is_empty():
		_fail("the instance was started with %s, and the setting holds none" % live)
		return

	# The setting is read once, while the extension loads, so writing one now must not
	# reach the instance that is already running. This is the restart contract as a
	# script can see it; the other half -- that the next run does read it -- is what the
	# startup line and the unit tests cover.
	var replacement: Array[String] = ["--no-video"]
	ProjectSettings.set_setting("vlc/arguments", replacement)
	var after := VLCInstance.get_arguments()
	ProjectSettings.set_setting("vlc/arguments", configured)
	if after != live:
		_fail("editing vlc/arguments reached the running instance: %s" % after)
		return

	# Nothing above replaced the instance either: it is still the one that came up.
	if not VLCInstance.has_instance():
		_fail("the instance went away while vlc/arguments was edited")
		return

	print("instance_arguments OK: the instance was started with %s, and vlc/arguments is %s" % [
		live, configured
	])
	print("instance_arguments: editing vlc/arguments left the running instance at %s" % after)
	quit(0)


func _fail(what: String) -> void:
	push_error("instance_arguments FAIL: %s" % what)
	quit(1)
