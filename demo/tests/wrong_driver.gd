extends SceneTree

# Under a non-D3D12 rendering driver (e.g. --rendering-driver vulkan), the
# adapter-LUID probe must return a structured error and not crash.

func _init() -> void:
	var player := VLCMediaPlayer.new()
	var luids: Dictionary = player._debug_get_adapter_luids()
	player.queue_free()

	if not luids.has("error"):
		push_error("wrong_driver: expected 'error' key, got %s" % str(luids))
		quit(1)
		return

	var msg: String = luids["error"]
	if not msg.contains("d3d12"):
		push_error("wrong_driver: error string missing 'd3d12' marker: %s" % msg)
		quit(1)
		return

	print("wrong_driver OK: %s" % msg)
	quit(0)
