+++
title = "weather"
description = "Current conditions and temperature from Open-Meteo (keyless), drawn as a glass card or a compact status pill. Read-only; the network fetch is cached and kept off the render path."
weight = 52
template = "docs-page.html"

[extra]
binary = "veiland-weather"
one_liner = "current conditions"
category = "widgets"
image = "previews/weather.png"
example = "weather.toml"

[[extra.props]]
key = "layout"
type = "string"
default = "`\"card\"`"
meaning = "`card` (a bottom-corner glass card) or `pill` (a compact status-pill glyph + temperature)."

[[extra.props]]
key = "units"
type = "string"
default = "`\"celsius\"`"
meaning = "Display unit: `celsius` or `fahrenheit`."

[[extra.props]]
key = "location"
type = "string"
default = "none"
meaning = "City name, geocoded once at startup (a privacy-friendlier alternative to raw coordinates). Ignored if `latitude`/`longitude` are set."

[[extra.props]]
key = "latitude"
type = "float"
default = "none"
meaning = "Explicit latitude (&minus;90..90). Requires `longitude` too; wins over `location`. A partial or out-of-range pair is ignored."

[[extra.props]]
key = "longitude"
type = "float"
default = "none"
meaning = "Explicit longitude (&minus;180..180), paired with `latitude`."

[[extra.props]]
key = "network"
type = "bool"
default = "`true`"
meaning = "Master network switch. `false` = no HTTP fetch and no geocoding; the widget shows cached data or a placeholder only."

[[extra.props]]
key = "refresh_minutes"
type = "float"
default = "`15.0`"
meaning = "Minutes between fetches, floored at `5.0`."

[[extra.props]]
key = "show_location"
type = "bool"
default = "`true`"
meaning = "Whether the card shows the place-name label."

[[extra.props]]
key = "pill_color"
type = "[r,g,b,a]"
default = "`[0.059, 0.071, 0.11, 0.686]`"
meaning = "Chip background in `pill` layout. `[0, 0, 0, 0]` draws no chip."

[[extra.props]]
key = "icon_color"
type = "[r,g,b,a]"
default = "white"
meaning = "Tints the condition glyph. Omit for white."
+++

`weather` fetches from [Open-Meteo](https://open-meteo.com/), which needs no API key. Give
it a `location` city name (geocoded once) or an explicit `latitude`/`longitude`; with
neither it uses IP geolocation. The fetch runs on a timer, is cached to disk, and is kept
off the render path, so a locked screen never blocks on the network. Set `network = false`
to disable all outbound requests entirely.

See the [widgets](@/docs/plugins/_index.md) overview for the shared font,
`content_halign` / `content_valign`, and `debug_border` keys.
