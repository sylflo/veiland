+++
title = "wifi"
description = "Wi-Fi signal-strength glyph in a small pill, with an optional SSID label. Live from NetworkManager; bucketed into five strength levels plus an off state."
weight = 56
template = "docs-page.html"

[extra]
binary = "veiland-wifi"
one_liner = "Wi-Fi status pill"
category = "widgets"
example = "wifi.toml"

[[extra.props]]
key = "pill_color"
type = "[r,g,b,a]"
default = "`[0.059, 0.071, 0.11, 0.686]`"
meaning = "Chip background behind the glyph. Alpha is the opacity; `[0, 0, 0, 0]` draws no chip (bare glyph)."

[[extra.props]]
key = "icon_color"
type = "[r,g,b,a]"
default = "white"
meaning = "Tints the monochrome glyph. Omit for the glyph's authored white."

[[extra.props]]
key = "show_label"
type = "bool"
default = "`false`"
meaning = "Show the SSID next to the glyph. Off = icon-only pill."

[[extra.props]]
key = "label_color"
type = "[r,g,b,a]"
default = "`[0.95, 0.95, 0.95, 1.0]`"
meaning = "SSID text color. Only the RGB is used; alpha does not hide it (use `show_label = false` for that)."

[[extra.props]]
key = "label_pos"
type = "string"
default = "`\"bottom\"`"
meaning = "Where the label sits relative to the glyph: `top`, `bottom`, `left`, or `right`."

[[extra.props]]
key = "label_disconnected"
type = "string"
default = "`\"N/A\"`"
meaning = "Label text when there is no SSID (disconnected, radio off, or no device). Set to `\"\"` to vanish the label and leave just the glyph."
+++

The glyph reflects live signal strength from NetworkManager, bucketed to five levels
(0/25/50/75/100 %) plus an off/disconnected state. Data is read read-only over the system
D-Bus; the widget never touches your credentials or the connection itself.

This is one of the four **status pills** &mdash; see the [widgets](@/docs/plugins/_index.md)
overview for the shared `pill_color` / `icon_color` look and the `content_halign` /
`content_valign` / `debug_border` / font keys they all accept. Cluster several pills in one
row with the [`status_cluster.toml`](https://github.com/sylflo/veiland/blob/master/docs/examples/status_cluster.toml)
scene.
