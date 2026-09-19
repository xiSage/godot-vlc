extends SceneTree

# The media resource loader. It used to be a GDScript that reached the extension through
# the engine singleton; it is now a class the extension registers itself, and this is
# the only test that goes through it -- every other one builds a media with
# VLCMedia.load_from_file, while a scene that references res://media.mp4 (like
# demo/main.tscn does) gets it from here.
#
# It also checks the boundary with the subtitle importer: media files are loaded, and
# subtitle files are imported, and neither mechanism should take the other's files.
#
#   godot --headless --path demo --script res://tests/media_loader.gd

func _init() -> void:
	if not ResourceLoader.exists("res://test.mp4", "VLCMedia"):
		_fail("no loader answers for res://test.mp4 as a VLCMedia")
		return

	var media: Variant = load("res://test.mp4")
	if not (media is VLCMedia):
		_fail("res://test.mp4 loaded as %s instead of a VLCMedia" % media)
		return

	var subtitle: Variant = load("res://subtitle.srt")
	if not (subtitle is VLCSubtitle):
		_fail("res://subtitle.srt loaded as %s instead of a VLCSubtitle" % subtitle)
		return

	print("media_loader OK: res://test.mp4 is a VLCMedia, res://subtitle.srt a VLCSubtitle")
	quit(0)


func _fail(what: String) -> void:
	push_error("media_loader FAIL: %s" % what)
	quit(1)
