extends SceneTree

# The error signal is the only report of a failure that happens after play():
# play() returns 0 for a media it cannot open, and libvlc's error state is not
# one a caller can observe -- a failed open goes through the same
# stopping/stopped as a media that ended. This drives that failure end to end,
# through the binding's attach and its parked events, which the Rust acceptance
# test cannot: that one drives LibVLC directly.
#
# The wait is counted in milliseconds rather than in frames, because a missing
# local file is reported in about 140ms and a headless loop runs faster than
# sixty frames a second.

const MISSING_MRL := "file:///definitely-not-a-directory/this-media-does-not-exist.mp4"
const REPORT_TIMEOUT_MS := 10000

# A member rather than a local: a GDScript lambda captures a local by value, so
# a local counter would stay at zero while the lambda counted.
var reported := 0


func _init() -> void:
	var player := VLCMediaPlayer.new()
	root.add_child(player)

	# The player attaches libvlc's callbacks when it becomes ready, and a play()
	# before that starts the input with nothing listening: this failure lands
	# within a frame of the call, and the report would be lost. One frame is all
	# it takes, and autoplay waits for the same reason.
	await process_frame

	var media := VLCMedia.load_from_mrl(MISSING_MRL)
	if media == null:
		push_error("error_signal FAIL: the MRL was rejected before playback, so nothing was tested")
		quit(1)
		return

	player.error.connect(func() -> void: reported += 1)
	player.media = media
	player.play()

	var deadline := Time.get_ticks_msec() + REPORT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline and reported == 0:
		await process_frame

	if reported == 0:
		push_error("error_signal FAIL: no error signal within %dms for a media that cannot be opened" % REPORT_TIMEOUT_MS)
		quit(1)
		return

	if reported > 1:
		push_error("error_signal FAIL: %d error signals for one failed input; libvlc raises it once" % reported)
		quit(1)
		return

	print("error_signal OK: the failure was reported once")
	quit(0)
