extends Node

## Sizes this demo's interface for the screen it is running on.
##
## Godot measures UI in pixels, and an Android handset at 480 dpi packs three
## times as many of them per inch as the 160 dpi the platform treats as its
## baseline. A panel that is a comfortable size on a desktop is therefore a third
## of that on a phone, which is the difference between a button you can hit and
## one you cannot. Godot's multiple-resolutions guidance asks the same thing of a
## non-game application, which is what this is: keep the `disabled` stretch mode
## -- already the project's default -- so the video keeps the window's real
## resolution, and scale the interface in units.
##
## The knob is `content_scale_factor` on the root Window, which is what the
## documentation recommends for scaling 2D content at runtime;
## `gui/theme/default_theme_scale` is only read when the project initializes.
##
## The dialogs are deliberately left alone. Scaling a Window's content means
## scaling its size with it, and the MRL dialog's is 600 units wide, so at three
## times that it no longer fits the width of the phone this exists for. A native
## file dialog is drawn by the OS, which applies its own DPI handling anyway.

## Android's mdpi, at which one unit is one pixel.
const REFERENCE_DPI := 160.0

## The panel is 201 units wide, so past this it stops fitting a 720 px screen.
const MAX_SCALE := 3.0

## Never shrink the interface below the size it was authored at: a desktop at
## 96 dpi works out below 1.0, and changing what those users see is not the point.
const MIN_SCALE := 1.0

## The (screen, dpi) pair the current scale was computed from.
var _applied := Vector2i(-1, -1)


## The scale to draw the interface at, from a screen's density and the display
## scale it reports. Static and pure so that demo/tests/ui_scale.gd can assert it
## without a window.
static func scale_for(dpi: int, display_scale: float) -> float:
	# The larger of the two, because they answer different questions: on macOS and
	# on Android the display scale is the authoritative UI factor, while on an X11
	# desktop it stays 1.0 and the density is all there is to go on.
	return clampf(maxf(display_scale, float(dpi) / REFERENCE_DPI), MIN_SCALE, MAX_SCALE)


func _ready() -> void:
	_apply()


func _process(_delta: float) -> void:
	# Checked every frame because it is how a window dragged to a denser monitor,
	# or a phone turned on its side, is noticed at all: Godot sends no signal for
	# either, and both come down to a change in what the screen reports.
	var screen := get_window().current_screen
	if Vector2i(screen, DisplayServer.screen_get_dpi(screen)) == _applied:
		return
	_apply()


func _apply() -> void:
	var window := get_window()
	var screen := window.current_screen
	var dpi := DisplayServer.screen_get_dpi(screen)
	var factor := scale_for(dpi, DisplayServer.screen_get_scale(screen))

	_applied = Vector2i(screen, dpi)
	window.content_scale_factor = factor

	var label := get_node_or_null("%UIScale") as Label
	if label:
		label.text = "ui scale: %.2f (dpi %d)" % [factor, dpi]
