extends SceneTree

# Under --rendering-driver d3d12, the LUID Godot reports for its D3D12
# device and the LUID DXGI reports for the matching adapter must be equal
# and nonzero — that's how the GPU backend picks the right adapter for the
# D3D11 device it hands to libvlc.

func _init() -> void:
	var player := VLCMediaPlayer.new()
	var luids: Dictionary = player._debug_get_adapter_luids()
	player.queue_free()

	if luids.has("error"):
		push_error("adapter_match: probe returned error: %s" % luids["error"])
		quit(1)
		return

	if not luids.has("godot_luid") or not luids.has("d3d11_luid"):
		push_error("adapter_match: missing keys in %s" % str(luids))
		quit(1)
		return

	var godot_luid: int = luids["godot_luid"]
	var d3d11_luid: int = luids["d3d11_luid"]

	if godot_luid == 0 or d3d11_luid == 0:
		push_error("adapter_match: zero LUID (godot=%d d3d11=%d)" % [godot_luid, d3d11_luid])
		quit(1)
		return

	if godot_luid != d3d11_luid:
		push_error("adapter_match: LUID mismatch (godot=%d d3d11=%d)" % [godot_luid, d3d11_luid])
		quit(1)
		return

	print("adapter_match OK: godot_luid=0x%x d3d11_luid=0x%x" % [godot_luid, d3d11_luid])
	quit(0)
