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
# Listening is not enough on its own, and that is the second half of this test. A cover is
# reported only by the parse that finds it, and libvlc refuses to parse a media it has already
# read: the player parses a media it is given by itself, without reporting anything, so by the
# time a panel can see the media on the player, the cover has already gone to nobody. The
# listener and the parse both have to be in place before the player has the media, which is
# what the demo's load path does -- so this drives that path, through the file dialog's signal,
# rather than assigning a media to the scene behind its back.
#
# The panel is the embedded script of `main.tscn`, so this loads the scene and drives it: the
# cover arrives for the FLAC as it is loaded, a press shows the cover it kept, playing the media
# does not lose it, and a video answers a press with a decoded frame.
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
	var dialog := scene.find_child("FileDialog", true, false) as FileDialog
	if button == null or info == null or picture == null or dialog == null:
		_fail("the demo scene has no thumbnail panel")
		return

	# An audio file, loaded the way a person loads one: the cover art is the only picture it has,
	# and the panel hears it from the parse the demo asks for as the media arrives -- before the
	# player has the media, and unpressed, which is what a person sees.
	dialog.file_selected.emit(ProjectSettings.globalize_path(COVER))
	if not await _wait_for(info, "embedded cover", WAIT_TIMEOUT_MS):
		_fail("no cover was shown for a file that has one (%s)" % info.text)
		return
	if picture.texture == null:
		_fail("the panel named a cover but showed no picture")
		return
	print(
		"thumbnail_panel: the FLAC's cover is up unpressed, %dx%d"
		% [picture.texture.get_width(), picture.texture.get_height()]
	)

	# Pressed: the event has fired and the media has been read, so there is nothing left to hear
	# and the panel shows the picture it kept.
	info.text = ""
	button.pressed.emit()
	await create_timer(1.0).timeout
	if not info.text.contains("remembered"):
		_fail("a press did not show the kept cover (%s)" % info.text)
		return
	print("thumbnail_panel: a press shows the cover it kept")

	# Played: the player opens the input item, which is what makes a later parse refused. The
	# cover was heard before that, and it must still be up after it.
	scene.call("play")
	await create_timer(2.0).timeout
	info.text = ""
	button.pressed.emit()
	await create_timer(1.0).timeout
	if not info.text.contains("embedded cover"):
		_fail("the cover went missing once the media played (%s)" % info.text)
		return
	print("thumbnail_panel: the cover is still up after the media played")
	scene.call("stop_async")
	await create_timer(0.5).timeout

	# A video, loaded the same way: a decoded frame, on the other signal, and the cover that
	# belonged to the media before it must not be what a press shows.
	dialog.file_selected.emit(ProjectSettings.globalize_path(VIDEO))
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
