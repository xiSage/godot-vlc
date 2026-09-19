extends SceneTree

# get_info() turns libvlc's track struct into one dictionary, and only the engine can
# prove that half: src/acceptance.rs reads the same fields through the same reader, but
# the dictionary itself -- which keys exist, their types, and that a type's member is
# absent rather than zero -- is built with Godot's own Variant type and never crosses
# into Rust. So this is the GDScript half of the same claim, in the same way
# tests/subtitles.gd is the engine half of the subtitle claim.
#
# The demo media is the one with geometry and audio in it; src/acceptance.rs asserts the
# same numbers (854x480, 1280:1281, 2 channels at 48 kHz) against the same file.
#
#   godot --headless --path demo --script res://tests/track_info.gd

const STARTUP_TIMEOUT_MS := 20000
const MEDIA_WIDTH := 854
const MEDIA_HEIGHT := 480
const MEDIA_SAR_NUM := 1280
const MEDIA_SAR_DEN := 1281
const MEDIA_CHANNELS := 2
const MEDIA_RATE := 48000

# Every track carries these, whatever its type.
const COMMON_KEYS := ["type", "codec", "codec_description", "original_fourcc", "profile",
	"level", "bitrate", "language", "description", "id", "id_stable", "name", "selected"]
# And the union member that its type names, and no other.
const VIDEO_KEYS := ["width", "height", "sar_num", "sar_den", "frame_rate_num",
	"frame_rate_den", "orientation", "projection", "pose_yaw", "pose_pitch", "pose_roll",
	"pose_field_of_view", "multiview"]
const AUDIO_KEYS := ["channels", "rate"]
const TEXT_KEYS := ["encoding"]

var player: VLCMediaPlayer


func _init() -> void:
	player = VLCMediaPlayer.new()
	root.add_child(player)
	await process_frame

	player.media = VLCMedia.load_from_file(ProjectSettings.globalize_path("res://test.mp4"))
	player.play()

	var video := await _wait_for_info(VLCTrack.TYPE_VIDEO, "width")
	if video.is_empty():
		_fail("no video track reported a width within %d ms" % STARTUP_TIMEOUT_MS)
		return
	print("track_info: video %s" % str(video))

	var missing := _missing(video, COMMON_KEYS)
	if missing != "":
		_fail("a video track's dictionary has no common key '%s'" % missing)
		return
	missing = _missing(video, VIDEO_KEYS)
	if missing != "":
		_fail("a video track's dictionary has no video key '%s'" % missing)
		return
	for key in AUDIO_KEYS + TEXT_KEYS:
		if video.has(key):
			_fail("a video track's dictionary carries '%s', which its type does not name" % key)
			return

	if video["type"] != VLCTrack.TYPE_VIDEO:
		_fail("a video track reports type %d" % video["type"])
		return
	if video["width"] != MEDIA_WIDTH or video["height"] != MEDIA_HEIGHT:
		_fail("the demo media is %dx%d; the track reports %dx%d"
			% [MEDIA_WIDTH, MEDIA_HEIGHT, video["width"], video["height"]])
		return
	if video["sar_num"] != MEDIA_SAR_NUM or video["sar_den"] != MEDIA_SAR_DEN:
		_fail("the demo media's pixel aspect ratio is %d:%d; the track reports %d:%d"
			% [MEDIA_SAR_NUM, MEDIA_SAR_DEN, video["sar_num"], video["sar_den"]])
		return
	# The constants are libvlc's values, and the defaults are what a file that declares
	# no rotation, no projection and no stereoscopy reports.
	if video["orientation"] != VLCTrack.ORIENT_TOP_LEFT:
		_fail("a plain video reports orientation %d, not ORIENT_TOP_LEFT" % video["orientation"])
		return
	if video["projection"] != VLCTrack.PROJECTION_RECTANGULAR:
		_fail("a plain video reports projection %d, not PROJECTION_RECTANGULAR"
			% video["projection"])
		return
	if video["multiview"] != VLCTrack.MULTIVIEW_2D:
		_fail("a plain video reports multiview %d, not MULTIVIEW_2D" % video["multiview"])
		return

	var audio := await _wait_for_info(VLCTrack.TYPE_AUDIO, "channels")
	if audio.is_empty():
		_fail("no audio track reported its channels within %d ms" % STARTUP_TIMEOUT_MS)
		return
	print("track_info: audio %s" % str(audio))
	if _missing(audio, AUDIO_KEYS) != "":
		_fail("an audio track's dictionary has no audio key")
		return
	if audio.has("width") or audio.has("encoding"):
		_fail("an audio track's dictionary carries a member its type does not name")
		return
	if audio["channels"] != MEDIA_CHANNELS or audio["rate"] != MEDIA_RATE:
		_fail("the demo media's audio is %d channels at %d Hz; the track reports %d at %d"
			% [MEDIA_CHANNELS, MEDIA_RATE, audio["channels"], audio["rate"]])
		return

	# The demo media has no subtitle, so its text tracklist is empty -- and an empty
	# tracklist is what libvlc answers with, not an error. Nothing here reads a text
	# track's dictionary; src/acceptance.rs covers the encoding that is in one.
	if not _tracks(VLCTrack.TYPE_TEXT).is_empty():
		_fail("the demo media reports a text track")
		return

	print("track_info: video %dx%d sar %d:%d pose %.1f/%.1f/%.1f/%.1f profile %d level %d"
		% [video["width"], video["height"], video["sar_num"], video["sar_den"],
			video["pose_yaw"], video["pose_pitch"], video["pose_roll"],
			video["pose_field_of_view"], video["profile"], video["level"]])
	print("track_info OK")
	quit(0)


# The tracks of one type, or an empty array. libvlc answers a null list when it has no
# track of that category, which is not an error.
func _tracks(track_type: int) -> Array:
	var list: VLCTrackList = player.get_tracklist(track_type, false)
	if list == null:
		return []
	return list.get_tracks()


# Waits until the first track of one type reports a non-zero value for one key.
#
# A track appears as soon as the input does, and libvlc fills the decoder's numbers in
# later -- as a *new* track, published as a new list, rather than by changing the one it
# already handed out. So a caller that wants them has to ask again, which is what this
# does; waiting on the first answer would read the zeroes.
func _wait_for_info(track_type: int, key: String) -> Dictionary:
	var deadline := Time.get_ticks_msec() + STARTUP_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		var tracks := _tracks(track_type)
		if not tracks.is_empty():
			var info: Dictionary = tracks[0].get_info()
			if info.has(key) and info[key] != 0:
				return info
		await process_frame
	return {}


func _missing(info: Dictionary, keys: Array) -> String:
	for key in keys:
		if not info.has(key):
			return key
	return ""


func _fail(what: String) -> void:
	push_error("track_info FAIL: %s" % what)
	quit(1)
