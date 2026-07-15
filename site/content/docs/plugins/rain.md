+++
title = "rain"
description = "Wind-slanted rain streaks with depth: near drops are longer, faster, and brighter than far ones."
weight = 33
template = "docs-page.html"

[extra]
one_liner = "slanted streaks"
category = "particles"
image = "previews/readme/gallery-rain.gif"
example = "rain.toml"

[[extra.props]]
key = "count"
type = "integer"
default = "`90`"
meaning = "Number of drops. Rain is a volume, so the default is the densest in the family."

[[extra.props]]
key = "color"
type = "[r,g,b,a]"
default = "`[0.72, 0.80, 0.95, 0.65]`"
meaning = "Drop color (cool translucent blue-grey); alpha sets the brightness of the nearest drops."

[[extra.props]]
key = "length_px"
type = "float"
default = "`36.0`"
meaning = "Streak length in logical px for the nearest drops; farther drops shrink automatically."

[[extra.props]]
key = "slant_deg"
type = "float"
default = "`10.0`"
meaning = "Shared wind angle in degrees from vertical; positive leans the fall rightward. All drops share it, so the rain falls as a coherent sheet."
+++

`slant_deg` is the only configurable wind in the particle family.
