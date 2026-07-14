<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Veiland plugin reference

Every first-party plugin, its config keys, types, and defaults —
extracted from the plugin sources. This is the companion to
[`config.md`](config.md): that document covers the core schema
(`name`, `binary`, `z_index`, `region`, `monitors`, `[password]`);
this one covers what goes *inside* each plugin's `[plugin.config]`
table.

The complete working scenes in [`docs/examples/`](examples/) use
these keys; the README gallery shows what each scene looks like.

## 1. How plugin options work

Plugin options live in the `[plugin.config]` table of a `[[plugin]]`
entry. The host passes the table through to the plugin process
verbatim (see [`config.md` §3](config.md)); the keys below are each
plugin's own schema.

```toml
[[plugin]]
name = "sakura"
binary = "/usr/bin/veiland-sakura"
z_index = 25

[plugin.config]
count = 40
size_px = 26.0
color = [1.0, 0.9, 0.95, 0.9]
```

Conventions shared by all first-party plugins:

- **Colors are float arrays, not hex strings.** `[r, g, b]` or
  `[r, g, b, a]`, each component `0.0`–`1.0`. There is no
  `"#rrggbb"` or `"rgba(...)"` form here — that string syntax
  belongs to the core's `[password]` table only.
- **Every key is optional.** An omitted key falls back to its
  default, so a `[plugin.config]` table with one key is fine, and
  no table at all gives you the plugin's stock look.
- **Bad config never crashes anything.** If the table fails to
  parse as a whole — including one key of the wrong *type* — the
  plugin logs a warning to stderr and runs with **all** defaults
  (there is no partial recovery). Out-of-range values are clamped
  or replaced per key where the plugin validates them.
- **Misspelled keys are silently ignored.** `raduis_px = 60` is not
  an error; the plugin just never sees it and uses the default. If
  a setting seems to have no effect, check the spelling first.
- **Sizes ending in `_px` are logical pixels.** They are multiplied
  by the output scale, so one value renders the same physical size
  on 1× and HiDPI monitors. The text plugins (clock, label) use a
  different model — fractions of the surface — described in §5.

## 2. Backgrounds

Opaque, full-region plugins meant for the bottom of the stack
(low `z_index`).

### wallpaper — `veiland-wallpaper`

Displays a single image, stretched to fill the region.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `path` | string | `""` | Absolute path to the image file. |

- **JPEG and PNG only**, detected by file content, not extension.
- Any failure — empty path, missing file, unreadable, unsupported
  format, decode error — logs the reason and renders **solid
  black** instead. A bad wallpaper path never breaks the lock.
- The image is stretched to the region (no cover/contain modes),
  so pick an image matching your monitor's aspect ratio. Decoding
  runs on a worker thread; the first frames may be black before
  the image pops in.
- Remember pitfall from `config.md`: no `~` or `$HOME` expansion —
  use a full absolute path.

### gradient — `veiland-gradient`

A slow-flowing, seamlessly looping multi-stop color gradient,
optionally with a rotating axis.
Example: [`examples/gradient.toml`](examples/gradient.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `colors` | array of `[r,g,b]` | indigo, purple, teal | 2–4 ramp stops; extras beyond 4 are ignored. |
| `angle_deg` | float | `45.0` | Gradient axis. `0` = left-to-right, positive rotates clockwise. |
| `speed` | float | `0.25` | Ramp loop speed in cycles per minute (`0.25` = one loop every 4 minutes). `0` freezes it. |
| `rotate_deg_per_min` | float | `0.0` | Axis rotation in degrees per minute. `0` = fixed axis. Clamped to ±360. |
| `scale` | float | `0.75` | Ramp lengths per screen height. Smaller = broader, softer bands. Clamped to 0.05–10. |

Default stops: `[[0.10, 0.16, 0.42], [0.38, 0.12, 0.48], [0.05, 0.36, 0.44]]`.
Fewer than 2 valid stops falls back to that default palette.
`speed` is clamped to 0–30 cycles/min.

### blobs — `veiland-blobs`

Large soft metaballs drifting on slow orbits over a dark
background — the lava-lamp look.
Example: [`examples/blobs.toml`](examples/blobs.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `colors` | array of `[r,g,b]` | blue, magenta, teal, amber | Blob palette, 1–8 colors, cycled across blobs. |
| `background` | `[r,g,b]` | `[0.02, 0.03, 0.08]` | The color the blobs float over. |
| `count` | integer | `6` | Number of blobs. Clamped to 1–8. |
| `size` | float | `0.25` | Base blob radius as a fraction of screen height (each blob varies ±30% around it). Clamped to 0.05–1.0; past ~0.35 the field saturates. |
| `speed` | float | `1.0` | Drift-speed multiplier; `1.0` is one slow orbit over a couple of minutes, `0` freezes the field. Clamped to 0–10. |
| `softness` | float | `0.6` | Edge falloff. Lower = tighter cores and darker gaps, higher = hazier until blobs wash together. Clamped to 0.25–4. |
| `seed` | integer | `2654435769` | Layout/motion seed; change it for a different arrangement. |

Default palette: `[[0.12, 0.20, 0.55], [0.45, 0.15, 0.50], [0.05, 0.42, 0.45], [0.50, 0.28, 0.12]]`.
Fewer colors than blobs just cycles the palette. The motion never
visibly repeats.

### raymarcher — `veiland-raymarcher`

A slow camera drift through infinite raymarched gyroid tunnels.
The scene itself is fixed — there is one tunnel geometry and no
scene-selection key; you steer the palette, fog, and pace.
Example: [`examples/raymarcher.toml`](examples/raymarcher.toml)
(this is also the default scene when you have no config file).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `colors` | array of `[r,g,b]` | indigo, amber, teal | 2–4 palette stops; the first also tints the fog. |
| `speed` | float | `1.0` | Drift speed; `1.0` crosses one tunnel cell every ~18 s, `0` freezes the camera. Clamped to 0–10. |
| `fov_deg` | float | `70.0` | Vertical field of view in degrees. Clamped to 30–110. |
| `fog` | float | `1.0` | Fog-density multiplier. `0` works but the fog also hides the far draw boundary, so very low values are not recommended. Clamped to 0–4. |
| `render_scale` | float | `0.5` | Internal resolution as a fraction of the region; the host upscales. `0.5` costs a quarter of the rays of native. Clamped to 0.1–1.0. |
| `max_fps` | float | `30.0` | Frame-rate cap. `0` = uncapped (compositor rate). Clamped to 0–240. |

Default stops: `[[0.08, 0.10, 0.18], [0.55, 0.30, 0.15], [0.20, 0.35, 0.40]]`.
The two thermal knobs (`render_scale`, `max_fps`) are conservative
by default — raise them if you have GPU headroom and want a
sharper, smoother tunnel.

## 3. Overlays

Transparent plugins meant to sit above a background and below
text.

### vignette — `veiland-vignette`

Darkens the corners (and optionally the whole frame) with a soft
radial gradient. Static — costs nearly nothing.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `color` | `[r,g,b,a]` | `[0.10, 0.14, 0.20, 1.0]` | Vignette tint; the alpha is a master intensity multiplier. |
| `opacity_top_left` | float | `0.6` | Strength of the top-left corner. |
| `opacity_top_right` | float | `0.6` | Strength of the top-right corner. |
| `opacity_bottom_left` | float | `0.7` | Strength of the bottom-left corner. |
| `opacity_bottom_right` | float | `0.7` | Strength of the bottom-right corner. |
| `radius` | float | `0.7` | How far each corner's shading reaches toward the center, as a fraction of the half-diagonal. |
| `base_opacity` | float | `0.0` | Uniform dim over the whole frame, under the corners. `0.15`–`0.3` gives a soft haze; `0` is the classic corners-only look. |

The bottom corners default slightly stronger than the top — that's
where wallpapers tend to be brightest. The summed opacity saturates
at fully opaque rather than overflowing, so generous values are
safe.

### parallax — `veiland-parallax`

Three depth layers of soft bokeh circles drifting at different
speeds — a subtle depth cue over any background. Fully procedural:
no image files involved.
Example: [`examples/parallax.toml`](examples/parallax.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `color` | `[r,g,b,a]` | `[1.0, 1.0, 1.0, 0.2]` | Circle color; the alpha is the master opacity of the whole effect. |
| `size_px` | float | `80.0` | Max circle radius of the *near* layer, in logical px; the deeper layers scale down from it. Clamped to 4–512. |
| `density` | float | `0.5` | Fraction of the layout grid that holds a circle, 0–1. |
| `speed` | float | `8.0` | Near-layer drift in px/s; deeper layers move slower. Clamped to 0–200. |
| `angle_deg` | float | `30.0` | Drift direction; `0` = rightward, `90` = upward. |
| `softness` | float | `0.5` | Edge feather as a fraction of the radius. `1.0` = fully soft bokeh, small values = crisp dots. Clamped to 0.02–1. |
| `seed` | integer | `2654435769` | Layout seed; change it to reshuffle all three layers. |

The layer ratios (size, speed, and opacity per depth) are fixed.

## 4. Particles

Six variations on one idea: a field of independent particles
drifting across a transparent buffer, composited over your
background. They share two keys — `count` and a color — plus one
size key each; the motion itself (sway, timing, fades) is tuned
per effect and not configurable.

`count` is an absolute number, not a density: the same value puts
the same number of particles on a 1080p and a 4K monitor. Sizes
(`*_px`) do scale with the output, so the particles themselves stay
the same physical size.

### particles — `veiland-particles`

Small soft glowing motes drifting slowly **upward** — the only
riser in the family. Used in
[`examples/shinkai.toml`](examples/shinkai.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `count` | integer | `40` | Number of motes. |
| `color` | `[r,g,b,a]` | `[1.0, 1.0, 1.0, 0.5]` | Mote color. |
| `radius_px` | float | `0.4` | Core radius in logical px. Deliberately tiny — a soft glow halo about 3× the core does the visible work, so small changes go a long way. |

### sakura — `veiland-sakura`

Falling, swaying, tumbling cherry-blossom petals, drawn from a
built-in petal texture.
Example: [`examples/sakura.toml`](examples/sakura.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `count` | integer | `25` | Number of petals. |
| `color` | `[r,g,b,a]` | `[1.0, 1.0, 1.0, 1.0]` | A *tint* multiplied into the petal texture — the petals are already pink, so white means "as-is". Lower the alpha to fade the whole field. |
| `size_px` | float | `22.0` | Petal size in logical px. |

### snow — `veiland-snow`

A few large procedural snow crystals — six-fold dendritic flakes,
each uniquely shaped — drifting down with a slow tumble.
Example: [`examples/snow.toml`](examples/snow.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `count` | integer | `12` | Number of crystals. Deliberately low — the detail needs room. |
| `color` | `[r,g,b,a]` | `[1.0, 1.0, 1.0, 0.9]` | Crystal color. |
| `radius_px` | float | `60.0` | Crystal radius in logical px. Below ~40 the fern structure collapses into a dot — this effect wants *few and large*, not a dense flurry. |

### rain — `veiland-rain`

Wind-slanted rain streaks with depth: near drops are longer,
faster, and brighter than far ones.
Example: [`examples/rain.toml`](examples/rain.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `count` | integer | `90` | Number of drops — rain is a volume, so the default is the densest in the family. |
| `color` | `[r,g,b,a]` | `[0.72, 0.80, 0.95, 0.65]` | Drop color (cool translucent blue-grey); alpha sets the brightness of the *nearest* drops. |
| `length_px` | float | `36.0` | Streak length in logical px for the nearest drops; farther drops shrink automatically. |
| `slant_deg` | float | `10.0` | Shared wind angle in degrees from vertical; positive leans the fall rightward. The only configurable wind in the family — all drops share it, so the rain falls as a coherent sheet. |

### embers — `veiland-embers`

A warm glow band along the bottom edge with bright sparks rising,
curving, and fading as they climb.
Example: [`examples/embers.toml`](examples/embers.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `count` | integer | `80` | Number of sparks. |
| `spark_color` | `[r,g,b,a]` | `[1.0, 0.65, 0.10, 1.0]` | Spark color (hot core; the halo reuses it dimmer). |
| `glow_color` | `[r,g,b]` | `[0.80, 0.18, 0.02]` | Color of the bottom glow band. Note: three components, no alpha — the band's strength and height (bottom ~30% of the region) are fixed. |

### fireflies — `veiland-fireflies`

Softly glowing lights wandering on lazy paths, each blinking on
its own rhythm.
Example: [`examples/fireflies.toml`](examples/fireflies.toml).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `count` | integer | `25` | Number of fireflies. |
| `color` | `[r,g,b,a]` | `[0.72, 1.0, 0.18, 0.95]` | Glow color (warm yellow-green); alpha is the peak flash brightness. |
| `radius_px` | float | `2.5` | Core radius in logical px; the visible halo extends about 4× beyond it. |
| `flash_sharpness` | float | `0.4` | Blink character, 0–1: `0` = gentle continuous pulsing, `1` = brief sharp flashes with long dark gaps. |

## 5. Text

Both text plugins position and size themselves as **fractions of
the surface**, not pixels: a `font_size` of `0.03` is 3% of the
surface height (~32 px on 1080p, ~65 px on 4K), and a `position` of
`[0.5, 0.5]` is the center. One config therefore looks the same on
any monitor. Colors are `[r,g,b,a]` floats like everywhere else.

Shared font behavior:

- `font_family` accepts `"Sans"`, `"Serif"`, `"Monospace"`, or any
  installed system family name (e.g. `"JetBrains Mono"`,
  `"Noto Sans CJK JP"`). Unknown names fall back to the system
  sans-serif.
- `font_weight` is the CSS numeric scale: `100` thin, `300` light,
  `400` normal, `700` bold. Missing weights fall back to the
  nearest face the family has.
- `shadow_offset = [x, y]` enables a drop shadow (each component a
  fraction of surface height). `shadow_blur` is accepted but **not
  implemented yet** — any value draws a sharp-edged shadow and logs
  a one-time warning.
- Letter-spacing keys add tracking as a fraction of the font size.

### clock — `veiland-clock`

The current time and date as two independently styled labels. Time
comes from the host (the plugin never reads the system clock) and
follows your timezone.

| Key | Type | Default | Meaning |
|---|---|---|---|
| `time_format` | string | `"%H:%M"` | [chrono `strftime`](https://docs.rs/chrono/latest/chrono/format/strftime/) pattern for the time. `"%I:%M %p"` for 12-hour. |
| `date_format` | string | `"%B %d, %Y"` | `strftime` pattern for the date line. |
| `font_family` | string | `"Sans"` | Family for both labels. |
| `font_weight` | integer | `400` | Weight for both labels. |
| `time_font_size` | float | `0.067` | Time size, fraction of surface height (~7%). |
| `date_font_size` | float | `0.013` | Date size, fraction of surface height. |
| `time_color` | `[r,g,b,a]` | `[0.91, 0.96, 0.97, 0.9]` | Time color. |
| `date_color` | `[r,g,b,a]` | `[0.66, 0.84, 0.91, 0.6]` | Date color. |
| `time_position` | `[x, y]` | `[0.026, 0.046]` | Time anchor, fractions of the surface. |
| `date_position` | `[x, y]` | `[0.026, 0.150]` | Date anchor. |
| `halign` | `"left"` / `"center"` / `"right"` | `"left"` | Which horizontal edge of the text sits on the anchor (both labels). |
| `valign` | `"top"` / `"middle"` / `"bottom"` | `"top"` | Vertical counterpart. |
| `time_letter_spacing` | float | `0.0` | Extra tracking for the time, fraction of its font size. |
| `date_letter_spacing` | float | `0.0` | Extra tracking for the date. |
| `shadow_offset` | `[x, y]` or absent | absent | Set to enable a drop shadow on both labels. |
| `shadow_color` | `[r,g,b,a]` | `[0.0, 0.0, 0.0, 0.9]` | Shadow color. |
| `shadow_blur` | float | `0.0` | Reserved; draws sharp for now. |

An invalid `strftime` pattern doesn't error — chrono renders the
unrecognized parts literally, so if you see stray `%q` on your
lockscreen, check the pattern.

### label — `veiland-label`

One static styled text label — names, quotes, kaomoji, vertical
captions. Run several instances for several labels
(see [`examples/label.toml`](examples/label.toml) and the shinkai
scene).

| Key | Type | Default | Meaning |
|---|---|---|---|
| `text` | string | `"veiland-label (no [plugin.config] set)"` | The text. Deliberately loud when unconfigured so you notice. |
| `font_family` | string | `"Sans"` | Font family. |
| `font_weight` | integer | `400` | Weight. |
| `italic` | bool | `false` | Use the family's italic face. Families without one (many CJK fonts) render upright — no fake slant is synthesized. |
| `font_size` | float | `0.030` | Size, fraction of surface height (~3%). |
| `color` | `[r,g,b,a]` | `[1.0, 1.0, 1.0, 1.0]` | Text color. |
| `position` | `[x, y]` | `[0.5, 0.5]` | Anchor, fractions of the surface (default: dead center). |
| `halign` | `"left"` / `"center"` / `"right"` | `"center"` | Horizontal edge on the anchor. Note the default differs from clock. |
| `valign` | `"top"` / `"middle"` / `"bottom"` | `"middle"` | Vertical counterpart. |
| `rotation` | float | `0.0` | Counter-clockwise rotation in degrees around the anchor — vertical spine text is `90` or `-90`. |
| `letter_spacing` | float | `0.0` | Extra tracking, fraction of the font size. |
| `shadow_offset` | `[x, y]` or absent | absent | Set to enable a drop shadow. |
| `shadow_color` | `[r,g,b,a]` | `[0.0, 0.0, 0.0, 0.6]` | Shadow color. |
| `shadow_blur` | float | `0.0` | Reserved; draws sharp for now. |

## 6. stress — `veiland-stress`

Not a lockscreen plugin. `stress` is a load generator used to
benchmark the render→IPC→composite round trip; it burns GPU on a
deliberately heavy shader, renders a fixed 1920×1080 buffer,
ignores its assigned region by design, and prints frame timings to
stderr. It reads no `[plugin.config]` keys at all — its knobs are
compile-time constants. Leave it out of real configs.

## 7. Pitfalls

- **A single wrong *type* discards the whole table.** If you write
  `count = "40"` (a string), the plugin can't parse the config as a
  whole and silently runs with **all** keys at their defaults — not
  just `count`. The warning goes to stderr, which you won't see on
  a lockscreen; test scenes from a terminal first.
- **Misspelled keys don't warn.** Unknown keys are ignored, so a
  typo looks like "the setting does nothing." Compare against the
  tables above.
- **Colors are `0.0`–`1.0` floats, not `0`–`255` and not hex.**
  `color = [255, 128, 0, 255]` won't error — it's just wildly out
  of range. Divide by 255. And note the `[password]` table in the
  core config uses CSS-style strings (`"rgba(...)"`) — the two
  syntaxes don't mix.
- **`count` doesn't scale with resolution.** A field tuned on a
  laptop screen looks sparser on a 4K monitor of the same physical
  size; bump `count` per scene, not per plugin default.
- **Text sizes are fractions, not points.** `font_size = 24` is
  24× the surface height. You want values like `0.02`–`0.10`.
- **Integer-valued floats are fine either way** — TOML `22` and
  `22.0` both parse for float keys via JSON. Type strictness bites
  on strings-vs-numbers, not int-vs-float.

## 8. See also

- [`config.md`](config.md) — the core config schema: plugin
  entries, regions, z-order, monitors, and the `[password]` field.
- [`docs/examples/`](examples/) — complete working scenes using
  the keys above, one per README gallery entry.
- [`plugin-api.md`](plugin-api.md) — for writing your own plugin.
