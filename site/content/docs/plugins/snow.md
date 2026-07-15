+++
title = "snow"
description = "A few large procedural snow crystals: six-fold dendritic flakes, each uniquely shaped, drifting down with a slow tumble."
weight = 32
template = "docs-page.html"

[extra]
one_liner = "dendritic crystals"
category = "particles"
image = "previews/readme/gallery-snow.gif"
example = "snow.toml"

[[extra.props]]
key = "count"
type = "integer"
default = "`12`"
meaning = "Number of crystals. Deliberately low, the detail needs room."

[[extra.props]]
key = "color"
type = "[r,g,b,a]"
default = "`[1.0, 1.0, 1.0, 0.9]`"
meaning = "Crystal color."

[[extra.props]]
key = "radius_px"
type = "float"
default = "`60.0`"
meaning = "Crystal radius in logical px. Below ~40 the fern structure collapses into a dot; this effect wants few and large, not a dense flurry."
+++

Every crystal is generated procedurally, so no two are the same shape.
