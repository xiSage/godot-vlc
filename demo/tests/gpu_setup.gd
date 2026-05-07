extends SceneTree

# With force_hardware=true under --rendering-driver d3d12, the GPU backend
# should initialize and libvlc's update_output_cb should fire once libvlc
# starts decoding — evidenced by an event in the mailbox carrying the
# clip's resolution.

func _init() -> void:
	var player := VLCMediaPlayer.new()
	player.force_hardware = true
	player.media = VLCMedia.load_from_file(ProjectSettings.globalize_path("res://test.mp4"))
	player.autoplay = true
	root.add_child(player)

	for _i in 60:
		await process_frame

	if not player._debug_gpu_active():
		push_error("gpu_setup FAIL: _debug_gpu_active() returned false (GPU init failed; see godot_error log above)")
		quit(1)
		return

	var size: Vector2i = player._debug_pop_gpu_event()
	if size.x <= 0 or size.y <= 0:
		push_error("gpu_setup FAIL: no event in mailbox after 60 frames (update_output_cb never fired?)")
		quit(1)
		return

	print("gpu_setup OK: GPU backend active, update_output_cb fired with %dx%d" % [size.x, size.y])
	# stop_async + drain a few frames so libvlc's render thread isn't mid-flight
	# during SceneTree teardown.
	player.stop_async()
	for _i in 30:
		await process_frame
	quit(0)
