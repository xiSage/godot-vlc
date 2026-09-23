extends SceneTree

# The chapter and title descriptions reach a script as Godot's own type -- an Array of
# Dictionaries -- and that half never crosses into Rust, so only the engine can check
# its shape: which keys an entry carries, and that the two getters answer with an empty
# array rather than a null. src/acceptance.rs reads the same lists through the same C
# structs against the same fixture; this is the GDScript half of that claim, in the same
# way tests/track_info.gd is the engine half of the get_info() claim.

# The fixture lives with the acceptance media instead of in the demo, because that is
# where this repository keeps the files a test has to reproduce, and a 57 KB clip with
# four named chapters is what makes the chapter names assertable at all. The demo's own
# movie is the opposite case, a media with no chapter list, and it is played here and in
# src/acceptance.rs for the same reason.
#
#   godot --headless --path demo --script res://tests/chapters.gd

const STARTUP_TIMEOUT_MS := 20000
const CHAPTERED_MEDIA := "res://../test/media/h264_64x64_4s_4chapters.mp4"
# The demo's own movie, which carries no chapter table.
const PLAIN_MEDIA := "res://test.mp4"

const CHAPTER_NAMES := ["Opening Scene", "Second Part", "Third Part", "Closing Part"]
# Each chapter is one second long and starts on the second.
const CHAPTER_MS := 1000
const CHAPTER_COUNT := 4
const TITLE_MS := 4000

# What one entry of each list carries, and nothing else: the keys are the C struct's
# fields with libvlc's prefixes dropped, so a name that is not a field name is a bug.
const CHAPTER_KEYS := ["name", "time_offset", "duration"]
const TITLE_KEYS := ["name", "duration", "flags"]

var player: VLCMediaPlayer
var chapters_changed: Array[int] = []
var title_lists_changed := 0
var titles_selected: Array[int] = []
var selected_title_problems: Array[String] = []


func _init() -> void:
	player = VLCMediaPlayer.new()
	root.add_child(player)
	await process_frame
	player.chapter_changed.connect(_on_chapter_changed)
	player.title_list_changed.connect(_on_title_list_changed)
	player.title_selection_changed.connect(_on_title_selection_changed)

	# Nothing is known before something plays: libvlc publishes the chapter and title
	# lists with the *input*, not with the media, so a media set but not started has
	# neither -- and both getters answer with an empty array, which is what a script
	# iterating the result relies on.
	if not _chapters(0).is_empty():
		_fail("a player that has not started reports %d chapters" % _chapters(0).size())
		return
	if not player.get_full_title_descriptions().is_empty():
		_fail("a player that has not started reports a title list")
		return
	if player.get_chapter() != -1:
		_fail("a player that has not started reports chapter %d" % player.get_chapter())
		return

	player.media = VLCMedia.load_from_file(_media_path())
	player.play()

	var chapters := await _wait_for_chapters()
	if chapters.is_empty():
		_fail("no chapter list arrived within %d ms" % STARTUP_TIMEOUT_MS)
		return
	print("chapters: %s" % str(chapters))

	if chapters.size() != CHAPTER_COUNT:
		_fail("the fixture declares %d chapters; the list has %d"
			% [CHAPTER_COUNT, chapters.size()])
		return
	for i in chapters.size():
		var chapter: Dictionary = chapters[i]
		var problems := _key_problems(chapter, CHAPTER_KEYS)
		if problems != "":
			_fail("chapter %d has %s" % [i, problems])
			return
		if chapter["name"] != CHAPTER_NAMES[i]:
			_fail("chapter %d is named '%s', not '%s'"
				% [i, chapter["name"], CHAPTER_NAMES[i]])
			return
		if chapter["time_offset"] != i * CHAPTER_MS or chapter["duration"] != CHAPTER_MS:
			_fail("chapter %d is at %d ms for %d ms; the fixture declares %d and %d"
				% [i, chapter["time_offset"], chapter["duration"],
					i * CHAPTER_MS, CHAPTER_MS])
			return

	# The title list. An MP4 only has one when its chapter table exists, which is the
	# same table the list above came from, so this is one title and not zero.
	var titles := player.get_full_title_descriptions()
	if titles.size() != 1:
		_fail("the fixture has one title; the list has %d" % titles.size())
		return
	print("titles: %s" % str(titles))
	var problems = _key_problems(titles[0], TITLE_KEYS)
	if problems != "":
		_fail("the title has %s" % problems)
		return
	if titles[0]["duration"] != TITLE_MS:
		_fail("the title is %d ms long; the list says %d" % [TITLE_MS, titles[0]["duration"]])
		return
	# No flag is set on the title an MP4's chapter table produces, so a caller that
	# wants to tell a menu from a film reads flags == 0 rather than a "plain" constant,
	# which libvlc does not define.
	if titles[0]["flags"] != 0:
		_fail("the fixture's title carries flags %d" % titles[0]["flags"])
		return
	# An MP4's chapter table does not name its title, so the name is libvlc's own: the
	# index, and the length in brackets. Nothing in the file says "Title 0".
	if not titles[0]["name"].begins_with("Title 0"):
		_fail("the fixture's title is named '%s'" % titles[0]["name"])
		return

	# The counts and the selected index are the same answer the descriptions carry.
	if player.get_chapter_count() != CHAPTER_COUNT:
		_fail("the fixture has %d chapters; get_chapter_count() says %d"
			% [CHAPTER_COUNT, player.get_chapter_count()])
		return
	var selected := player.get_chapter()
	if selected < 0 or selected >= CHAPTER_COUNT:
		_fail("the selected chapter is %d on a media with %d chapters"
			% [selected, CHAPTER_COUNT])
		return
	if player.get_chapter_count_for_title(0) != CHAPTER_COUNT:
		_fail("title 0 has %d chapters; get_chapter_count_for_title(0) says %d"
			% [CHAPTER_COUNT, player.get_chapter_count_for_title(0)])
		return
	# A title index is a signed C int and libvlc asserts on a negative one, so the
	# binding answers -1 instead of passing it down. See the addon's documentation.
	if player.get_chapter_count_for_title(-1) != -1:
		_fail("a negative title index answers %d instead of -1"
			% player.get_chapter_count_for_title(-1))
		return
	# The same list, asked for by an explicit title index and by "whatever is selected".
	if _chapters(0).size() != chapters.size():
		_fail("title 0 has %d chapters; the selected title has %d"
			% [_chapters(0).size(), chapters.size()])
		return
	if not _chapters(1).is_empty():
		_fail("the fixture has one title, but title 1 reports chapters")
		return

	# The events. The lists are published by the input thread and libvlc's event reaches
	# a script on the next frame the player drains it, so neither has necessarily
	# happened by the time the lists above have become readable.
	if not await _wait_for_event(func() -> bool: return title_lists_changed > 0):
		_fail("no title_list_changed was reported for a media with a title list")
		return
	if not await _wait_for_event(func() -> bool: return not titles_selected.is_empty()):
		_fail("no title_selection_changed was reported for a media with a title")
		return
	if titles_selected[-1] != 0:
		_fail("the title selection events are %s" % str(titles_selected))
		return
	# The signal carries a copy of the selected title rather than only its index, and the
	# copy is the same shape as one entry of the getter above -- libvlc points at its own
	# frame where it raises this, so a dictionary with the same keys is what came out of it.
	if not selected_title_problems.is_empty():
		_fail(selected_title_problems[0])
		return

	var before := chapters_changed.size()
	player.set_chapter(2)
	if not await _wait_for_chapter(2, before):
		_fail("set_chapter(2) was never reported")
		return

	before = chapters_changed.size()
	player.set_chapter(0)
	if not await _wait_for_chapter(0, before):
		_fail("set_chapter(0) was never reported")
		return

	# Once the input is gone there is nothing left to describe, and the dropdown in the
	# demo is emptied by the same answer.
	player.stop_async()
	if not await _wait_for_no_chapters():
		_fail("a stopped player still reports %d chapters" % _chapters(0).size())
		return

	# The same two getters over a media that has no chapter list at all. That is the case
	# a caller meets most often, and the one the fold from libvlc's -1 has to cover: the
	# description getter answers -1 there, not an empty list, and a script must not be
	# handed the difference. The demo's own movie is such a file and 466 seconds long, so
	# what follows cannot be playback having already ended.
	player.media = VLCMedia.load_from_file(_plain_media_path())
	player.play()
	if not await _wait_for_event(func() -> bool: return player.is_playing()):
		_fail("the demo's own movie never started playing")
		return
	# The wait src/acceptance.rs takes for the same measurement, so that a title list
	# which appears late cannot be read as one that never appears.
	await _wait_for_milliseconds(500)
	if not player.get_full_chapter_descriptions(-1).is_empty():
		_fail("a media with no chapter list reports %d chapters"
			% player.get_full_chapter_descriptions(-1).size())
		return
	if not player.get_full_title_descriptions().is_empty():
		_fail("a media with no chapter list reports a title list")
		return
	# get_chapter() and the list disagree here, and libvlc does not apologise for it: with
	# an input that has no chapters the C getter answers the index its field starts at,
	# 0, and keeps -1 for having no input at all. A caller that trusted the index alone
	# would offer "chapter 1" of a movie it has no chapter list for, so the list is what
	# tells the two apart -- and the dropdown in the demo is built from the list.
	if player.get_chapter() != 0:
		_fail("a media with no chapter list reports chapter %d, not the 0 its field starts at"
			% player.get_chapter())
		return

	print("chapters: %d named, titles: %d, chapter events: %s, title selection: %s"
		% [CHAPTER_COUNT, titles.size(), str(chapters_changed), str(titles_selected)])
	print("chapters OK")
	quit(0)


func _media_path() -> String:
	return ProjectSettings.globalize_path(CHAPTERED_MEDIA).simplify_path()


func _plain_media_path() -> String:
	return ProjectSettings.globalize_path(PLAIN_MEDIA).simplify_path()


# The chapters of one title, or an empty array. -1 is "the title that is playing", which
# is what a caller wants when it has no title index to hand.
func _chapters(title: int) -> Array:
	return player.get_full_chapter_descriptions(title)


func _wait_for_chapters() -> Array:
	var deadline := Time.get_ticks_msec() + STARTUP_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		var chapters := _chapters(0)
		if not chapters.is_empty():
			return chapters
		await process_frame
	return []


# Waits for the report that a jump landed, counting only the events that arrived after it.
# The index the getter ends on is not the thing to wait for: libvlc follows the demuxer, so
# the reporting keeps moving after the jump and can pass through chapters that were never
# played. What a caller can rely on is that the chapter it asked for is reported.
func _wait_for_chapter(number: int, from: int) -> bool:
	var deadline := Time.get_ticks_msec() + STARTUP_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		if chapters_changed.slice(from).has(number):
			return true
		await process_frame
	return false


func _wait_for_no_chapters() -> bool:
	var deadline := Time.get_ticks_msec() + STARTUP_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		if _chapters(0).is_empty():
			return true
		await process_frame
	return false


# Waits for a signal the test has to observe rather than read: the player emits it on the
# frame it drains the event, which is not the frame the underlying change is visible on.
func _wait_for_event(reached: Callable) -> bool:
	var deadline := Time.get_ticks_msec() + STARTUP_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		if reached.call():
			return true
		await process_frame
	return false


# A pause of a known length, on the frame clock: some measurements are "give libvlc long
# enough to do the thing it would do", and there is no signal to wait for.
func _wait_for_milliseconds(milliseconds: int) -> void:
	var deadline := Time.get_ticks_msec() + milliseconds
	while Time.get_ticks_msec() < deadline:
		await process_frame


# What is wrong with an entry's keys, or "" when nothing is: every documented key has to
# be there, and nothing else may be. A getter that grew a key nobody asked for is as much
# a change to the dictionary as one that dropped a key, so it fails here too.
func _key_problems(entry: Dictionary, keys: Array) -> String:
	for key in keys:
		if not entry.has(key):
			return "no key '%s'" % key
	for key in entry:
		if not keys.has(key):
			return "an unexpected key '%s'" % key
	return ""


func _on_chapter_changed(chapter: int) -> void:
	chapters_changed.append(chapter)


func _on_title_list_changed() -> void:
	title_lists_changed += 1


func _on_title_selection_changed(index: int, title: Dictionary) -> void:
	titles_selected.append(index)
	var problems := _key_problems(title, TITLE_KEYS)
	if problems != "":
		selected_title_problems.append("the selected title has %s" % problems)
	elif title["name"].is_empty():
		selected_title_problems.append("the selected title has no name")


func _fail(what: String) -> void:
	push_error("chapters FAIL: %s" % what)
	quit(1)
