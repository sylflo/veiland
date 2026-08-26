+++
title = "Plugins"
template = "plugins-section.html"
sort_by = "weight"

[extra]
[[extra.categories]]
name = "backgrounds"
desc = "opaque, full-region layers for the bottom of the stack"
notes = "Meant for the bottom of the stack (low `z_index`)."

[[extra.categories]]
name = "overlays"
desc = "transparent layers that sit above a background, below text"
notes = "Transparent plugins meant to sit above a background and below text."

[[extra.categories]]
name = "particles"
desc = "one idea, six moods; count is absolute, sizes scale with the output"
notes = """
Six variations on one idea: a field of independent particles drifting across a
transparent buffer, composited over your background. They share two keys — `count`
and a color — plus one size key each; the motion itself (sway, timing, fades) is
tuned per effect and not configurable.

`count` is an absolute number, not a density: the same value puts the same number
of particles on a 1080p and a 4K monitor. Sizes (`*_px`) do scale with the output,
so the particles themselves stay the same physical size.
"""

[[extra.categories]]
name = "widgets"
desc = "glanceable info panels and cards; the reference widgets are Python programs"
notes = """
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
  `font_size` is a fraction of the widget's box. The status pills and `markup`
  use the full set. `avatar`, `now-playing`, and `weather` derive their text
  size from geometry, so they honor only `font_family` + `italic`. `shape` and
  `ethernet` draw no text and read no font keys.
- **Debug border** — `debug_border = true` strokes a 1px outline around the
  region (color `debug_border_color`, default bright magenta) so you can see
  and tune the anchor. Honored by every widget except `shape`.

**Status pills** (`wifi`, `ethernet`, `bluetooth`, `battery`) share a look: a
monochrome glyph in a small translucent chip. They all take `pill_color`
(chip background, default a translucent dark navy; `[0,0,0,0]` draws no chip)
and `icon_color` (glyph tint, default white). Each pill page lists only what
is unique to it &mdash; the label keys and data source.
"""
+++

## How plugin options work

Plugin options live in the `[plugin.config]` table of a `[[plugin]]` entry. The
host passes the table through to the plugin process verbatim (see the
[configuration reference](@/docs/configuration.md)); the keys documented per
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

- The [configuration reference](@/docs/configuration.md) — the core schema:
  plugin entries, regions, z-order, monitors, and the `[password]` field.
- [`docs/examples/`](https://github.com/sylflo/veiland/tree/master/docs/examples)
  — complete working scenes using these keys.
- [Writing plugins](@/docs/writing-plugins.md) — for building your own.
