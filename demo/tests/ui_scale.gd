extends SceneTree

# The scale the demo gives its interface is a pure function of the screen, so it
# can be asserted without a window. Run it the way the other tests in this
# directory are run:
#
#   godot --headless --path demo --script res://tests/ui_scale.gd
#
# The densities are Android's six generalized tiers, which is what
# DisplayServer.screen_get_dpi returns on a device, plus a desktop and a Retina
# display for the two ways the display scale is reported.

const UiScale := preload("res://ui_scale.gd")

const CASES := [
	[96, 1.0, 1.0],   # a desktop: unchanged, the panel was authored at this size
	[160, 1.0, 1.0],  # Android mdpi, the baseline
	[240, 1.0, 1.5],  # hdpi
	[320, 1.0, 2.0],  # xhdpi
	[480, 1.0, 3.0],  # xxhdpi
	[640, 1.0, 3.0],  # xxxhdpi, clamped so the panel still fits the screen
	[96, 2.0, 2.0],   # a Retina display reports its scale rather than its density
]


func _init() -> void:
	var failures := 0
	for case in CASES:
		var got: float = UiScale.scale_for(case[0], case[1])
		if not is_equal_approx(got, case[2]):
			push_error("ui_scale FAIL: dpi %d, display scale %.1f -> %.3f, expected %.3f"
				% [case[0], case[1], got, case[2]])
			failures += 1

	if failures > 0:
		quit(1)
		return

	print("ui_scale: OK (%d cases)" % CASES.size())
	quit(0)
