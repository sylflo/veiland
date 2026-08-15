+++
title = "bluetooth"
description = "Bluetooth status glyph in a small pill (off / on / connected), with an optional connected-device label. Live from bluez over the system D-Bus."
weight = 58
template = "docs-page.html"

[extra]
binary = "veiland-bluetooth"
one_liner = "Bluetooth status pill"
category = "widgets"
example = "bluetooth.toml"

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
meaning = "Show the connected device name. Off = icon-only pill."

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

[[extra.props]]
key = "label_disconnected"
type = "string"
default = "`\"N/A\"`"
meaning = "Label text when nothing is connected. Set to `\"\"` for glyph-only."
+++

The glyph has three states &mdash; off, on-but-idle, and connected &mdash; read live from
bluez. With `show_label` on, a connected device's name appears beside it. See the
[widgets](@/docs/plugins/_index.md) overview for the shared `pill_color` /
`icon_color`, anchor, font, and `debug_border` keys.
