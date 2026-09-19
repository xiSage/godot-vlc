extends SceneTree

# §3.1 through the engine: libvlc's playback-time watcher.
#
# The Rust acceptance covers the runtime's half -- where the points come from, one
# watcher per player, the two calls one seek makes, and the arithmetic libvlc's
# interpolate refuses. What only this can cover is the binding's half: the four signals
# arriving with their five arguments, the two guards that exist because libvlc would
# abort or crash rather than answer (a negative period, and an unwatch with nothing to
# unwatch), and the clock a script has to use for its own arithmetic.
#
#   godot --headless --path demo --script res://tests/time_point.gd

const WAIT_TIMEOUT_MS := 20000
const POINTS_WANTED := 8

var points: Array = []
var pauses: Array = []
var seeks: Array = []
var seek_finished := 0

var player: VLCMediaPlayer


func _init() -> void:
	player = VLCMediaPlayer.new()
	root.add_child(player)
	await process_frame

	# libvlc's own clock, which is what a script needs to compare an interpolated time
	# with a later one: Godot's microsecond clock is on a different origin.
	var clock_a := VLCInstance.get_clock_us()
	var clock_b := VLCInstance.get_clock_us()
	if clock_b < clock_a or clock_a <= 0:
		_fail("libvlc's clock went from %d to %d" % [clock_a, clock_b])
		return
	print("time_point: libvlc's clock reads %d us and does not go backwards" % clock_b)

	# A negative period would abort the process inside libvlc, so the binding refuses it
	# here; what matters is that the call answers and the script keeps running.
	if player.watch_time(-1) != -1:
		_fail("a negative min_period was accepted")
		return
	if player.is_watching_time():
		_fail("the refused call left a watcher registered")
		return

	if player.watch_time(0) != 0:
		_fail("watching the clock was refused")
		return
	if not player.is_watching_time():
		_fail("watch_time answered 0 but is_watching_time says otherwise")
		return
	# libvlc allows one watcher: the binding answers the second one itself, so that the
	# caller does not also get libvlc's own complaint in the log.
	if player.watch_time(0) != -1:
		_fail("a second watcher was accepted")
		return

	player.time_point.connect(
		func(ts_us: int, position: float, rate: float, length_us: int, system_date_us: int) -> void:
			points.append({
				"ts_us": ts_us,
				"position": position,
				"rate": rate,
				"length_us": length_us,
				"system_date_us": system_date_us,
			})
	)
	player.time_point_paused.connect(func(system_date_us: int) -> void: pauses.append(system_date_us))
	player.time_point_seek.connect(
		func(ts_us: int, _position: float, _rate: float, _length_us: int, _system_date_us: int) -> void:
			seeks.append(ts_us)
	)
	player.time_point_seek_finished.connect(func() -> void: seek_finished += 1)

	player.media = VLCMedia.load_from_file("res://test.mp4")
	player.play()
	if not await _wait_for_state(VLCMediaPlayer.STATE_PLAYING):
		_fail("playback never started")
		return

	var deadline := Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline and points.size() < POINTS_WANTED:
		await process_frame
	if points.size() < POINTS_WANTED:
		_fail("only %d time points arrived in %dms" % [points.size(), WAIT_TIMEOUT_MS])
		return
	print("time_point: %d points, the first is %s" % [points.size(), str(points[0])])

	for point in points:
		if point["ts_us"] < -1 or point["position"] < 0.0 or point["position"] > 1.0:
			_fail("a point is out of range: %s" % str(point))
			return
		if point["rate"] <= 0.0:
			_fail("a point reports rate %f" % point["rate"])
			return

	# The interpolated clock is what a game reads every frame: it moves between the
	# points libvlc reports, and it is read against libvlc's own clock.
	var first: Dictionary = player.interpolate_time_point()
	var second: Dictionary = player.interpolate_time_point()
	if first["ts_us"] < 0:
		_fail("nothing to interpolate from while playing: %s" % str(first))
		return
	if second["ts_us"] < first["ts_us"]:
		_fail("the interpolated clock went backwards: %d then %d" % [first["ts_us"], second["ts_us"]])
		return
	print("time_point: interpolate answered %d us, then %d us" % [first["ts_us"], second["ts_us"]])

	# A pause is reported with a system date, and it is one of the two reasons libvlc
	# sends that callback: the other is a stop, which sends 0.
	player.set_pause(true)
	var paused_deadline := Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < paused_deadline and pauses.is_empty():
		await process_frame
	if pauses.is_empty():
		_fail("no time_point_paused arrived after pausing")
		return
	if pauses[0] <= 0:
		_fail("a pause reported the system date %d, which is libvlc's 'stopping' value" % pauses[0])
		return
	print("time_point: pausing reported the system date %d" % pauses[0])

	player.set_pause(false)
	if not await _wait_for_state(VLCMediaPlayer.STATE_PLAYING):
		_fail("playback never resumed")
		return

	# One seek, two signals: the point it was asked for, then the end of it.
	player.set_time(10000, false)
	var seek_deadline := Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < seek_deadline and seek_finished == 0:
		await process_frame
	if seeks.is_empty() or seek_finished == 0:
		_fail("a seek reported %d points and %d finishes" % [seeks.size(), seek_finished])
		return
	print("time_point: the seek reported %d us and then finished" % seeks[0])

	# Taking the watcher off stops the points, and asking twice does not take the
	# process down: libvlc's own unwatch would walk a null pointer there.
	player.unwatch_time()
	if player.is_watching_time():
		_fail("the watcher is still registered after unwatch_time")
		return
	var settled := points.size()
	var quiet_deadline := Time.get_ticks_msec() + 500
	while Time.get_ticks_msec() < quiet_deadline:
		await process_frame
	if points.size() != settled:
		_fail("%d more points arrived after the watcher was taken off" % (points.size() - settled))
		return
	player.unwatch_time()

	print("time_point OK: five-argument points, an interpolated clock, one seek in two signals, and both guards")
	quit(0)


func _wait_for_state(wanted: int) -> bool:
	var deadline := Time.get_ticks_msec() + WAIT_TIMEOUT_MS
	while Time.get_ticks_msec() < deadline:
		if player.get_state() == wanted:
			return true
		await process_frame
	return false


func _fail(what: String) -> void:
	push_error("time_point FAIL: %s" % what)
	quit(1)
