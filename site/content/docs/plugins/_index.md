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
name = "text"
desc = "sizes and positions are fractions of the surface; one config looks the same on any monitor"
notes = """
Both text plugins position and size themselves as **fractions of the surface**,
not pixels: a `font_size` of `0.03` is 3% of the surface height (~32 px on 1080p,
~65 px on 4K), and a `position` of `[0.5, 0.5]` is the center. One config
therefore looks the same on any monitor. Colors are `[r,g,b,a]` floats like
everywhere else.

Shared font behavior:

- `font_family` accepts `"Sans"`, `"Serif"`, `"Monospace"`, or any installed
  system family name (e.g. `"JetBrains Mono"`, `"Noto Sans CJK JP"`). Unknown
  names fall back to the system sans-serif.
- `font_weight` is the CSS numeric scale: `100` thin, `300` light, `400` normal,
  `700` bold. Missing weights fall back to the nearest face the family has.
- `shadow_offset = [x, y]` enables a drop shadow (each component a fraction of
  surface height). `shadow_blur` is accepted but **not implemented yet** — any
  value draws a sharp-edged shadow and logs a one-time warning.
- Letter-spacing keys add tracking as a fraction of the font size.
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
  monitors. The text plugins (clock, label) use a different model — fractions
  of the surface — described under their category.

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
  surface height. You want values like `0.02`–`0.10`.
- **Integer-valued floats are fine either way** — TOML `22` and `22.0` both
  parse for float keys via JSON. Type strictness bites on strings-vs-numbers,
  not int-vs-float.

## See also

- The [configuration reference](@/docs/configuration.md) — the core schema:
  plugin entries, regions, z-order, monitors, and the `[password]` field.
- [`docs/examples/`](https://github.com/sylflo/veiland/tree/master/docs/examples)
  — complete working scenes using these keys.
- [Writing plugins](@/docs/writing-plugins.md) — for building your own.
