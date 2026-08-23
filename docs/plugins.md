<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

<!--
  GENERATED FILE, do not edit by hand.
  Source of truth: site/content/docs/plugins/ (frontmatter + body).
  Regenerate with: python3 scripts/gen-plugins-md.py
  CI verifies this file is in sync (.github/workflows/site.yml).
-->

# Veiland plugin reference

Every first-party plugin, its config keys, types, and defaults. This is
the companion to [`config.md`](config.md): that document covers the core
schema (`name`, `binary`, `z_index`, `region`, `monitors`, `[password]`);
this one covers what goes *inside* each plugin's `[plugin.config]` table.

The complete working scenes in [`docs/examples/`](examples/) use these
keys; the website gallery shows what each scene looks like.

## How plugin options work

Plugin options live in the `[plugin.config]` table of a `[[plugin]]` entry. The
host passes the table through to the plugin process verbatim (see the
[configuration reference](config.md)); the keys documented per
plugin are each plugin's own schema.

```toml
[[plugin]]
name = "sakura"
binary = "veiland-sakura"
z_index = 25

[plugin.config]
count = 40
size_px = 26.0
color = [1.0, 0.9, 0.95, 0.9]
```

Conventions shared by all first-party plugins:

- **Colors are float arrays, not hex strings.** `[r, g, b]` or `[r, g, b, a]`,
  each component `0.0`–`1.0`. There is no `"#rrggbb"` or `"rgba(...)"` form
  here — that string syntax belongs to the core's `[password]` table only.
- **Every key is optional.** An omitted key falls back to its default, so a
  `[plugin.config]` table with one key is fine, and no table at all gives you
  the plugin's stock look.
- **Bad config never crashes anything.** If the table fails to parse as a
  whole — including one key of the wrong *type* — the plugin logs a warning to
  stderr and runs with **all** defaults (there is no partial recovery).
  Out-of-range values are clamped or replaced per key where the plugin
  validates them.
- **Misspelled keys are silently ignored.** `raduis_px = 60` is not an error;
  the plugin just never sees it and uses the default. If a setting seems to
  have no effect, check the spelling first.
- **Sizes ending in `_px` are logical pixels.** They are multiplied by the
  output scale, so one value renders the same physical size on 1× and HiDPI
  monitors. Text sizes use a different model — a `font_size` is a fraction of
  the widget's region, not a pixel count (see the `markup` widget).

## Backgrounds

Meant for the bottom of the stack (low `z_index`).

### wallpaper — `veiland-wallpaper`

Displays one JPEG or PNG, stretched to fill its region. Any failure logs the reason and renders solid black. A wrong path never breaks the lock.
Used in [`examples/sakura.toml`](examples/sakura.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | `""` | Absolute path to the image (no `~` expansion). JPEG/PNG, detected by content. |
| `blur` | float | `0.0` | Gaussian blur strength over the whole surface. `0` is off and a hard no-op (the blur path is skipped entirely, so a plain wallpaper pays nothing). Rounded and clamped to `0`&ndash;`20` passes; values below `0.5` round to off. |
| `darken` | float | `0.0` | Base dim (`0.0`&ndash;`1.0`) applied everywhere on top of the (possibly blurred) image. `0` = no dimming. Also the default per-region darken for any `blur_regions` entry that omits its own. |
| `blur_regions` | array of tables | `[]` | Frosted "cards": rounded rectangles that add an extra tint on top of the globally blurred base. Each entry is `{ x, y, w, h, radius, darken }` (see below). Empty = just the global blur/darken. Up to 10; extras are dropped with a log. |

The image is stretched to the region with no cover or contain modes, so pick an image
matching your monitor's aspect ratio. Decoding runs on a worker thread; the first frames
may be black before the image pops in.

Remember the pitfall from [configuration](config.md): asset paths get no `~`
or `$HOME` expansion, so always give a full absolute path.

## Blur and frosted cards

`blur` blurs the entire surface; `darken` dims it. Both are free when left at `0` &mdash;
the ping-pong blur path is only built when `blur > 0`. This is how you get a
frosted-glass backdrop: blur the wallpaper, then place your clock and widgets on top.

`blur_regions` layers "cards" on that blurred base &mdash; rounded rectangles that get an
extra `darken` so they read as distinct panels behind a widget. Blur is *not* gated by the
regions: the whole surface is blurred when `blur > 0`, and each region only adds its
per-card tint. Every region field is a **fraction of the surface** (`0.0`&ndash;`1.0`), so
one config looks the same on any monitor:

| Region key | Type | Default | Meaning |
|---|---|---|---|
| `x`, `y` | float | `0.0` | Top-left corner of the card. `y` is measured from the top. |
| `w`, `h` | float | `0.0` | Card width and height. |
| `radius` | float | `0.0` | Corner radius, as a fraction of surface **height** (so corners stay circular, not elliptical). `0` = hard corners; an over-large value clamps to a pill/circle. |
| `darken` | float | inherits `darken` | Per-card dim (`0.0`&ndash;`1.0`). Omit to inherit the global `darken`; set it to make a card read as a card over the surrounding blur. |

```toml
[plugin.config]
path = "/home/you/wall.jpg"
blur = 10
darken = 0.2
blur_regions = [
  { x = 0.06, y = 0.30, w = 0.28, h = 0.40, radius = 0.03, darken = 0.45 },
]
```

The [`aurora-dashboard.toml`](examples/aurora-dashboard.toml)
and [`deepfield.toml`](examples/deepfield.toml)
scenes use these keys.

### gradient — `veiland-gradient`

A slow-flowing, seamlessly looping multi-stop color gradient, optionally with a rotating axis.
Example: [`examples/gradient.toml`](examples/gradient.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `colors` | array of [r,g,b] | indigo, purple, teal | 2 to 4 ramp stops; extras beyond 4 are ignored. |
| `angle_deg` | float | `45.0` | Gradient axis. `0` is left-to-right, positive rotates clockwise. |
| `speed` | float | `0.25` | Ramp loop speed in cycles per minute (`0.25` is one loop every 4 minutes). `0` freezes it. |
| `rotate_deg_per_min` | float | `0.0` | Axis rotation in degrees per minute. `0` keeps the axis fixed. Clamped to plus or minus 360. |
| `scale` | float | `0.75` | Ramp lengths per screen height. Smaller means broader, softer bands. Clamped to 0.05 to 10. |

Default stops are `[[0.10, 0.16, 0.42], [0.38, 0.12, 0.48], [0.05, 0.36, 0.44]]`.
Fewer than 2 valid stops falls back to that default palette. `speed` is clamped to
0 to 30 cycles per minute.

### blobs — `veiland-blobs`

Large soft metaballs drifting on slow orbits over a dark background. The lava-lamp look.
Example: [`examples/blobs.toml`](examples/blobs.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `colors` | array of [r,g,b] | blue, magenta, teal, amber | Blob palette, 1 to 8 colors, cycled across blobs. |
| `background` | [r,g,b] | `[0.02, 0.03, 0.08]` | The color the blobs float over. |
| `count` | integer | `6` | Number of blobs. Clamped to 1 to 8. |
| `size` | float | `0.25` | Base blob radius as a fraction of screen height; each blob varies about 30% around it. Past roughly 0.35 the field saturates. |
| `speed` | float | `1.0` | Drift-speed multiplier; `1.0` is one slow orbit over a couple of minutes, `0` freezes the field. Clamped to 0 to 10. |
| `softness` | float | `0.6` | Edge falloff. Lower gives tighter cores and darker gaps, higher gets hazier until blobs wash together. Clamped to 0.25 to 4. |
| `seed` | integer | `2654435769` | Layout and motion seed; change it for a different arrangement. |

Default palette: `[[0.12, 0.20, 0.55], [0.45, 0.15, 0.50], [0.05, 0.42, 0.45], [0.50, 0.28, 0.12]]`.
Fewer colors than blobs just cycles the palette. The motion never visibly repeats.

### raymarcher — `veiland-raymarcher`

A slow camera drift through infinite raymarched gyroid tunnels. Also the built-in default scene when no config file exists.
Example: [`examples/raymarcher.toml`](examples/raymarcher.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `colors` | array of [r,g,b] | indigo, amber, teal | 2 to 4 palette stops; the first also tints the fog. |
| `speed` | float | `1.0` | Drift speed; `1.0` crosses one tunnel cell every ~18 s, `0` freezes the camera. Clamped to 0 to 10. |
| `fov_deg` | float | `70.0` | Vertical field of view in degrees. Clamped to 30 to 110. |
| `fog` | float | `1.0` | Fog-density multiplier. Very low values also reveal the far draw boundary, so they are not recommended. Clamped to 0 to 4. |
| `render_scale` | float | `0.5` | Internal resolution as a fraction of the region; the host upscales. `0.5` costs a quarter of the rays of native. |
| `max_fps` | float | `30.0` | Frame-rate cap. `0` means uncapped (compositor rate). Clamped to 0 to 240. |

The scene itself is fixed: there is one tunnel geometry and no scene-selection key. You
steer the palette, fog, and pace. Default stops are
`[[0.08, 0.10, 0.18], [0.55, 0.30, 0.15], [0.20, 0.35, 0.40]]`.

The two thermal knobs (`render_scale`, `max_fps`) are conservative by default. Raise them
if you have GPU headroom and want a sharper, smoother tunnel.

## Overlays

Transparent plugins meant to sit above a background and below text.

### vignette — `veiland-vignette`

Darkens the corners, and optionally the whole frame, with a soft radial gradient. Static, and costs nearly nothing.
Used in [`examples/shinkai.toml`](examples/shinkai.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `color` | [r,g,b,a] | `[0.10, 0.14, 0.20, 1.0]` | Vignette tint; the alpha is a master intensity multiplier. |
| `opacity_top_left` | float | `0.6` | Strength of the top-left corner. |
| `opacity_top_right` | float | `0.6` | Strength of the top-right corner. |
| `opacity_bottom_left` | float | `0.7` | Strength of the bottom-left corner. |
| `opacity_bottom_right` | float | `0.7` | Strength of the bottom-right corner. |
| `radius` | float | `0.7` | How far each corner's shading reaches toward the center, as a fraction of the half-diagonal. |
| `base_opacity` | float | `0.0` | Uniform dim over the whole frame, under the corners. `0.15` to `0.3` gives a soft haze; `0` is the classic corners-only look. |

The bottom corners default slightly stronger than the top; that is where wallpapers tend
to be brightest. The summed opacity saturates at fully opaque rather than overflowing, so
generous values are safe.

### parallax — `veiland-parallax`

Three depth layers of soft bokeh circles drifting at different speeds. A subtle depth cue over any background, fully procedural.
Example: [`examples/parallax.toml`](examples/parallax.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `color` | [r,g,b,a] | `[1.0, 1.0, 1.0, 0.2]` | Circle color; the alpha is the master opacity of the whole effect. |
| `size_px` | float | `80.0` | Max circle radius of the near layer, in logical px; the deeper layers scale down from it. Clamped to 4 to 512. |
| `density` | float | `0.5` | Fraction of the layout grid that holds a circle, 0 to 1. |
| `speed` | float | `8.0` | Near-layer drift in px/s; deeper layers move slower. Clamped to 0 to 200. |
| `angle_deg` | float | `30.0` | Drift direction; `0` is rightward, `90` is upward. |
| `softness` | float | `0.5` | Edge feather as a fraction of the radius. `1.0` is fully soft bokeh, small values give crisp dots. Clamped to 0.02 to 1. |
| `seed` | integer | `2654435769` | Layout seed; change it to reshuffle all three layers. |

The layer ratios (size, speed, and opacity per depth) are fixed. No image files are
involved; everything is generated.

## Particles

Six variations on one idea: a field of independent particles drifting across a
transparent buffer, composited over your background. They share two keys — `count`
and a color — plus one size key each; the motion itself (sway, timing, fades) is
tuned per effect and not configurable.

`count` is an absolute number, not a density: the same value puts the same number
of particles on a 1080p and a 4K monitor. Sizes (`*_px`) do scale with the output,
so the particles themselves stay the same physical size.

### particles — `veiland-particles`

Small soft glowing motes drifting slowly upward, the only riser in the particle family.
Used in [`examples/shinkai.toml`](examples/shinkai.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `count` | integer | `40` | Number of motes. |
| `color` | [r,g,b,a] | `[1.0, 1.0, 1.0, 0.5]` | Mote color. |
| `radius_px` | float | `0.4` | Core radius in logical px. Deliberately tiny; a soft glow halo about 3x the core does the visible work, so small changes go a long way. |
| `twinkle` | bool | `false` | Pulse each mote's brightness in and out, so the field shimmers instead of glowing steadily. Off = constant brightness. |
| `twinkle_speed` | float | `1.4` | Pulse rate in radians/second (only used when `twinkle` is on). Higher = faster shimmer. |
| `twinkle_depth` | float | `0.6` | How far the pulse dims a mote below its peak, `0.0`&ndash;`1.0` (only used when `twinkle` is on). This is the "how noticeable" lever: `0` is imperceptible, `1` fades fully to dark. |

Like the rest of the family, `count` is an absolute number, not a density: the same
value puts the same number of motes on a 1080p and a 4K monitor. Bump it per scene if a
field tuned on a laptop looks sparse on a large display.

Set `twinkle = true` for a shimmering starfield: each mote pulses independently, at
`twinkle_speed`, dimming by up to `twinkle_depth`. The [`deepfield.toml`](examples/deepfield.toml)
scene uses it for its twinkling stars.

### sakura — `veiland-sakura`

Falling, swaying, tumbling cherry-blossom petals, drawn from a built-in petal texture.
Example: [`examples/sakura.toml`](examples/sakura.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `count` | integer | `25` | Number of petals. |
| `color` | [r,g,b,a] | `[1.0, 1.0, 1.0, 1.0]` | A tint multiplied into the petal texture; the petals are already pink, so white means as-is. Lower the alpha to fade the whole field. |
| `size_px` | float | `22.0` | Petal size in logical px. |

The petal texture is embedded in the binary, so there is nothing to supply. The motion
(sway, tumble, timing) is tuned per effect and not configurable.

### snow — `veiland-snow`

A few large procedural snow crystals: six-fold dendritic flakes, each uniquely shaped, drifting down with a slow tumble.
Example: [`examples/snow.toml`](examples/snow.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `count` | integer | `12` | Number of crystals. Deliberately low, the detail needs room. |
| `color` | [r,g,b,a] | `[1.0, 1.0, 1.0, 0.9]` | Crystal color. |
| `radius_px` | float | `60.0` | Crystal radius in logical px. Below ~40 the fern structure collapses into a dot; this effect wants few and large, not a dense flurry. |

Every crystal is generated procedurally, so no two are the same shape.

### rain — `veiland-rain`

Wind-slanted rain streaks with depth: near drops are longer, faster, and brighter than far ones.
Example: [`examples/rain.toml`](examples/rain.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `count` | integer | `90` | Number of drops. Rain is a volume, so the default is the densest in the family. |
| `color` | [r,g,b,a] | `[0.72, 0.80, 0.95, 0.65]` | Drop color (cool translucent blue-grey); alpha sets the brightness of the nearest drops. |
| `length_px` | float | `36.0` | Streak length in logical px for the nearest drops; farther drops shrink automatically. |
| `slant_deg` | float | `10.0` | Shared wind angle in degrees from vertical; positive leans the fall rightward. All drops share it, so the rain falls as a coherent sheet. |

`slant_deg` is the only configurable wind in the particle family.

### embers — `veiland-embers`

A warm glow band along the bottom edge with bright sparks rising, curving, and fading as they climb.
Example: [`examples/embers.toml`](examples/embers.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `count` | integer | `80` | Number of sparks. |
| `spark_color` | [r,g,b,a] | `[1.0, 0.65, 0.10, 1.0]` | Spark color (hot core; the halo reuses it dimmer). |
| `glow_color` | [r,g,b] | `[0.80, 0.18, 0.02]` | Color of the bottom glow band. Three components, no alpha; the band's strength and height (bottom ~30% of the region) are fixed. |

### fireflies — `veiland-fireflies`

Softly glowing lights wandering on lazy paths, each blinking on its own rhythm.
Example: [`examples/fireflies.toml`](examples/fireflies.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `count` | integer | `25` | Number of fireflies. |
| `color` | [r,g,b,a] | `[0.72, 1.0, 0.18, 0.95]` | Glow color (warm yellow-green); alpha is the peak flash brightness. |
| `radius_px` | float | `2.5` | Core radius in logical px; the visible halo extends about 4x beyond it. |
| `flash_sharpness` | float | `0.4` | Blink character, 0 to 1: `0` is gentle continuous pulsing, `1` is brief sharp flashes with long dark gaps. |

## Widgets

Unlike the background plugins (which are Rust), these reference
**widgets are Python programs**, installed as `veiland-avatar`,
`veiland-weather`, `veiland-now-playing`, `veiland-markup`, `veiland-shape`,
and the status pills `veiland-wifi` / `veiland-ethernet` / `veiland-bluetooth`
/ `veiland-battery`. They speak the same protocol and read the same
`[plugin.config]` table; the language is an implementation detail. A widget
reads live data (battery, network, media, weather) read-only and never
receives keyboard input, exactly like any other plugin.

**Shared keys.** Most widgets honor the same opt-in conventions, so these are
documented once here rather than repeated per widget:

- **Placement inside the region** — `content_halign` (`left`/`center`/`right`,
  default `center`) and `content_valign` (`top`/`center`/`bottom`, default
  `center`) position the widget's content block within its assigned region.
  Used by every widget except `shape` (which fills its region) and
  `now-playing` (which self-centers).
- **Font** — `font_family` (default `"Sans"`), `font_weight` (CSS 100&ndash;900,
  default `400`), and `italic` (default `false`) style any text a widget draws;
  `font_size` is a fraction of the widget's box. The status pills, `markup`, and
  `weather` use the full set. `avatar` and `now-playing` derive their text size
  from geometry, so they honor only `font_family` + `italic`. `shape` and
  `ethernet` draw no text and read no font keys.
- **Debug border** — `debug_border = true` strokes a 1px outline around the
  region (color `debug_border_color`, default bright magenta) so you can see
  and tune the anchor. Honored by every widget except `shape`.

**Status pills** (`wifi`, `ethernet`, `bluetooth`, `battery`) share a look: a
monochrome glyph in a small translucent chip. They all take `pill_color`
(chip background, default a translucent dark navy; `[0,0,0,0]` draws no chip)
and `icon_color` (glyph tint, default white). Each pill page lists only what
is unique to it &mdash; the label keys and data source.

### avatar — `veiland-avatar`

The user's picture cover-cropped into a disc (or rounded square), or a tinted initials disc when no picture is found. Zero-config: reads your account picture automatically.
Example: [`examples/avatar.toml`](examples/avatar.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | your full name | Seeds the initials letter and the disc's tint. Defaults to your GECOS full name, then `$USER`, then `there`. |
| `avatar` | string | `~/.face` | Path to a picture (`~` expanded), cover-cropped into the disc. Missing or unreadable falls back to `~/.face`, then to a tinted initials disc. |
| `shape` | string | `"circle"` | Disc outline: `circle` or `rounded` (a rounded square). |
| `ring_color` | [r,g,b,a] | `[1.0, 1.0, 1.0, 0.22]` | Thin rim around the disc. Alpha `0` draws no ring. |

`avatar` shows just the profile disc &mdash; pair it with a [`markup`](@/docs/plugins/markup.md)
greeting for a name/welcome line beside it. With no config it uses your account picture
(`~/.face`) and full name automatically, so it works out of the box.

Only `font_family` and `italic` affect the initials letter; its size comes from the disc
diameter, so `font_size` and `font_weight` have no effect here. See the
[widgets](plugins.md) overview for the `content_halign` /
`content_valign` / `debug_border` keys.

### weather — `veiland-weather`

Current conditions and temperature from Open-Meteo (keyless), drawn as a glass card or a compact status pill. Read-only; the network fetch is cached and kept off the render path.
Example: [`examples/weather.toml`](examples/weather.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `layout` | string | `"card"` | `card` (a bottom-corner glass card) or `pill` (a compact status-pill glyph + temperature). |
| `units` | string | `"celsius"` | Display unit: `celsius` or `fahrenheit`. |
| `location` | string | none | City name, geocoded once at startup (a privacy-friendlier alternative to raw coordinates). Ignored if `latitude`/`longitude` are set. |
| `latitude` | float | none | Explicit latitude (&minus;90..90). Requires `longitude` too; wins over `location`. A partial or out-of-range pair is ignored. |
| `longitude` | float | none | Explicit longitude (&minus;180..180), paired with `latitude`. |
| `network` | bool | `true` | Master network switch. `false` = no HTTP fetch and no geocoding; the widget shows cached data or a placeholder only. |
| `refresh_minutes` | float | `15.0` | Minutes between fetches, floored at `5.0`. |
| `show_location` | bool | `true` | Whether the card shows the place-name label. |
| `pill_color` | [r,g,b,a] | `[0.059, 0.071, 0.11, 0.686]` | Chip background in `pill` layout. `[0, 0, 0, 0]` draws no chip. |
| `icon_color` | [r,g,b,a] | white | Tints the condition glyph. Omit for white. |

`weather` fetches from [Open-Meteo](https://open-meteo.com/), which needs no API key. Give
it a `location` city name (geocoded once) or an explicit `latitude`/`longitude`; with
neither it uses IP geolocation. The fetch runs on a timer, is cached to disk, and is kept
off the render path, so a locked screen never blocks on the network. Set `network = false`
to disable all outbound requests entirely.

See the [widgets](plugins.md) overview for the shared font,
`content_halign` / `content_valign`, and `debug_border` keys.

### now-playing — `veiland-now-playing`

A glanceable now-playing card: album art, title and artist, progress bar. Read-only, from any MPRIS media player over D-Bus. No transport controls.
Example: [`examples/now_playing.toml`](examples/now_playing.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `layout` | string | `"compact"` | `compact` (a chip: cover + title/artist + progress) or `star` (a large centered cover with a blurred-cover backdrop). Only `star` is special-cased; any other value renders `compact`. |
| `fetch_remote_art` | bool | `false` | Allow fetching `http(s)://` cover art (e.g. Spotify). Off = a locked screen makes no network request; `file://` covers still decode locally. |

`now-playing` reads whatever your MPRIS-capable player (Spotify, mpv, browsers, ...) is
playing, over the session D-Bus. It is **read-only**: there are no play/pause/skip buttons,
because clicks are not yet forwarded to plugins. The accent color is sampled from the album
art.

Only `font_family` and `italic` affect the text; each line's size is derived from the card
geometry, so `font_size` and `font_weight` have no effect. This widget centers itself and
does not read `content_halign` / `content_valign` (it does honor `debug_border`). See the
[widgets](plugins.md) overview.

### markup — `veiland-markup`

One block of Pango markup with {variable} substitution, composited over the wallpaper. The dynamic styled-text widget: a clock, a greeting, a live system-info line.
Example: [`examples/markup.toml`](examples/markup.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `text` | string | a bold time + date block | The template: Pango `<span>` markup plus `{variable}` placeholders (see the token table below). Newlines allowed. |
| `text_color` | [r,g,b,a] | `[1.0, 1.0, 1.0, 0.96]` | Base text fill. An inline `<span color=...>` in the markup overrides it per run. |
| `shadow_color` | [r,g,b,a] | `[0.0, 0.0, 0.0, 0.45]` | Drop-shadow color. Alpha `0` = no shadow (bare text). |
| `bg_color` | [r,g,b,a] | none | Optional chip behind the text. Omitted or fully transparent = bare markup. |
| `bg_radius` | float | `0.5` | Chip corner radius as a fraction of the chip height (`0.5` = full capsule). |
| `bg_padding` | float | `0.5` | Chip padding around the text, as a fraction of the font pixel size. |

`markup` renders one block of [Pango markup](https://docs.gtk.org/Pango/pango_markup.html)
with `{variable}` substitution, over the wallpaper. It ticks about once a second and
redraws only when the substituted string changes. Style with inline `<span>` tags; wrap it
in a chip with `bg_color`.

Unlike the other text widgets, when you omit `font_size` markup uses a larger default
(0.20 of the region height) so a bare clock reads at a glance; an explicit `font_size` is
honored as-is. `content_halign` also sets Pango line justification. See the
[widgets](plugins.md) overview for the shared font, anchor, and
`debug_border` keys.

## Variables

Any `{name}` in `text` is substituted before rendering. An unknown `{name}` is left
verbatim &mdash; it never errors. Time and date tokens take an optional
[`strftime`](https://strftime.org/) spec after a colon.

| Token | Resolves to |
|---|---|
| `{time}` / `{time:%H:%M}` | Current time (default spec `%H:%M`). |
| `{date}` / `{date:%A %d %B}` | Current date (default spec `%x`). |
| `{user}` | `$USER` (or `there`). |
| `{name}` | Your GECOS full name, then `$USER`, then `there`. |
| `{host}` | The machine hostname. |
| `{uptime}` | Human uptime, e.g. `2d 3h 47m`. |
| `{loadavg}` | 1 / 5 / 15-minute load averages. |
| `{kernel}` | Kernel release (`uname -r`). |
| `{distro}` | Distro pretty name, e.g. `Ubuntu 24.04.1 LTS`. |
| `{distro_version}` | Distro version id, e.g. `24.04`. |

```toml
[plugin.config]
text = "<span size='xx-large' weight='bold'>{time:%H:%M}</span>\n<span size='large'>Hi, {name}</span>"
```

### shape — `veiland-shape`

One rounded, colored, alpha-blended rectangle filling its region: the backdrop/card primitive. Static, read-only, draws once. The veiland answer to hyprlock's shape block.
Example: [`examples/shape.toml`](examples/shape.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `color` | [r,g,b,a] | `[0.8, 0.643, 0.486, 1.0]` | Fill color of the rectangle (a warm tan by default). Alpha is the opacity, so a translucent value makes a tinted card over the wallpaper. There is no "off" &mdash; shape always paints its region. |
| `radius` | float | `0.0` | Corner radius as a fraction of the region **height**. `0` = hard corners; an over-large value clamps to half the shorter side (a pill or circle). |

`shape` is the card behind your other widgets: a semi-transparent rounded rectangle you
stack *under* a clock, greeting, or status cluster to group them visually. It draws once
and never changes &mdash; no data source, no polling.

There is no grouping primitive in veiland: you place a card by giving `shape` and the
content widget the **same region**, and giving the content a higher `z_index` so it paints
on top. Where the card sits and how big it is are set by the `[[plugin]]` region anchor
(see [configuration](config.md)); this plugin only fills whatever region it
is handed.

A `shape` does **not** blur. A frosted-glass card comes from the wallpaper's
[`blur_regions`](plugins.md) (real OpenGL blur); `shape` is a flat
translucent tint. For text or an icon on the card, stack a `markup`, `label`, or status
widget on top.

### wifi — `veiland-wifi`

Wi-Fi signal-strength glyph in a small pill, with an optional SSID label. Live from NetworkManager; bucketed into five strength levels plus an off state.
Example: [`examples/wifi.toml`](examples/wifi.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `pill_color` | [r,g,b,a] | `[0.059, 0.071, 0.11, 0.686]` | Chip background behind the glyph. Alpha is the opacity; `[0, 0, 0, 0]` draws no chip (bare glyph). |
| `icon_color` | [r,g,b,a] | white | Tints the monochrome glyph. Omit for the glyph's authored white. |
| `show_label` | bool | `false` | Show the SSID next to the glyph. Off = icon-only pill. |
| `label_color` | [r,g,b,a] | `[0.95, 0.95, 0.95, 1.0]` | SSID text color. Only the RGB is used; alpha does not hide it (use `show_label = false` for that). |
| `label_pos` | string | `"bottom"` | Where the label sits relative to the glyph: `top`, `bottom`, `left`, or `right`. |
| `label_disconnected` | string | `"N/A"` | Label text when there is no SSID (disconnected, radio off, or no device). Set to `""` to vanish the label and leave just the glyph. |

The glyph reflects live signal strength from NetworkManager, bucketed to five levels
(0/25/50/75/100 %) plus an off/disconnected state. Data is read read-only over the system
D-Bus; the widget never touches your credentials or the connection itself.

This is one of the four **status pills** &mdash; see the [widgets](plugins.md)
overview for the shared `pill_color` / `icon_color` look and the `content_halign` /
`content_valign` / `debug_border` / font keys they all accept. Cluster several pills in one
row with the [`status_cluster.toml`](examples/status_cluster.toml)
scene.

### ethernet — `veiland-ethernet`

Wired-link status glyph in a small pill: up or down. Live from NetworkManager over the system D-Bus. Icon-only, no label.
Example: [`examples/ethernet.toml`](examples/ethernet.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `pill_color` | [r,g,b,a] | `[0.059, 0.071, 0.11, 0.686]` | Chip background behind the glyph. `[0, 0, 0, 0]` draws no chip. |
| `icon_color` | [r,g,b,a] | white | Tints the glyph. Omit for white. |

The glyph shows whether the wired link is up or down, live from NetworkManager. This is the
simplest of the [status pills](plugins.md): it draws no text, so it
reads `pill_color`, `icon_color`, and the anchor / `debug_border` keys, but no label or
font keys.

### bluetooth — `veiland-bluetooth`

Bluetooth status glyph in a small pill (off / on / connected), with an optional connected-device label. Live from bluez over the system D-Bus.
Example: [`examples/bluetooth.toml`](examples/bluetooth.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `pill_color` | [r,g,b,a] | `[0.059, 0.071, 0.11, 0.686]` | Chip background behind the glyph. `[0, 0, 0, 0]` draws no chip. |
| `icon_color` | [r,g,b,a] | white | Tints the glyph. Omit for white. |
| `show_label` | bool | `false` | Show the connected device name. Off = icon-only pill. |
| `label_color` | [r,g,b,a] | `[0.95, 0.95, 0.95, 1.0]` | Label text color (RGB only; alpha does not hide it). |
| `label_pos` | string | `"bottom"` | Label position relative to the glyph: `top`, `bottom`, `left`, or `right`. |
| `label_disconnected` | string | `"N/A"` | Label text when nothing is connected. Set to `""` for glyph-only. |

The glyph has three states &mdash; off, on-but-idle, and connected &mdash; read live from
bluez. With `show_label` on, a connected device's name appears beside it. See the
[widgets](plugins.md) overview for the shared `pill_color` /
`icon_color`, anchor, font, and `debug_border` keys.

### battery — `veiland-battery`

Battery status drawn from bucketed SVG icons in a small pill, with an optional percent + charging-state label. Reads /sys/class/power_supply. The template for the status-icon pattern.
Example: [`examples/battery_svg.toml`](examples/battery_svg.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `pill_color` | [r,g,b,a] | `[0.059, 0.071, 0.11, 0.686]` | Chip background behind the glyph. `[0, 0, 0, 0]` draws no chip. |
| `icon_color` | [r,g,b,a] | white | Tints the glyph. Omit for white. |
| `show_label` | bool | `false` | Show the percent + state text (e.g. `64% Discharging`, or `AC` on a desktop with no battery). Off = icon-only pill. |
| `label_color` | [r,g,b,a] | `[0.95, 0.95, 0.95, 1.0]` | Label text color (RGB only; alpha does not hide it). |
| `label_pos` | string | `"bottom"` | Label position relative to the glyph: `top`, `bottom`, `left`, or `right`. |

The glyph is chosen from bucketed SVG icons by charge level and charging state, read from
`/sys/class/power_supply`. A machine with no battery shows the "AC" glyph. This widget is
also the copy-me starting point for writing your own status-icon pill.

See the [widgets](plugins.md) overview for the shared `pill_color` /
`icon_color`, anchor, font, and `debug_border` keys.

## The stress plugin

Not a lockscreen plugin. `stress` is a load generator used to benchmark the
render→IPC→composite round trip; it burns GPU on a deliberately heavy shader,
renders a fixed 1920×1080 buffer, ignores its assigned region by design, and
prints frame timings to stderr. It reads no `[plugin.config]` keys at all —
its knobs are compile-time constants. Leave it out of real configs.

## Pitfalls

- **A single wrong *type* discards the whole table.** If you write
  `count = "40"` (a string), the plugin can't parse the config as a whole and
  silently runs with **all** keys at their defaults — not just `count`. The
  warning goes to stderr, which you won't see on a lockscreen; test scenes
  from a terminal first.
- **Misspelled keys don't warn.** Unknown keys are ignored, so a typo looks
  like "the setting does nothing." Compare against the property tables.
- **Colors are `0.0`–`1.0` floats, not `0`–`255` and not hex.**
  `color = [255, 128, 0, 255]` won't error — it's just wildly out of range.
  Divide by 255. And note the `[password]` table in the core config uses
  CSS-style strings (`"rgba(...)"`) — the two syntaxes don't mix.
- **`count` doesn't scale with resolution.** A field tuned on a laptop screen
  looks sparser on a 4K monitor of the same physical size; bump `count` per
  scene, not per plugin default.
- **Text sizes are fractions, not points.** `font_size = 24` is 24× the
  widget's region height. You want values like `0.1`–`0.7`.
- **Integer-valued floats are fine either way** — TOML `22` and `22.0` both
  parse for float keys via JSON. Type strictness bites on strings-vs-numbers,
  not int-vs-float.

## See also

- The [configuration reference](config.md) — the core schema:
  plugin entries, regions, z-order, monitors, and the `[password]` field.
- [`docs/examples/`](examples/)
  — complete working scenes using these keys.
- [Writing plugins](plugin-api.md) — for building your own.
