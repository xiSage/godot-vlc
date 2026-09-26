extends SceneTree

# Event callbacks are detached before the object that owns them is released.
#
# Releasing is not enough, and that is the whole point of this test. libvlc destroys a media
# player or a media only when its last reference goes, and this extension's wrapper is not
# the only one that can hold a reference: `VLCMediaList.add_media` hands libvlc a media and
# keeps no object of its own, so a script that drops its `VLCMedia` frees this extension's
# state while libvlc plays on. A parse or a thumbnail that lands afterwards would then run a
# callback against memory that is gone. `VLCMediaPlayer` has the same shape through a list
# player, which retains the player node it drives.
#
# `_debug_outstanding_attachments` counts the callbacks attached and not yet detached over
# the whole process, so the invariant checked here is exact: every wrapper that is freed
# takes its own attachments with it, whatever libvlc still holds. The media half also proves
# the reference really outlives the wrapper -- the list hands the media back after the
# wrapper is gone -- and then plays that media, which opens it through the path libvlc was
# given when the wrapper was built. That pointer outlives the wrapper for the same reason,
# and playing it is what reads it.
#
#   godot --headless --path demo --script res://tests/event_attachments.gd

# A file that is not on disk, so libvlc cannot be handed a path to it and the media is built
# on this extension's callbacks: see `media_loader.gd`.
const NOT_ON_DISK := "res://not-there.mp4"
const REPORT_TIMEOUT_MS := 10000
const PLAYER_ATTACHMENTS := 21
const LIST_PLAYER_ATTACHMENTS := 3
const MEDIA_ATTACHMENTS := 3
const MEDIA_LIST_ATTACHMENTS := 5


func _init() -> void:
	# A script error raised after an `await` kills this coroutine, and a headless Godot whose
	# script never reaches `quit()` then runs forever instead of failing. The watchdog is what
	# turns that into a failure with an exit code.
	create_timer(180.0).timeout.connect(func() -> void:
		push_error("event_attachments FAIL: 180s without finishing")
		quit(1)
	)

	var base: int = VLCMediaPlayer._debug_outstanding_attachments()

	# A player and the list player that drives it: libvlc is handed the player, and holds a
	# reference of its own to it for as long as the list player lives.
	var player := VLCMediaPlayer.new()
	root.add_child(player)
	var list_player := VLCMediaListPlayer.new()
	player.add_child(list_player)
	await process_frame
	var held: int = VLCMediaPlayer._debug_outstanding_attachments()
	if held != base + PLAYER_ATTACHMENTS + LIST_PLAYER_ATTACHMENTS:
		_fail(
			"a player with a list player under it holds %d callbacks, not %d"
			% [held - base, PLAYER_ATTACHMENTS + LIST_PLAYER_ATTACHMENTS]
		)
		return
	player.free()
	var freed: int = VLCMediaPlayer._debug_outstanding_attachments()
	if freed != base:
		_fail("freeing that player left %d callbacks attached" % (freed - base))
		return
	print("attachments: a freed player and its list player left %d callbacks behind" % (freed - base))

	# A media a list holds. The list keeps libvlc's media and nothing of this extension's, so
	# letting go of the wrapper frees it while the media stays.
	var list := VLCMediaList.new()
	var media := VLCMedia.load_from_file(NOT_ON_DISK)
	if media == null:
		_fail("no media was built for %s" % NOT_ON_DISK)
		return
	if media.get_mrl() != "imem://":
		_fail("%s was opened as %s, not through the callbacks" % [NOT_ON_DISK, media.get_mrl()])
		return
	if list.add_media(media) != 0:
		_fail("the list refused %s" % NOT_ON_DISK)
		return
	var with_media: int = VLCMediaPlayer._debug_outstanding_attachments()
	if with_media != base + MEDIA_LIST_ATTACHMENTS + MEDIA_ATTACHMENTS:
		_fail(
			"a list and the media in it hold %d callbacks, not %d"
			% [with_media - base, MEDIA_LIST_ATTACHMENTS + MEDIA_ATTACHMENTS]
		)
		return
	media = null
	await process_frame
	var after_media: int = VLCMediaPlayer._debug_outstanding_attachments()
	if after_media != base + MEDIA_LIST_ATTACHMENTS:
		_fail("freeing the media wrapper left %d of its callbacks attached" % (after_media - base - MEDIA_LIST_ATTACHMENTS))
		return
	print("attachments: a freed media left none behind while the list still held it")

	# The list still holds libvlc's media, and hands it back as a new wrapper -- which is the
	# proof that the wrapper was not the last reference.
	var revived := list.item_at_index(0)
	if revived == null:
		_fail("the list lost the media whose wrapper was freed")
		return
	if revived == media:
		_fail("the list handed back the wrapper that was freed")
		return
	if revived.get_mrl() != "imem://":
		_fail("the media the list kept reports %s, not the callback MRL" % revived.get_mrl())
		return

	# Playing it opens the media again, and the open callback reads the path the media was
	# built with -- a pointer that has to still be there after the wrapper is gone. The file
	# is not there either, so the failure is the observable end of it.
	var replay := VLCMediaPlayer.new()
	root.add_child(replay)
	replay.media = revived
	var errored := [false]
	replay.error.connect(func() -> void: errored[0] = true)
	replay.play()
	var deadline := Time.get_ticks_msec() + REPORT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline and not errored[0]:
		await process_frame
	if not errored[0]:
		_fail("playing the media whose wrapper was freed reported no error within %d ms" % REPORT_TIMEOUT_MS)
		return

	print("event_attachments OK: freed wrappers took their callbacks, and the media they left behind still opened")
	quit(0)


func _fail(what: String) -> void:
	push_error("event_attachments FAIL: %s" % what)
	quit(1)
