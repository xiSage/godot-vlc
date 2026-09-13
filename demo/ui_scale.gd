extends Node

## Lays this demo's interface out for the screen it is running on: how big it
## should be, how far from the edges it has to stay, and how much of it is on
## screen at once.
##
## Size first. Godot measures UI in pixels, and an Android handset at 560 dpi packs
## three and a half times as many of them per inch as the 160 dpi the platform
## treats as its baseline, so a panel that is comfortable on a desktop is a third of
## that in the hand. Godot's multiple-resolutions guidance asks the same thing of a
## non-game application, which this is: keep the `disabled` stretch mode -- already
## the project's default -- so the video keeps the window's real resolution, and
## scale the interface in units. The knob is `content_scale_factor` on the root
## Window, which is what the documentation recommends for 2D content at runtime;
## `gui/theme/default_theme_scale` is only read when the project initializes.
##
## Position next, because a phone draws edge to edge and its rounded corners and
## camera cutout sit over the top-left corner this panel is anchored to, which
## leaves the first buttons partly behind them.
## `DisplayServer.get_display_safe_area()` is the unobscured rectangle of the
## display, in screen pixels, and the frame is placed inside it. Android and iOS
## implement it; elsewhere it falls back to the screen's usable rectangle, which a
## windowed demo already sits inside, so the offset comes out as the padding alone.
##
## Frame last. At the scale a phone's density asks for, the panel is taller than the
## screen, so it scrolls inside a ScrollContainer rather than being shrunk to fit:
## shrinking it would undo the reason it is scaled at all, and a readable panel that
## scrolls is worth more than a small one that fits. Only the width still caps the
## scale, so that the panel is never wider than the screen.
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

## Air between the frame and the safe area, in units. The cutout API reports the
## camera but not the display's rounded corners -- Android keeps those to itself --
## so the frame is kept off the boundary rather than flush against it.
const SAFE_AREA_PADDING := 8.0

## What the current layout was computed from.
var _applied_screen := -1
var _applied_dpi := -1
var _applied_window := Vector2i.ZERO


## The scale to draw the interface at, from a screen's density and the display
## scale it reports. Static and pure so that demo/tests/ui_scale.gd can assert it
## without a window.
static func scale_for(dpi: int, display_scale: float) -> float:
	# The larger of the two, because they answer different questions: on macOS and
	# on Android the display scale is the authoritative UI factor, while on an X11
	# desktop it stays 1.0 and the density is all there is to go on.
	return clampf(maxf(display_scale, float(dpi) / REFERENCE_DPI), MIN_SCALE, MAX_SCALE)


## The left and top parts of the window that the display's safe area does not
## cover, in screen pixels: what a cutout, a notch or a status bar can hide.
##
## Only the left and top are computed because that is the corner the panel is
## anchored to; a cutout on the far side of the screen cannot hide it. Pure, for
## the same reason as scale_for.
static func safe_insets(window_rect: Rect2i, safe_area: Rect2i) -> Vector2i:
	# Clamped at zero: a window that is already inside the safe area, or a platform
	# reporting no safe area at all, must not push the panel off the screen.
	return Vector2i(
		maxi(safe_area.position.x - window_rect.position.x, 0),
		maxi(safe_area.position.y - window_rect.position.y, 0))


## Caps the scale so that the panel is no wider than the window. The two widths are
## in different units on purpose -- the panel's in units, the window's in pixels --
## because the comparison that matters is between the window and the panel as drawn,
## which is the panel's width times this scale.
static func fit_to_width(scale: float, content_width: float, available_width: float) -> float:
	if content_width <= 0.0 or available_width <= 0.0:
		return scale
	return maxf(MIN_SCALE, minf(scale, available_width / content_width))


## The rectangle the interface scrolls inside, in content units: from the safe
## area's corner to the bottom of the window, as wide as the panel needs and as tall
## as what is left. Pure, like the rest of the layout.
static func frame_rect_for(insets: Vector2i, window_size: Vector2i, content_width: float, scale_factor: float) -> Rect2:
	var unit := 1.0 / maxf(scale_factor, 0.001)
	var padding := Vector2(SAFE_AREA_PADDING, SAFE_AREA_PADDING)
	var origin := Vector2(insets) * unit + padding
	# The insets are pixels and do not depend on the scale, so the room that is left
	# for the frame is the window minus them; the height is whatever remains, which
	# is what makes the panel scroll rather than overflow.
	var room := (Vector2(window_size) - Vector2(insets)) * unit - padding * 2.0
	return Rect2(origin, Vector2(maxf(content_width, 0.0), maxf(room.y, 0.0)))


func _ready() -> void:
	_apply()


func _process(_delta: float) -> void:
	# Checked every frame because it is how a window dragged to a denser monitor, or
	# a phone turned on its side, is noticed at all: Godot sends no signal for
	# either, and both come down to a change in what the screen reports. The window
	# size is part of the key because turning a phone around moves the safe area
	# without necessarily changing the density.
	var window := get_window()
	var dpi := DisplayServer.screen_get_dpi(window.current_screen)
	if window.current_screen == _applied_screen and dpi == _applied_dpi and window.size == _applied_window:
		return
	_apply()


func _apply() -> void:
	var window := get_window()
	var screen := window.current_screen
	var dpi := DisplayServer.screen_get_dpi(screen)
	var window_rect := Rect2i(window.position, window.size)
	var insets := safe_insets(window_rect, DisplayServer.get_display_safe_area())

	var content := get_node_or_null("%VBoxContainer") as Control
	var content_width := content.get_combined_minimum_size().x if content else 0.0

	var factor := scale_for(dpi, DisplayServer.screen_get_scale(screen))
	if content_width > 0.0:
		factor = fit_to_width(factor, content_width, float(window.size.x - insets.x))

	_applied_screen = screen
	_applied_dpi = dpi
	_applied_window = window.size
	window.content_scale_factor = factor

	var frame := get_node_or_null("%ScrollContainer") as ScrollContainer
	if frame and content_width > 0.0:
		var rect := frame_rect_for(insets, window.size, content_width, factor)
		frame.position = rect.position
		frame.size = rect.size

	var label := get_node_or_null("%UIScale") as Label
	if label:
		label.text = "ui scale: %.2f (dpi %d, safe %d,%d)" % [factor, dpi, insets.x, insets.y]

	# Printed as well as shown, because this is the state a screenshot or a bug
	# report has to quote and a log is easier to quote than a picture.
	print("ui scale: %.2f dpi %d safe %d,%d frame %.0fx%.0f at %.0f,%.0f" % [
		factor, dpi, insets.x, insets.y, frame.size.x, frame.size.y, frame.position.x, frame.position.y])
