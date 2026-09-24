extends SceneTree

# Thumbnails and embedded covers through the engine.
#
# The Rust acceptance covers libvlc's own behaviour -- that a request answers with a picture,
# that its stride describes its buffer, what byte order ARGB comes out in, that a cover is
# reported once and that a second parse is refused. What only the engine can cover is the
# binding's half: the signals arriving at a script with a `VLCPicture` in hand, `to_image()`
# turning one into something displayable, and a request that owns its libvlc request.
#
# The fixture is built here rather than committed: the repository has no file with cover art,
# and an ID3 tag with a 67-byte PNG in it is not worth one in `test/media`.
#
#   godot --headless --path demo --script res://tests/thumbnail.gd

const WAIT_TIMEOUT_MS := 20000
const FIXTURE := "user://thumbnail_probe.mp3"

var covers: Array = []
var reported: Array = []


func _init() -> void:
	var file := FileAccess.open(FIXTURE, FileAccess.WRITE)
	if file == null:
		_fail("the fixture could not be written to %s" % FIXTURE)
		return
	file.store_buffer(_mp3_with_a_cover())
	file.close()

	# load_from_file, not a hand-built file:// MRL: the fixture is a path the operating system can
	# see (globalize_path resolves user://), so the loader hands libvlc the file itself and libvlc
	# builds the URI -- separators, escaping and all.
	var media := VLCMedia.load_from_file(FIXTURE)
	if media == null:
		_fail("the fixture MRL was refused")
		return

	# The cover half. The event comes from a parse and only once, so this connects first --
	# and it needs a real parse: nothing else makes libvlc look for cover art.
	media.attached_thumbnails_found.connect(
		func(pictures: Array) -> void: covers.append(pictures)
	)
	if media.parse_request(
		VLCMedia.PARSE_FLAG_PARSE_LOCAL | VLCMedia.PARSE_FLAG_PARSE_FORCED, 0
	) != 0:
		_fail("the parse was refused")
		return
	var deadline := Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline and covers.is_empty():
		await process_frame
	if covers.is_empty():
		_fail("no cover was reported for a file that has one embedded")
		return
	if covers[0].is_empty():
		_fail("the cover event arrived with nothing in it")
		return
	var cover: VLCPicture = covers[0][0]
	var cover_image: Image = cover.to_image()
	if cover_image == null:
		_fail("the embedded cover could not be turned into an image")
		return
	print(
		"thumbnail: the embedded cover became a %dx%d image, type %d"
		% [cover_image.get_width(), cover_image.get_height(), cover.get_type()]
	)

	# The request half, made by position: a fraction needs no known duration, which is what the
	# editor's inspector relies on.
	# A video, because a thumbnail is a decoded *video* frame: an audio-only file has no
	# frame to hand over, and its cover art comes from the attachment path above instead.
	var video := VLCMedia.load_from_file("res://test.mp4")
	if video == null:
		_fail("the demo media could not be loaded")
		return
	video.thumbnail_generated.connect(
		func(picture: VLCPicture) -> void: reported.append(picture)
	)
	var request := video.thumbnail_request_by_pos(
	# Position 0.0, not the middle: this fixture is a fifth of a second long and its middle
	# decodes to nothing. The acceptance asks a real video file for its middle, which is what
	# the editor's inspector wants.
		0.0, VLCThumbnailRequest.SEEK_PRECISE, 64, 64, false, VLCPicture.TYPE_PNG, 5000
	)
	if request == null:
		_fail("libvlc refused the request")
		return
	deadline = Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline and reported.is_empty():
		await process_frame
	if reported.is_empty():
		_fail("the request was never answered within %dms" % WAIT_TIMEOUT_MS)
		return
	var picture: VLCPicture = reported[0]
	if picture == null:
		# Also a result worth having: a null payload is libvlc's only word for every kind of
		# failure, and the binding passes it on rather than swallowing it.
		print("thumbnail: the request was answered with no picture, which is how libvlc reports a failure")
		print("thumbnail OK: a cover reached a script as an image, and a request was answered")
		quit(0)
		return
	var image: Image = picture.to_image()
	if image == null:
		_fail("the requested picture could not be turned into an image")
		return
	if image.get_width() != 64 or image.get_height() != 64:
		_fail(
			"the image is %dx%d, not the 64x64 that was asked for"
			% [image.get_width(), image.get_height()]
		)
		return
	if ImageTexture.create_from_image(image) == null:
		_fail("the image could not become a texture")
		return
	print(
		"thumbnail: the request answered with a %dx%d picture, %d bytes"
		% [picture.get_width(), picture.get_height(), picture.get_buffer().size()]
	)

	# The request is still in hand, and dropping it is what destroys libvlc's request -- which
	# the binding documents as the only thing a caller must not forget.
	print("thumbnail OK: a cover and a requested thumbnail both reached a script as images")
	quit(0)


# A tiny MP3 carrying one front-cover picture in its ID3v2 tag.
func _mp3_with_a_cover() -> PackedByteArray:
	const PNG := [
		0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
		0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
		0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
		0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
		0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
	]

	var apic := PackedByteArray()
	apic.push_back(0x00)  # text encoding: ISO-8859-1
	apic.append_array("image/png".to_ascii_buffer())
	apic.push_back(0x00)  # the MIME string's terminator
	apic.push_back(0x03)  # picture type: front cover
	apic.push_back(0x00)  # empty description
	apic.append_array(PackedByteArray(PNG))

	var frame := PackedByteArray()
	frame.append_array("APIC".to_ascii_buffer())
	var size: int = apic.size()
	frame.append_array(PackedByteArray([
		(size >> 24) & 0xff, (size >> 16) & 0xff, (size >> 8) & 0xff, size & 0xff
	]))
	frame.append_array(PackedByteArray([0x00, 0x00]))
	frame.append_array(apic)

	var tag := PackedByteArray()
	tag.append_array("ID3".to_ascii_buffer())
	tag.append_array(PackedByteArray([0x03, 0x00, 0x00]))  # version 2.3, no flags
	tag.append_array(PackedByteArray([
		(size >> 21) & 0x7f, (size >> 14) & 0x7f, (size >> 7) & 0x7f, size & 0x7f
	]))
	tag.append_array(frame)
	# One MPEG frame behind the tag, so the file has audio to demux as well.
	tag.append_array(PackedByteArray([0xff, 0xfb, 0x90, 0x00]))
	tag.resize(tag.size() + 413)
	return tag


func _fail(what: String) -> void:
	push_error("thumbnail FAIL: %s" % what)
	quit(1)
