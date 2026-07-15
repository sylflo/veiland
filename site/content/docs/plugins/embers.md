+++
title = "embers"
description = "A warm glow band along the bottom edge with bright sparks rising, curving, and fading as they climb."
weight = 34
template = "docs-page.html"

[extra]
one_liner = "rising sparks"
category = "particles"
image = "previews/readme/gallery-embers.gif"
example = "embers.toml"

[[extra.props]]
key = "count"
type = "integer"
default = "`80`"
meaning = "Number of sparks."

[[extra.props]]
key = "spark_color"
type = "[r,g,b,a]"
default = "`[1.0, 0.65, 0.10, 1.0]`"
meaning = "Spark color (hot core; the halo reuses it dimmer)."

[[extra.props]]
key = "glow_color"
type = "[r,g,b]"
default = "`[0.80, 0.18, 0.02]`"
meaning = "Color of the bottom glow band. Three components, no alpha; the band's strength and height (bottom ~30% of the region) are fixed."
+++
