extends SceneTree

# The demo lays its interface out with pure functions, so the layout can be
# asserted without a window. Run it the way the tests beside it are run:
#
#   godot --headless --path demo --script res://tests/ui_scale.gd
#
# The densities are Android's six generalized tiers, which is what
# DisplayServer.screen_get_dpi returns on a device, plus a desktop and a Retina
# display for the two ways the display scale is reported. The rectangles are a
# notched phone in portrait, a punch-hole phone in landscape, a windowed demo on a
# desktop, and a platform that reports no safe area at all.

const UiScale := preload("res://ui_scale.gd")

const SCALE_CASES := [
	[96, 1.0, 1.0],   # a desktop: unchanged, the panel was authored at this size
	[160, 1.0, 1.0],  # Android mdpi, the baseline
	[240, 1.0, 1.5],  # hdpi
	[320, 1.0, 2.0],  # xhdpi
	[480, 1.0, 3.0],  # xxhdpi
	[560, 1.0, 3.0],  # what the phone this was written for reports
	[640, 1.0, 3.0],  # xxxhdpi, clamped so the panel still fits the screen
	[96, 2.0, 2.0],   # a Retina display reports its scale rather than its density
]

const INSET_CASES := [
	# name, window rect, safe area, expected insets
	["a notch at the top", Rect2i(0, 0, 1080, 2400), Rect2i(0, 90, 1080, 2400), Vector2i(0, 90)],
	["a punch-hole on the left", Rect2i(0, 0, 2400, 1080), Rect2i(60, 0, 2400, 1080), Vector2i(60, 0)],
	["a windowed demo inside the safe area", Rect2i(300, 200, 1280, 800), Rect2i(0, 0, 2560, 1400), Vector2i(0, 0)],
	["a window partly off the safe area", Rect2i(-50, -20, 1280, 800), Rect2i(0, 0, 2560, 1400), Vector2i(50, 20)],
	["a platform reporting none", Rect2i(0, 0, 1080, 2400), Rect2i(0, 0, 0, 0), Vector2i(0, 0)],
]

var _failures := 0


func _init() -> void:
	for case in SCALE_CASES:
		_expect_float("scale at dpi %d, display scale %.1f" % [case[0], case[1]],
			UiScale.scale_for(case[0], case[1]), case[2])

	for case in INSET_CASES:
		_expect_vector("insets for %s" % case[0],
			Vector2(UiScale.safe_insets(case[1], case[2])), Vector2(case[3]))

	# Only the width caps the scale: the frame scrolls vertically, so a panel taller
	# than the screen is expected rather than something to shrink away from.
	var phone_width := 1080.0 - 160.0
	_expect_float("a phone is wide enough for the panel at its density's scale",
		UiScale.fit_to_width(3.0, 201.0, phone_width), 3.0)
	_expect_float("a window narrower than the panel caps the scale",
		UiScale.fit_to_width(3.0, 201.0, 320.0), 320.0 / 201.0)
	_expect_float("a desktop window does not cap the scale",
		UiScale.fit_to_width(1.0, 201.0, 1280.0), 1.0)

	# The frame: a notched phone in portrait, at 3x, with a 201-unit panel. The
	# 90 px notch is 30 units of a 3x screen, and the padding sits outside that.
	var frame := UiScale.frame_rect_for(Vector2i(0, 90), Vector2i(1080, 2400), 201.0, 3.0)
	_expect_vector("the frame's corner, in units",
		frame.position, Vector2(0.0 + UiScale.SAFE_AREA_PADDING, 30.0 + UiScale.SAFE_AREA_PADDING))
	_expect_vector("the frame is as wide as the panel and as tall as what is left",
		frame.size, Vector2(201.0, (2400.0 - 90.0) / 3.0 - UiScale.SAFE_AREA_PADDING * 2.0))

	if _failures > 0:
		quit(1)
		return

	print("ui_scale: OK (%d cases)" % (SCALE_CASES.size() + INSET_CASES.size() + 5))
	quit(0)


func _expect_float(what: String, got: float, want: float) -> void:
	if not is_equal_approx(got, want):
		push_error("ui_scale FAIL: %s -> %.4f, expected %.4f" % [what, got, want])
		_failures += 1


func _expect_vector(what: String, got: Vector2, want: Vector2) -> void:
	if not got.is_equal_approx(want):
		push_error("ui_scale FAIL: %s -> %v, expected %v" % [what, got, want])
		_failures += 1
