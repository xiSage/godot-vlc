extends Node

## Lays this demo's interface out for the screen it is running on: how big it
## should be, how far from the edges it has to stay, and -- because the two
## interact -- how big it can be and still fit.
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
## Then position, because a phone draws edge to edge and its rounded corners and
## camera cutout sit over the top-left corner this panel is anchored to, which
## leaves the first buttons partly behind them.
## `DisplayServer.get_display_safe_area()` is the unobscured rectangle of the
## display, in screen pixels, and the panel is placed inside it. Android and iOS
## implement it; elsewhere it falls back to the screen's usable rectangle, which a
## windowed demo already sits inside, so the offset comes out as the padding alone.
##
## Then fit, because the insets are in pixels and the panel is in units: they are
## taken off the window before the scale is capped to what is left, which is what
## keeps the panel on screen in landscape, where a phone is shorter than the panel
## is tall at the scale its density asked for.
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

## Air between the panel and the safe area, in units. The cutout API reports the
## camera but not the display's rounded corners -- Android keeps those to itself --
## so the panel is kept off the boundary rather than flush against it.
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


## Caps the scale so that the panel still fits the space the safe area left.
static func fit_to_window(scale: float, panel_size: Vector2, available: Vector2) -> float:
	# A panel or a window with no size is not something to divide by; the layout is
	# recomputed as soon as either has one.
	if panel_size.x <= 0.0 or panel_size.y <= 0.0 or available.x <= 0.0 or available.y <= 0.0:
		return scale
	return maxf(MIN_SCALE, minf(scale, minf(available.x / panel_size.x, available.y / panel_size.y)))


## Where the panel's top-left corner belongs, in content units, from insets in
## screen pixels.
static func panel_offset_for(insets: Vector2i, scale_factor: float) -> Vector2:
	# Divided by the scale because the insets arrive in pixels while the panel is
	# positioned in units: at 3x a 90 px cutout is 30 units, and the padding is what
	# one unit of air costs on that screen.
	return Vector2(insets) / maxf(scale_factor, 0.001) + Vector2(SAFE_AREA_PADDING, SAFE_AREA_PADDING)


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

	var factor := scale_for(dpi, DisplayServer.screen_get_scale(screen))
	var panel := get_node_or_null("%VBoxContainer") as Control
	if panel:
		# The insets are pixels and do not depend on the scale, so they come off the
		# window first and the remaining space is what the scale is fitted to.
		factor = fit_to_window(factor, panel.size, Vector2(window.size - insets))

	_applied_screen = screen
	_applied_dpi = dpi
	_applied_window = window.size
	window.content_scale_factor = factor

	if panel:
		panel.position = panel_offset_for(insets, factor)

	var label := get_node_or_null("%UIScale") as Label
	if label:
		label.text = "ui scale: %.2f (dpi %d, safe %d,%d)" % [factor, dpi, insets.x, insets.y]
