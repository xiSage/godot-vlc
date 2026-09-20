extends SceneTree

# The inspector's control: the picture and the metadata the editor's plugin puts at the top of
# every VLCMedia's inspector.
#
# What only the engine can cover is *when* the control can read what libvlc already knows. The
# control is built by the plugin before it is handed to the inspector (`show_media` first, then
# `add_custom_control`), so its labels do not exist yet when it describes the media; a panel that
# only ever redraws on a parse event therefore shows nothing for a media that was read before the
# panel was built -- which is what opening the same file a second time looks like, because the
# loader caches the media by path. The first half of this drives two opens of one media in that
# order and asserts that the second shows what the first did.
#
# The second half is the defect that came out of a cover being carried across media: the fixture has
# an embedded cover and `test.mp4` has none, and after the fixture has been read, opening the MP4
# must not show the fixture's cover.
#
# The fixture is a committed 13 KB FLAC rather than one built here, because what this test is about
# only happens to a media the *loader* makes: libvlc's art cache is keyed on a media's URL, and
# every media this extension loads through callbacks answers the constant `imem://`, so a media with
# a cover and a media without one share one cache slot -- which is why the cover is left on the
# media (`VLCMedia.editor_cover`) instead of read from that cache. `res://test.mp4` is the media the
# demo already ships, and it is exactly the no-cover case. It was built once with:
#
#   ffmpeg -f lavfi -i "sine=frequency=440:duration=0.3" -f lavfi -i "color=c=red:s=16x16:d=1" \
#     -map 0:a -map 1:v -c:a flac -c:v png -disposition:v attached_pic \
#     -metadata title="inspector cover fixture" -metadata artist="fixture" test_cover.flac
#
# What it does NOT cover: `VlcMediaInspectorPlugin`, and so the three lines that build the control
# and hand it to the inspector -- Godot refuses to instantiate an `EditorInspectorPlugin` outside
# the editor ("Class 'VlcMediaInspectorPlugin' can only be instantiated by editor", measured). Every
# other part of the behaviour is in the control or in the media, and both are driven here.
#
#   godot --headless --path demo --script res://tests/media_inspector.gd

const FIXTURE := "res://test_cover.flac"
const PLAIN := "res://test.mp4"
const TITLE := "inspector cover fixture"
const WAIT_TIMEOUT_MS := 25000


func _init() -> void:
	# A script error raised after an `await` kills this coroutine, and a headless Godot whose
	# script never reaches `quit()` then runs forever instead of failing. The watchdog is what
	# turns that into a failure with an exit code.
	create_timer(180.0).timeout.connect(func() -> void:
		push_error("media_inspector FAIL: 180s without finishing")
		quit(1)
	)

	var fixture: VLCMedia = ResourceLoader.load(FIXTURE)
	if fixture == null:
		_fail("the fixture %s did not load as a VLCMedia" % FIXTURE)
		return
	print("inspector: fixture mrl=%s" % fixture.get_mrl())

	# The first open: this is the one that parses the media and receives its cover.
	var first: Dictionary = await _open(fixture, WAIT_TIMEOUT_MS, true)
	print("inspector: fixture open #1 metadata=%s" % _one_line(first["metadata"]))
	print("inspector: fixture open #1 picture=%s (%s)" % [first["picture"], first["origin"]])
	if not first["metadata"].contains(TITLE):
		_fail("the first open did not show the fixture's title")
		return
	if first["picture"] == "none":
		_fail("the first open showed no cover for a media that has one")
		return

	# The second open of the same media, which is what selecting the file again does. libvlc has
	# read it by now, so no parse event and no cover event will come.
	if fixture.get_parsed_status() != VLCMedia.PARSED_STATUS_DONE:
		_fail("the fixture is not parsed_status done after the first open, so this test is not about the case it names")
		return
	var second: Dictionary = await _open(fixture, WAIT_TIMEOUT_MS, true)
	print("inspector: fixture open #2 metadata=%s" % _one_line(second["metadata"]))
	print("inspector: fixture open #2 picture=%s (%s)" % [second["picture"], second["origin"]])
	if not second["metadata"].contains(TITLE):
		_fail("the second open showed no metadata for a media that still reports it")
		return
	if second["picture"] == "none":
		_fail("the second open showed no cover for a media that has one")
		return

	# Now a media with no cover at all, read *after* the fixture: the fixture's cover must not
	# follow it here. The first open of the MP4 is what parses it -- and it is the parse that
	# writes the shared `imem://` artwork slot -- so the second open is the one that used to
	# arrive with the fixture's cover attached.
	var plain: VLCMedia = ResourceLoader.load(PLAIN)
	if plain == null:
		_fail("the media %s did not load as a VLCMedia" % PLAIN)
		return
	await _open(plain, WAIT_TIMEOUT_MS, false)
	if plain.get_parsed_status() != VLCMedia.PARSED_STATUS_DONE:
		_fail("%s did not finish parsing, so the second open is not the case this test names" % PLAIN)
		return
	var plain_again: Dictionary = await _open(plain, WAIT_TIMEOUT_MS, false)
	print("inspector: %s open #2 picture=%s (%s)" % [
		PLAIN, plain_again["picture"], plain_again["origin"]
	])
	if plain_again["origin"].contains("embedded cover"):
		_fail("a media with no cover was shown with a cover: %s" % plain_again["origin"])
		return

	print("media_inspector OK: both opens of one media show its title and its cover, and a media without one is not given another media's")
	quit(0)


# One open, in the order the plugin uses: build the control, fill it, hand it to the inspector.
func _open(media: VLCMedia, timeout_ms: int, need_fixture_title: bool) -> Dictionary:
	var control: Node = ClassDB.instantiate("VlcMediaInspector")
	control.call("show_media", media)
	root.add_child(control)
	await process_frame
	await process_frame

	var picture: TextureRect = control.get_child(0)
	var origin: Label = control.get_child(1)
	var metadata: Label = control.get_child(2)
	var deadline := Time.get_ticks_msec() + timeout_ms
	while Time.get_ticks_msec() < deadline:
		await process_frame
		if origin.text == "":
			continue
		# The fixture's opens are settled when the title and the cover are both up; the MP4's are
		# settled by its row saying anything at all, since what is asserted about it is what that
		# line does *not* say.
		if not need_fixture_title:
			break
		if picture.texture != null and metadata.text.contains(TITLE):
			break

	var shown := {
		"picture": "none" if picture.texture == null else "%dx%d" % [
			picture.texture.get_width(), picture.texture.get_height()
		],
		"origin": origin.text,
		"metadata": metadata.text,
	}
	control.free()
	return shown


func _one_line(text: String) -> String:
	return text.replace("\n", " | ")


func _fail(what: String) -> void:
	push_error("media_inspector FAIL: %s" % what)
	quit(1)
