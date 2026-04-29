extends SceneTree

# With force_hardware=true under --rendering-driver d3d12, the per-frame
# importer must drain the SPSC mailbox, allocate a Godot RD destination,
# and run private_queue.copy_and_sync() each frame. Asserts frames_copied
# is positive after libvlc has had time to start signalling its fence.

func _init() -> void:
	var player := VLCMediaPlayer.new()
	player.force_hardware = true
	player.media = VLCMedia.load_from_file(ProjectSettings.globalize_path("res://test.mp4"))
	player.autoplay = true
	root.add_child(player)

	# Setup → register → libvlc decode start → update_output_cb →
	# first swap_cb (= first fence signal) → first copy_and_sync.
	# 120 frames at 60fps = ~2s, generous for libvlc to spin up.
	for _i in 120:
		await process_frame

	if not player._debug_gpu_active():
		push_error("gpu_render FAIL: GPU init never succeeded")
		quit(1)
		return

	var copied: int = player._debug_frames_copied()
	if copied < 1:
		push_error("gpu_render FAIL: frames_copied=%d after 120 frames (importer never ran a copy)" % copied)
		quit(1)
		return

	var avg: Color = player._debug_dst_pixel_avg()
	var cb: Vector3i = player._debug_callback_counts()
	print("gpu_render OK: frames_copied=%d, dst avg RGBA=(%.3f, %.3f, %.3f, %.3f), update_output=%d swap=%d make_current=%d" % [copied, avg.r, avg.g, avg.b, avg.a, cb.x, cb.y, cb.z])
	player.stop_async()
	for _i in 30:
		await process_frame
	quit(0)
