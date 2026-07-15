+++
title = "raymarcher"
description = "A slow camera drift through infinite raymarched gyroid tunnels. Also the built-in default scene when no config file exists."
weight = 13
template = "docs-page.html"

[extra]
one_liner = "infinite gyroid tunnels"
category = "backgrounds"
preview = "raymarcher"
example = "raymarcher.toml"

[[extra.props]]
key = "colors"
type = "array of [r,g,b]"
default = "indigo, amber, teal"
meaning = "2 to 4 palette stops; the first also tints the fog."

[[extra.props]]
key = "speed"
type = "float"
default = "`1.0`"
meaning = "Drift speed; `1.0` crosses one tunnel cell every ~18 s, `0` freezes the camera. Clamped to 0 to 10."

[[extra.props]]
key = "fov_deg"
type = "float"
default = "`70.0`"
meaning = "Vertical field of view in degrees. Clamped to 30 to 110."

[[extra.props]]
key = "fog"
type = "float"
default = "`1.0`"
meaning = "Fog-density multiplier. Very low values also reveal the far draw boundary, so they are not recommended. Clamped to 0 to 4."

[[extra.props]]
key = "render_scale"
type = "float"
default = "`0.5`"
meaning = "Internal resolution as a fraction of the region; the host upscales. `0.5` costs a quarter of the rays of native."

[[extra.props]]
key = "max_fps"
type = "float"
default = "`30.0`"
meaning = "Frame-rate cap. `0` means uncapped (compositor rate). Clamped to 0 to 240."
+++

A slow camera drift through infinite raymarched gyroid tunnels. Also the built-in default scene when no config file exists.

The scene itself is fixed: there is one tunnel geometry and no scene-selection key. You
steer the palette, fog, and pace. Default stops are
`[[0.08, 0.10, 0.18], [0.55, 0.30, 0.15], [0.20, 0.35, 0.40]]`.

The two thermal knobs (`render_scale`, `max_fps`) are conservative by default. Raise them
if you have GPU headroom and want a sharper, smoother tunnel.
