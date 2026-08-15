+++
title = "particles"
description = "Small soft glowing motes drifting slowly upward, the only riser in the particle family."
weight = 30
template = "docs-page.html"

[extra]
one_liner = "rising motes"
category = "particles"
image = "previews/readme/gallery-particles.gif"
used_in = "shinkai.toml"

[[extra.props]]
key = "count"
type = "integer"
default = "`40`"
meaning = "Number of motes."

[[extra.props]]
key = "color"
type = "[r,g,b,a]"
default = "`[1.0, 1.0, 1.0, 0.5]`"
meaning = "Mote color."

[[extra.props]]
key = "radius_px"
type = "float"
default = "`0.4`"
meaning = "Core radius in logical px. Deliberately tiny; a soft glow halo about 3x the core does the visible work, so small changes go a long way."

[[extra.props]]
key = "twinkle"
type = "bool"
default = "`false`"
meaning = "Pulse each mote's brightness in and out, so the field shimmers instead of glowing steadily. Off = constant brightness."

[[extra.props]]
key = "twinkle_speed"
type = "float"
default = "`1.4`"
meaning = "Pulse rate in radians/second (only used when `twinkle` is on). Higher = faster shimmer."

[[extra.props]]
key = "twinkle_depth"
type = "float"
default = "`0.6`"
meaning = "How far the pulse dims a mote below its peak, `0.0`&ndash;`1.0` (only used when `twinkle` is on). This is the \"how noticeable\" lever: `0` is imperceptible, `1` fades fully to dark."
+++

Like the rest of the family, `count` is an absolute number, not a density: the same
value puts the same number of motes on a 1080p and a 4K monitor. Bump it per scene if a
field tuned on a laptop looks sparse on a large display.

Set `twinkle = true` for a shimmering starfield: each mote pulses independently, at
`twinkle_speed`, dimming by up to `twinkle_depth`. The [`deepfield.toml`](https://github.com/sylflo/veiland/blob/master/docs/examples/deepfield.toml)
scene uses it for its twinkling stars.
