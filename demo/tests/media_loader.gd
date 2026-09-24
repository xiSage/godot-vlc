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
# And it is the only place the identity of a loaded media can be measured, because it is
# the only test that goes through the loader. Which identity that is depends on the access
# libvlc ended up with, and both cases are pinned below: a media whose file the operating
# system can see is opened by libvlc itself and reports a file:// MRL and a file type --
# that is the editor and any source run -- while one it cannot see is read through the
# extension's callbacks, reports the constant "imem://" and comes back untyped, which is
# the media of an exported project's PCK. The engine's resource_path is what names the
# file either way, and the two answer different questions -- see the class documentation
# of VLCMedia.
#
#   godot --headless --path demo --script res://tests/media_loader.gd

# A file that is not on disk, so libvlc cannot be handed a path to it: globalize_path
# still turns this into a native path, and the loader falls back to reading it through
# the extension's callbacks. Nothing parses it here, so its content does not matter.
const NOT_ON_DISK := "res://not-there.mp4"


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

	# Identity: the loader path is the one case where the engine names the file.
	if media.resource_path != "res://test.mp4":
		_fail("a loaded media reports resource_path %s" % media.resource_path)
		return
	if not media.get_mrl().begins_with("file://"):
		_fail("a media from res:// reports the MRL %s, not the file one" % media.get_mrl())
		return
	if media.get_type() != VLCMedia.MEDIA_TYPE_FILE:
		_fail("a media libvlc opens itself is typed %d, and it is a file" % media.get_type())
		return

	# A media a script builds itself has no resource_path, and the same MRL as the one the
	# loader made: both are files libvlc can open.
	var direct := VLCMedia.load_from_file("res://test.mp4")
	if !direct.resource_path.is_empty():
		_fail("a media built by a script reports resource_path %s" % direct.resource_path)
		return
	if not direct.get_mrl().begins_with("file://"):
		_fail("a media built by a script reports the MRL %s" % direct.get_mrl())
		return
	if direct.get_type() != VLCMedia.MEDIA_TYPE_FILE:
		_fail("a media a script loaded from a file is typed %d" % direct.get_type())
		return

	# A file the operating system cannot see goes the other way, through the extension's
	# callbacks: libvlc is given no path, so it has no MRL of its own to report and no type
	# to guess. This is the exported project's case, where every media is inside the PCK.
	var hidden := VLCMedia.load_from_file(NOT_ON_DISK)
	if hidden.get_mrl() != "imem://":
		_fail("a media read through the callbacks reports the MRL %s" % hidden.get_mrl())
		return
	if hidden.get_type() != VLCMedia.MEDIA_TYPE_UNKNOWN:
		_fail("a media built on callbacks is typed %d, and libvlc cannot guess it" % hidden.get_type())
		return

	# duplicate_media is a copy, not the same object, and it points where the
	# original does.
	var copy := direct.duplicate_media()
	if copy == null:
		_fail("duplicate_media returned null")
		return
	if copy == direct:
		_fail("duplicate_media returned the media itself")
		return
	if copy.get_mrl() != direct.get_mrl():
		_fail("the copy reports %s where the original reports %s" % [copy.get_mrl(), direct.get_mrl()])
		return

	print("media_loader OK: res://test.mp4 is a VLCMedia, res://subtitle.srt a VLCSubtitle")
	print("media_loader identity: loader resource_path=%s mrl=%s type=%d" % [
		media.resource_path, media.get_mrl(), media.get_type()
	])
	print("media_loader identity: not on disk mrl=%s type=%d" % [
		hidden.get_mrl(), hidden.get_type()
	])
	quit(0)


func _fail(what: String) -> void:
	push_error("media_loader FAIL: %s" % what)
	quit(1)
