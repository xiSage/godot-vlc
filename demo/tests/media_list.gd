extends SceneTree

# The media list through the engine: the wrapper a script builds, its five signals, and
# the subitems of a media that has been parsed.
#
# The Rust acceptance covers the runtime's half -- the lock, the ownership, the four
# item events in pairs, and the end of a parse. What only this can cover is the
# binding's half: that a script can build a list, that its signals arrive with a
# `VLCMedia` in hand (they are emitted from a deferred drain, because a signal carrying
# a Godot object has to build it on the main thread), and that a media's subitems can be
# read and written to as the read-only list they are.
#
#   godot --headless --path demo --script res://tests/media_list.gd

const WAIT_TIMEOUT_MS := 20000
const PLAYLIST_FILE := "user://media_list_sample.m3u"

var added: Array = []
var deleted: Array = []
var will_add := 0
var will_delete := 0
var ends := 0


func _init() -> void:
	var list := VLCMediaList.new()
	if list == null:
		_fail("VLCMediaList.new() answered null")
		return
	if list.count() != 0 or list.is_read_only():
		_fail("a fresh list holds %d entries and reports read_only=%s" % [
			list.count(), list.is_read_only()
		])
		return

	list.item_added.connect(
		func(media: VLCMedia, index: int) -> void: added.append([media.get_mrl(), index])
	)
	list.item_will_add.connect(
		func(_media: VLCMedia, _index: int) -> void: will_add += 1
	)
	list.item_deleted.connect(
		func(media: VLCMedia, index: int) -> void: deleted.append([media.get_mrl(), index])
	)
	list.item_will_delete.connect(
		func(_media: VLCMedia, _index: int) -> void: will_delete += 1
	)
	list.end_reached.connect(func() -> void: ends += 1)

	var first := VLCMedia.load_from_mrl("file:///first.mp4")
	var second := VLCMedia.load_from_mrl("file:///second.mp4")
	if list.add_media(first) != 0 or list.add_media(second) != 0:
		_fail("adding a media to a list a script built was refused")
		return
	if list.count() != 2 or list.index_of_item(second) != 1:
		_fail("after adding two, the list holds %d and the second is at %d" % [
			list.count(), list.index_of_item(second)
		])
		return
	var all := list.get_media_array()
	if all.size() != 2 or all[0].get_mrl() != "file:///first.mp4":
		_fail("get_media_array answered %d entries, the first being %s" % [
			all.size(), all[0].get_mrl() if all.size() > 0 else "<none>"
		])
		return
	if list.insert_media(first, 0) != 0 or list.count() != 3:
		_fail("the insert did not take")
		return
	if list.remove_index(0) != 0 or list.count() != 2:
		_fail("the removal did not take")
		return
	if list.remove_index(99) != -1:
		_fail("removing an index past the end was accepted")
		return

	# The signals arrive from a deferred drain, so they land after the calls that caused
	# them -- three adds (two appends and one insert) and one removal.
	var deadline := Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline and added.size() < 3:
		await process_frame
	if added.size() != 3 or deleted.size() != 1:
		_fail("the drains carried added=%d deleted=%d, not three and one" % [
			added.size(), deleted.size()
		])
		return
	if will_add != 3 or will_delete != 1:
		_fail("the pairs came out as will_add=%d will_delete=%d" % [will_add, will_delete])
		return
	if added[0][0] != "file:///first.mp4" or added[0][1] != 0:
		_fail("the first item_added carried %s" % str(added[0]))
		return
	print("media_list: three adds and a removal arrived as %d added, %d deleted" % [
		added.size(), deleted.size()
	])

	# The subitems of a parsed playlist. There is no playlist in the demo project, so
	# this writes one: the file is what libvlc parses, and its entries are what turn up
	# in the subitems list.
	var entry := ProjectSettings.globalize_path("res://test.mp4").replace("\\", "/")
	var file := FileAccess.open(PLAYLIST_FILE, FileAccess.WRITE)
	if file == null:
		_fail("the playlist fixture could not be written to %s" % PLAYLIST_FILE)
		return
	file.store_line("#EXTM3U")
	file.store_line("file:///%s" % entry)
	file.close()

	var playlist_path := ProjectSettings.globalize_path(PLAYLIST_FILE).replace("\\", "/")
	var playlist := VLCMedia.load_from_mrl("file:///%s" % playlist_path)
	if playlist == null:
		_fail("the playlist MRL was refused")
		return
	var subitems := playlist.get_subitems()
	if subitems == null:
		_fail("a media this binding built answered no subitems list")
		return
	if not subitems.is_read_only():
		_fail("a media's subitems are read-only, and this list says they are not")
		return
	if subitems.add_media(first) != -1:
		_fail("a read-only list accepted a media")
		return
	subitems.end_reached.connect(func() -> void: ends += 1)
	if playlist.parse_request(
		VLCMedia.PARSE_FLAG_PARSE_LOCAL | VLCMedia.PARSE_FLAG_PARSE_FORCED, 0
	) != 0:
		_fail("the parse was refused")
		return

	deadline = Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline and ends == 0:
		await process_frame
	if ends == 0:
		_fail("the end of the parse never arrived within %dms" % WAIT_TIMEOUT_MS)
		return
	if subitems.count() != 1:
		_fail("the parsed playlist holds %d subitems, not the one entry it names" % subitems.count())
		return
	var entries := subitems.get_media_array()
	if entries.size() != 1 or not entries[0].get_mrl().contains("test.mp4"):
		_fail("the subitem reads %s" % (entries[0].get_mrl() if entries.size() > 0 else "<none>"))
		return
	print("media_list: the parsed playlist holds %d subitem(s), the first being %s" % [
		subitems.count(), entries[0].get_mrl()
	])

	print("media_list OK: a script builds a list, its signals carry media, and a parsed playlist fills its subitems")
	quit(0)


func _fail(what: String) -> void:
	push_error("media_list FAIL: %s" % what)
	quit(1)
