+++
title = "blobs"
description = "Large soft metaballs drifting on slow orbits over a dark background. The lava-lamp look."
weight = 12
template = "docs-page.html"

[extra]
one_liner = "lava-lamp metaballs"
category = "backgrounds"
preview = "blobs"
example = "blobs.toml"

[[extra.props]]
key = "colors"
type = "array of [r,g,b]"
default = "blue, magenta, teal, amber"
meaning = "Blob palette, 1 to 8 colors, cycled across blobs."

[[extra.props]]
key = "background"
type = "[r,g,b]"
default = "`[0.02, 0.03, 0.08]`"
meaning = "The color the blobs float over."

[[extra.props]]
key = "count"
type = "integer"
default = "`6`"
meaning = "Number of blobs. Clamped to 1 to 8."

[[extra.props]]
key = "size"
type = "float"
default = "`0.25`"
meaning = "Base blob radius as a fraction of screen height; each blob varies about 30% around it. Past roughly 0.35 the field saturates."

[[extra.props]]
key = "speed"
type = "float"
default = "`1.0`"
meaning = "Drift-speed multiplier; `1.0` is one slow orbit over a couple of minutes, `0` freezes the field. Clamped to 0 to 10."

[[extra.props]]
key = "softness"
type = "float"
default = "`0.6`"
meaning = "Edge falloff. Lower gives tighter cores and darker gaps, higher gets hazier until blobs wash together. Clamped to 0.25 to 4."

[[extra.props]]
key = "seed"
type = "integer"
default = "`2654435769`"
meaning = "Layout and motion seed; change it for a different arrangement."
+++

Large soft metaballs drifting on slow orbits over a dark background. The lava-lamp look.

Default palette: `[[0.12, 0.20, 0.55], [0.45, 0.15, 0.50], [0.05, 0.42, 0.45], [0.50, 0.28, 0.12]]`.
Fewer colors than blobs just cycles the palette. The motion never visibly repeats.
