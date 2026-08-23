+++
title = "battery"
description = "Battery status drawn from bucketed SVG icons in a small pill, with an optional percent + charging-state label. Reads /sys/class/power_supply. The template for the status-icon pattern."
weight = 59
template = "docs-page.html"

[extra]
binary = "veiland-battery"
one_liner = "battery status pill"
category = "widgets"
image = "previews/battery.png"
example = "battery_svg.toml"

[[extra.props]]
key = "pill_color"
type = "[r,g,b,a]"
default = "`[0.059, 0.071, 0.11, 0.686]`"
meaning = "Chip background behind the glyph. `[0, 0, 0, 0]` draws no chip."

[[extra.props]]
key = "icon_color"
type = "[r,g,b,a]"
default = "white"
meaning = "Tints the glyph. Omit for white."

[[extra.props]]
key = "show_label"
type = "bool"
default = "`false`"
meaning = "Show the percent + state text (e.g. `64% Discharging`, or `AC` on a desktop with no battery). Off = icon-only pill."

[[extra.props]]
key = "label_color"
type = "[r,g,b,a]"
default = "`[0.95, 0.95, 0.95, 1.0]`"
meaning = "Label text color (RGB only; alpha does not hide it)."

[[extra.props]]
key = "label_pos"
type = "string"
default = "`\"bottom\"`"
meaning = "Label position relative to the glyph: `top`, `bottom`, `left`, or `right`."
+++

The glyph is chosen from bucketed SVG icons by charge level and charging state, read from
`/sys/class/power_supply`. A machine with no battery shows the "AC" glyph. This widget is
also the copy-me starting point for writing your own status-icon pill.

See the [widgets](@/docs/plugins/_index.md) overview for the shared `pill_color` /
`icon_color`, anchor, font, and `debug_border` keys.
