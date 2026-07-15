+++
title = "particles"
description = "Small soft glowing motes drifting slowly upward, the only riser in the particle family."
weight = 30
template = "docs-page.html"

[extra]
one_liner = "rising motes"
category = "particles"
preview = "particlesprev"
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
+++

Small soft glowing motes drifting slowly upward, the only riser in the particle family.

Like the rest of the family, `count` is an absolute number, not a density: the same
value puts the same number of motes on a 1080p and a 4K monitor. Bump it per scene if a
field tuned on a laptop looks sparse on a large display.
