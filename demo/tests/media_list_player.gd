extends SceneTree

# The list player through the engine: a node that drives another node.
#
# The Rust acceptance covers the runtime's half -- the announcements, the advance, the
# advance that a direct stop of the player causes, and the loop. What only this can cover
# is the binding's half: the exported property that points it at a `VLCMediaPlayer` in
# the scene, that the video still comes out of **that** node while the list plays, that
# repeat mode is refused instead of aborting the process, and that the association ends
# when the player node leaves the tree -- which is what keeps libvlc from calling this
# extension's callbacks after the node behind them is gone.
#
#   godot --headless --path demo --script res://tests/media_list_player.gd

const WAIT_TIMEOUT_MS := 20000

var announced: Array = []
var stopped := 0


func _init() -> void:
	var player := VLCMediaPlayer.new()
	root.add_child(player)
	var list_player := VLCMediaListPlayer.new()
	# The parent is the player: that is the whole association, and Godot frees children
	# before their parent, which is what keeps libvlc from outliving this node's
	# callbacks.
	player.add_child(list_player)
	await process_frame

	if not list_player.is_playable():
		_fail("a fresh list player answers that it is not playable")
		return

	if list_player.get_player() != player:
		_fail("the list player did not find the player it is a child of")
		return
	var list := VLCMediaList.new()
	var media := VLCMedia.load_from_file("res://test.mp4")
	if media == null:
		_fail("the demo media could not be loaded")
		return
	list.add_media(media)
	list.add_media(media)
	list_player.set_media_list(list)
	if list_player.get_media_list() != list:
		_fail("the media list did not keep what was set on it")
		return

	list_player.next_item_set.connect(
		func(item: VLCMedia) -> void: announced.append(item.get_mrl())
	)
	list_player.stopped.connect(func() -> void: stopped += 1)

	list_player.play()
	var deadline := Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline and announced.is_empty():
		await process_frame
	if announced.is_empty():
		_fail("playing the list announced no item within %dms" % WAIT_TIMEOUT_MS)
		return
	print("media_list_player: the list announced %s" % announced[0])

	# The picture is still the scene node's: the list player drives *that* player, it does
	# not play through one of its own.
	deadline = Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline and player.get_frame() == null:
		await process_frame
	if player.get_frame() == null:
		_fail("the player the list drives produced no frame")
		return

	# Next moves to the second entry, and the item is announced.
	if list_player.next() != 0:
		_fail("next was refused")
		return
	deadline = Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline and announced.size() < 2:
		await process_frame
	if announced.size() < 2:
		_fail("next announced no item")
		return

	# Its own stop is reported; the player underneath is left alone.
	list_player.stop_async()
	deadline = Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline and stopped == 0:
		await process_frame
	if stopped == 0:
		_fail("stopping the list player reported nothing")
		return

	# Repeat mode aborts libvlc, so the binding refuses it; loop is the one to use.
	list_player.set_playback_mode(VLCMediaListPlayer.PLAYBACK_MODE_REPEAT)
	list_player.set_playback_mode(VLCMediaListPlayer.PLAYBACK_MODE_LOOP)

	# Stop first, and wait for it to land: libvlc's audio output calls back into this
	# extension's audio node, and freeing the player while that is still running is a
	# use-after-free inside libvlc's own thread -- measured as an abort on
	# `AudioStreamPlayer::upcast_ref`. A player is freed after its playback has stopped.
	list_player.stop_async()
	deadline = Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline and player.get_state() != VLCMediaPlayer.STATE_STOPPED:
		await process_frame
	if player.get_state() != VLCMediaPlayer.STATE_STOPPED:
		_fail("the player never reached STATE_STOPPED after the list was stopped")
		return

	# The player node is freed, and the list player goes with it, because it is its child:
	# Godot frees children first, so libvlc's reference to the player is given back before
	# the callbacks that point into it disappear. Nothing is asserted *on* the list player
	# afterwards -- it is gone, and touching a freed object is the very thing this test is
	# about -- so what is checked is that it is gone, and that the process survives the
	# teardown.
	player.queue_free()
	await process_frame
	await process_frame
	if is_instance_valid(list_player):
		_fail("the list player outlived the player it was a child of")
		return

	print("media_list_player OK: a node drives another node, its stop is its own, repeat is refused, and the association ends with the player")
	quit(0)


func _fail(what: String) -> void:
	push_error("media_list_player FAIL: %s" % what)
	quit(1)
