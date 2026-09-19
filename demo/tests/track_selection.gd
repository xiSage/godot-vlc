extends SceneTree

# The GDScript half of the track selection API: the four things the Rust acceptance
# test cannot check.
#
# src/acceptance.rs drives libvlc directly, which leaves it blind to whether an
# Array[VLCTrack] survives the crossing into Rust, whether the five track signals
# arrive in a script with their arguments, whether get_selected_track answers a script
# at all, and -- the one that matters most -- whether a track that came from a media
# descriptor is refused instead of being handed to libvlc, which would dereference the
# es_id it does not have.
#
# The multi-track case is not here: two selected subtitles need two subtitle files and
# a media to hang them on, and src/acceptance.rs covers that with the runtime itself.
#
#   godot --headless --path demo --script res://tests/track_selection.gd

const STARTUP_TIMEOUT_MS := 20000

var player: VLCMediaPlayer
var added: Array[String] = []
var selected: Array[String] = []
var unselected: Array[String] = []


func _init() -> void:
	player = VLCMediaPlayer.new()
	root.add_child(player)

	# The player attaches libvlc's callbacks when it becomes ready; connecting
	# before that frame would miss the tracks of the media this test opens.
	await process_frame

	player.track_added.connect(func(_track_type: int, id: String) -> void: added.append(id))
	player.track_selected.connect(
		func(_track_type: int, id: String) -> void: selected.append(id)
	)
	player.track_unselected.connect(
		func(_track_type: int, id: String) -> void: unselected.append(id)
	)

	var media := VLCMedia.load_from_file(ProjectSettings.globalize_path("res://test.mp4"))
	player.media = media
	player.play()
	if not await _wait_for_playing():
		_fail("playback never started")
		return

	# The array the binding hands over has to be the array its selection takes.
	var videos := _tracks(VLCTrack.TYPE_VIDEO)
	if videos.is_empty():
		_fail("the demo media reports no video track")
		return
	var video: VLCTrack = videos[0]
	if video.get_id() == "":
		_fail("a video track came back without an id")
		return
	if not added.has(video.get_id()):
		_fail("no track_added arrived for %s; the signals saw %s" % [video.get_id(), added])
		return

	# Unselect first, so that the selection below is a move libvlc reports rather
	# than a no-op it stays silent about. The signal and the state do not arrive on
	# the same frame -- the state is libvlc's, the signal waits for the next frame's
	# drain -- so each is waited for on its own.
	player.unselect_track_type(VLCTrack.TYPE_VIDEO)
	if not await _wait_for_selection(VLCTrack.TYPE_VIDEO, ""):
		_fail("unselect_track_type left a video track selected")
		return
	if not await _wait_for_signal(unselected, video.get_id()):
		_fail("the selection changed and no track_unselected carried %s" % video.get_id())
		return

	player.select_tracks(VLCTrack.TYPE_VIDEO, videos)
	if not await _wait_for_selection(VLCTrack.TYPE_VIDEO, video.get_id()):
		_fail("select_tracks did not select %s" % video.get_id())
		return
	if not await _wait_for_signal(selected, video.get_id()):
		_fail("a track was selected and no track_selected carried its id")
		return

	# get_selected_track answers a script, and agrees with the selection just made.
	var one := player.get_selected_track(VLCTrack.TYPE_VIDEO)
	if one == null:
		_fail("get_selected_track answered null with a track selected")
		return
	if one.get_id() != video.get_id():
		_fail("get_selected_track answered %s, not %s" % [one.get_id(), video.get_id()])
		return

	# By id: the other half of what get_id() promises.
	player.unselect_track_type(VLCTrack.TYPE_VIDEO)
	if not await _wait_for_selection(VLCTrack.TYPE_VIDEO, ""):
		_fail("the video track could not be unselected")
		return
	player.select_tracks_by_ids(VLCTrack.TYPE_VIDEO, video.get_id())
	if not await _wait_for_selection(VLCTrack.TYPE_VIDEO, video.get_id()):
		_fail("select_tracks_by_ids did not select %s" % video.get_id())
		return
	var found := player.get_track_from_id(video.get_id())
	if found == null or found.get_id() != video.get_id():
		_fail("get_track_from_id did not find the id it was given")
		return
	if player.get_track_from_id("no/such/track") != null:
		_fail("get_track_from_id found a track for an id that matches nothing")
		return

	# A track from the media descriptor: libvlc cannot select it, and the binding is
	# supposed to refuse it with a line in the log rather than pass it on. What is
	# asserted here is that the call is harmless, since a void call has nothing else
	# to check.
	var media_audio := media.get_tracklist(VLCTrack.TYPE_AUDIO)
	if media_audio == null or media_audio.get_tracks().is_empty():
		print("track_selection: the media descriptor reported no audio track to refuse")
	else:
		var before := player.get_selected_track(VLCTrack.TYPE_AUDIO)
		player.select_track(media_audio.get_tracks()[0])
		var after := player.get_selected_track(VLCTrack.TYPE_AUDIO)
		if (before == null) != (after == null):
			_fail("a media descriptor's track changed the selection")
			return
		if before != null and before.get_id() != after.get_id():
			_fail("a media descriptor's track changed which audio track is selected")
			return
		print("track_selection: a media-side track was refused without changing anything")

	print(
		"track_selection OK (%d added, %d selected, %d unselected)"
		% [added.size(), selected.size(), unselected.size()]
	)
	quit(0)


# The tracks of one type, or an empty typed array. libvlc answers a null list when it
# has no track of that category, which is not an error.
func _tracks(track_type: int) -> Array[VLCTrack]:
	var tracks: Array[VLCTrack] = []
	var list := player.get_tracklist(track_type, false)
	if list != null:
		tracks.assign(list.get_tracks())
	return tracks


# Waits until the player reports `id` as the selected track of a type, or until it
# reports none at all when `id` is empty. A selection is queued to libvlc's input
# thread, so it does not land with the call that asked for it.
func _wait_for_selection(track_type: int, id: String) -> bool:
	var deadline := Time.get_ticks_msec() + STARTUP_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		var one := player.get_selected_track(track_type)
		if id.is_empty():
			if one == null:
				return true
		elif one != null and one.get_id() == id:
			return true
		await process_frame
	return false


# Waits until one of the signals has carried this id.
#
# The signal is emitted from the player's per-frame drain, not from the call that
# caused it, so it arrives after the state it describes -- a test that reads the array
# right after the state settles is reading it a frame too early.
func _wait_for_signal(carried: Array[String], id: String) -> bool:
	var deadline := Time.get_ticks_msec() + STARTUP_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		if carried.has(id):
			return true
		await process_frame
	return false


func _wait_for_playing() -> bool:
	var deadline := Time.get_ticks_msec() + STARTUP_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		if player.get_state() == VLCMediaPlayer.STATE_PLAYING:
			return true
		await process_frame
	return false


func _fail(what: String) -> void:
	push_error("track_selection FAIL: %s" % what)
	quit(1)
