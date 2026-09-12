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

	# Inset in units plus the padding: 90 px at 3x is 30 units.
	_expect_vector("the panel offset at 3x",
		UiScale.panel_offset_for(Vector2i(90, 0), 3.0),
		Vector2(30.0 + UiScale.SAFE_AREA_PADDING, 0.0 + UiScale.SAFE_AREA_PADDING))

	# A phone on its side is shorter than the panel is tall at the scale its density
	# asked for, so the scale is capped to what the screen leaves after the cutout.
	var landscape := UiScale.fit_to_window(3.0, Vector2(201, 423), Vector2(2720, 1204))
	_expect_float("the scale fitted to a phone in landscape", landscape, 1204.0 / 423.0)

	# And a desktop window is roomy enough that fitting changes nothing.
	_expect_float("the scale fitted to a desktop window",
		UiScale.fit_to_window(1.0, Vector2(201, 423), Vector2(1280, 800)), 1.0)

	if _failures > 0:
		quit(1)
		return

	print("ui_scale: OK (%d cases)" % (SCALE_CASES.size() + INSET_CASES.size() + 3))
	quit(0)


func _expect_float(what: String, got: float, want: float) -> void:
	if not is_equal_approx(got, want):
		push_error("ui_scale FAIL: %s -> %.4f, expected %.4f" % [what, got, want])
		_failures += 1


func _expect_vector(what: String, got: Vector2, want: Vector2) -> void:
	if not got.is_equal_approx(want):
		push_error("ui_scale FAIL: %s -> %v, expected %v" % [what, got, want])
		_failures += 1
