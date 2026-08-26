+++
title = "shape"
description = "One rounded, colored, alpha-blended rectangle filling its region: the backdrop/card primitive. Static, read-only, draws once. The veiland answer to hyprlock's shape block."
weight = 55
template = "docs-page.html"

[extra]
binary = "veiland-shape"
one_liner = "a rounded colored card"
category = "widgets"
image = "previews/shape.png"
example = "shape.toml"

[[extra.props]]
key = "color"
type = "[r,g,b,a]"
default = "`[0.8, 0.643, 0.486, 1.0]`"
meaning = "Fill color of the rectangle (a warm tan by default). Alpha is the opacity, so a translucent value makes a tinted card over the wallpaper. There is no \"off\" &mdash; shape always paints its region."

[[extra.props]]
key = "radius"
type = "float"
default = "`0.0`"
meaning = "Corner radius as a fraction of the region **height**. `0` = hard corners; an over-large value clamps to half the shorter side (a pill or circle)."
+++

`shape` is the card behind your other widgets: a semi-transparent rounded rectangle you
stack *under* a clock, greeting, or status cluster to group them visually. It draws once
and never changes &mdash; no data source, no polling.

There is no grouping primitive in veiland: you place a card by giving `shape` and the
content widget the **same region**, and giving the content a higher `z_index` so it paints
on top. Where the card sits and how big it is are set by the `[[plugin]]` region anchor
(see [configuration](@/docs/configuration.md)); this plugin only fills whatever region it
is handed.

A `shape` does **not** blur. A frosted-glass card comes from the wallpaper's
[`blur_regions`](@/docs/plugins/_index.md) (real OpenGL blur); `shape` is a flat
translucent tint. For text or an icon on the card, stack a `markup` or status
widget on top.
