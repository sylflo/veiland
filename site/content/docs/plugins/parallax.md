+++
title = "parallax"
description = "Three depth layers of soft bokeh circles drifting at different speeds. A subtle depth cue over any background, fully procedural."
weight = 21
template = "docs-page.html"

[extra]
one_liner = "layered bokeh depth"
category = "overlays"
image = "previews/readme/gallery-parallax.gif"
example = "parallax.toml"

[[extra.props]]
key = "color"
type = "[r,g,b,a]"
default = "`[1.0, 1.0, 1.0, 0.2]`"
meaning = "Circle color; the alpha is the master opacity of the whole effect."

[[extra.props]]
key = "size_px"
type = "float"
default = "`80.0`"
meaning = "Max circle radius of the near layer, in logical px; the deeper layers scale down from it. Clamped to 4 to 512."

[[extra.props]]
key = "density"
type = "float"
default = "`0.5`"
meaning = "Fraction of the layout grid that holds a circle, 0 to 1."

[[extra.props]]
key = "speed"
type = "float"
default = "`8.0`"
meaning = "Near-layer drift in px/s; deeper layers move slower. Clamped to 0 to 200."

[[extra.props]]
key = "angle_deg"
type = "float"
default = "`30.0`"
meaning = "Drift direction; `0` is rightward, `90` is upward."

[[extra.props]]
key = "softness"
type = "float"
default = "`0.5`"
meaning = "Edge feather as a fraction of the radius. `1.0` is fully soft bokeh, small values give crisp dots. Clamped to 0.02 to 1."

[[extra.props]]
key = "seed"
type = "integer"
default = "`2654435769`"
meaning = "Layout seed; change it to reshuffle all three layers."
+++

Three depth layers of soft bokeh circles drifting at different speeds. A subtle depth cue over any background, fully procedural.

The layer ratios (size, speed, and opacity per depth) are fixed. No image files are
involved; everything is generated.
