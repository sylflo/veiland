+++
title = "sakura"
description = "Falling, swaying, tumbling cherry-blossom petals, drawn from a built-in petal texture."
weight = 31
template = "docs-page.html"

[extra]
one_liner = "falling petals"
category = "particles"
preview = "sakura"
example = "sakura.toml"

[[extra.props]]
key = "count"
type = "integer"
default = "`25`"
meaning = "Number of petals."

[[extra.props]]
key = "color"
type = "[r,g,b,a]"
default = "`[1.0, 1.0, 1.0, 1.0]`"
meaning = "A tint multiplied into the petal texture; the petals are already pink, so white means as-is. Lower the alpha to fade the whole field."

[[extra.props]]
key = "size_px"
type = "float"
default = "`22.0`"
meaning = "Petal size in logical px."
+++

Falling, swaying, tumbling cherry-blossom petals, drawn from a built-in petal texture.

The petal texture is embedded in the binary, so there is nothing to supply. The motion
(sway, tumble, timing) is tuned per effect and not configurable.
