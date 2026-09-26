extends SceneTree

# The demo panel's thumbnail button, driven the way a person drives it.
#
# `thumbnail.gd` covers the API half -- the signals arriving with a `VLCPicture` in hand,
# `to_image()`, a request that owns its libvlc request. What it cannot cover is the panel's
# own wiring, and that is where this went wrong: the panel listened for `thumbnail_generated`
# alone. A video answers that with a decoded frame, but a FLAC has no frame to decode, and
# the only picture it carries is its cover art, which libvlc reports as
# `attached_thumbnails_found` from the parse that finds it -- so an audio file showed
# nothing, while the editor's inspector, listening for both, showed the cover.
#
# The panel is the embedded script of `main.tscn`, so this loads the scene and presses the
# button: a cover for the FLAC, the same cover remembered on a second press (libvlc reports
# it once per parse, and the parse is done), and a decoded frame for a video.
#
#   godot --headless --path demo --script res://tests/thumbnail_panel.gd

const WAIT_TIMEOUT_MS := 30000
const COVER := "res://test_cover.flac"
const VIDEO := "res://test.mp4"


func _init() -> void:
	create_timer(180.0).timeout.connect(_on_watchdog)

	var packed := load("res://main.tscn") as PackedScene
	if packed == null:
		_fail("the demo scene did not load")
		return
	var scene := packed.instantiate()
	root.add_child(scene)
	var button := scene.find_child("ThumbnailRequest", true, false) as Button
	var info := scene.find_child("ThumbnailInfo", true, false) as Label
	var picture := scene.find_child("Thumbnail", true, false) as TextureRect
	if button == null or info == null or picture == null:
		_fail("the demo scene has no thumbnail panel")
		return

	# An audio file: the cover art is the only picture it has, and it arrives from a parse.
	var media := VLCMedia.load_from_file(COVER)
	if media == null:
		_fail("the fixture was refused")
		return
	scene.set("media", media)
	button.pressed.emit()
	if not await _wait_for(info, "embedded cover", WAIT_TIMEOUT_MS):
		_fail("no cover was shown for a file that has one (%s)" % info.text)
		return
	if picture.texture == null:
		_fail("the panel named a cover but showed no picture")
		return
	print(
		"thumbnail_panel: the FLAC's cover is up, %dx%d"
		% [picture.texture.get_width(), picture.texture.get_height()]
	)

	# Pressed again: the event fires once per parse and the media has been read, so the panel
	# has nothing left to hear it from and shows the picture it kept instead.
	info.text = ""
	button.pressed.emit()
	await create_timer(1.0).timeout
	if not info.text.contains("remembered"):
		_fail("a second press did not show the remembered cover (%s)" % info.text)
		return
	print("thumbnail_panel: a second press shows the cover it kept")

	# A video: a decoded frame, on the other signal, and it must not give way to a cover that
	# belongs to a media that is no longer the one on the player.
	scene.set("media", VLCMedia.load_from_file(VIDEO))
	info.text = ""
	button.pressed.emit()
	if not await _wait_for(info, "generated", WAIT_TIMEOUT_MS):
		_fail("no frame was shown for a video (%s)" % info.text)
		return
	if picture.texture == null or picture.texture.get_width() != 192:
		_fail("the panel asked for a 192 wide frame and showed something else")
		return
	print(
		"thumbnail_panel: the video's frame is up, %dx%d"
		% [picture.texture.get_width(), picture.texture.get_height()]
	)
	print("thumbnail_panel OK")
	quit(0)


# Polls the panel's line rather than awaiting a signal: what is under test is what the panel
# puts on screen, and the two signals race each other.
func _wait_for(info: Label, wanted: String, ms: int) -> bool:
	var deadline := Time.get_ticks_msec() + ms
	while Time.get_ticks_msec() < deadline:
		if info.text.contains(wanted):
			return true
		await create_timer(0.25).timeout
	return false


func _on_watchdog() -> void:
	push_error("thumbnail_panel FAIL: the run never finished")
	quit(1)


func _fail(what: String) -> void:
	push_error("thumbnail_panel FAIL: %s" % what)
	quit(1)
