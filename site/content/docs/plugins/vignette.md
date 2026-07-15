+++
title = "vignette"
description = "Darkens the corners, and optionally the whole frame, with a soft radial gradient. Static, and costs nearly nothing."
weight = 20
template = "docs-page.html"

[extra]
one_liner = "soft corner darkening"
category = "overlays"
image = "previews/vignette.jpg"
used_in = "shinkai.toml"

[[extra.props]]
key = "color"
type = "[r,g,b,a]"
default = "`[0.10, 0.14, 0.20, 1.0]`"
meaning = "Vignette tint; the alpha is a master intensity multiplier."

[[extra.props]]
key = "opacity_top_left"
type = "float"
default = "`0.6`"
meaning = "Strength of the top-left corner."

[[extra.props]]
key = "opacity_top_right"
type = "float"
default = "`0.6`"
meaning = "Strength of the top-right corner."

[[extra.props]]
key = "opacity_bottom_left"
type = "float"
default = "`0.7`"
meaning = "Strength of the bottom-left corner."

[[extra.props]]
key = "opacity_bottom_right"
type = "float"
default = "`0.7`"
meaning = "Strength of the bottom-right corner."

[[extra.props]]
key = "radius"
type = "float"
default = "`0.7`"
meaning = "How far each corner's shading reaches toward the center, as a fraction of the half-diagonal."

[[extra.props]]
key = "base_opacity"
type = "float"
default = "`0.0`"
meaning = "Uniform dim over the whole frame, under the corners. `0.15` to `0.3` gives a soft haze; `0` is the classic corners-only look."
+++

Darkens the corners, and optionally the whole frame, with a soft radial gradient. Static, and costs nearly nothing.

The bottom corners default slightly stronger than the top; that is where wallpapers tend
to be brightest. The summed opacity saturates at fully opaque rather than overflowing, so
generous values are safe.
