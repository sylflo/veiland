+++
title = "fireflies"
description = "Softly glowing lights wandering on lazy paths, each blinking on its own rhythm."
weight = 35
template = "docs-page.html"

[extra]
one_liner = "wandering glow"
category = "particles"
image = "previews/readme/gallery-fireflies.gif"
example = "fireflies.toml"

[[extra.props]]
key = "count"
type = "integer"
default = "`25`"
meaning = "Number of fireflies."

[[extra.props]]
key = "color"
type = "[r,g,b,a]"
default = "`[0.72, 1.0, 0.18, 0.95]`"
meaning = "Glow color (warm yellow-green); alpha is the peak flash brightness."

[[extra.props]]
key = "radius_px"
type = "float"
default = "`2.5`"
meaning = "Core radius in logical px; the visible halo extends about 4x beyond it."

[[extra.props]]
key = "flash_sharpness"
type = "float"
default = "`0.4`"
meaning = "Blink character, 0 to 1: `0` is gentle continuous pulsing, `1` is brief sharp flashes with long dark gaps."
+++

Softly glowing lights wandering on lazy paths, each blinking on its own rhythm.
