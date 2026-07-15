+++
title = "gradient"
description = "A slow-flowing, seamlessly looping multi-stop color gradient, optionally with a rotating axis."
weight = 11
template = "docs-page.html"

[extra]
one_liner = "flowing color ramp"
category = "backgrounds"
image = "previews/readme/gallery-gradient.gif"
example = "gradient.toml"

[[extra.props]]
key = "colors"
type = "array of [r,g,b]"
default = "indigo, purple, teal"
meaning = "2 to 4 ramp stops; extras beyond 4 are ignored."

[[extra.props]]
key = "angle_deg"
type = "float"
default = "`45.0`"
meaning = "Gradient axis. `0` is left-to-right, positive rotates clockwise."

[[extra.props]]
key = "speed"
type = "float"
default = "`0.25`"
meaning = "Ramp loop speed in cycles per minute (`0.25` is one loop every 4 minutes). `0` freezes it."

[[extra.props]]
key = "rotate_deg_per_min"
type = "float"
default = "`0.0`"
meaning = "Axis rotation in degrees per minute. `0` keeps the axis fixed. Clamped to plus or minus 360."

[[extra.props]]
key = "scale"
type = "float"
default = "`0.75`"
meaning = "Ramp lengths per screen height. Smaller means broader, softer bands. Clamped to 0.05 to 10."
+++

A slow-flowing, seamlessly looping multi-stop color gradient, optionally with a rotating axis.

Default stops are `[[0.10, 0.16, 0.42], [0.38, 0.12, 0.48], [0.05, 0.36, 0.44]]`.
Fewer than 2 valid stops falls back to that default palette. `speed` is clamped to
0 to 30 cycles per minute.
