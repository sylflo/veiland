+++
title = "ethernet"
description = "Wired-link status glyph in a small pill: up or down. Live from NetworkManager over the system D-Bus. Icon-only, no label."
weight = 57
template = "docs-page.html"

[extra]
binary = "veiland-ethernet"
one_liner = "wired link status pill"
category = "widgets"
example = "ethernet.toml"

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
+++

The glyph shows whether the wired link is up or down, live from NetworkManager. This is the
simplest of the [status pills](@/docs/plugins/_index.md): it draws no text, so it
reads `pill_color`, `icon_color`, and the anchor / `debug_border` keys, but no label or
font keys.
