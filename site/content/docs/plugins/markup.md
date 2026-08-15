+++
title = "markup"
description = "One block of Pango markup with {variable} substitution, composited over the wallpaper. The dynamic styled-text widget: a clock, a greeting, a live system-info line."
weight = 54
template = "docs-page.html"

[extra]
binary = "veiland-markup"
one_liner = "dynamic styled text"
category = "widgets"
example = "markup.toml"

[[extra.props]]
key = "text"
type = "string"
default = "a bold time + date block"
meaning = "The template: Pango `<span>` markup plus `{variable}` placeholders (see the token table below). Newlines allowed."

[[extra.props]]
key = "text_color"
type = "[r,g,b,a]"
default = "`[1.0, 1.0, 1.0, 0.96]`"
meaning = "Base text fill. An inline `<span color=...>` in the markup overrides it per run."

[[extra.props]]
key = "shadow_color"
type = "[r,g,b,a]"
default = "`[0.0, 0.0, 0.0, 0.45]`"
meaning = "Drop-shadow color. Alpha `0` = no shadow (bare text)."

[[extra.props]]
key = "bg_color"
type = "[r,g,b,a]"
default = "none"
meaning = "Optional chip behind the text. Omitted or fully transparent = bare markup."

[[extra.props]]
key = "bg_radius"
type = "float"
default = "`0.5`"
meaning = "Chip corner radius as a fraction of the chip height (`0.5` = full capsule)."

[[extra.props]]
key = "bg_padding"
type = "float"
default = "`0.5`"
meaning = "Chip padding around the text, as a fraction of the font pixel size."
+++

`markup` renders one block of [Pango markup](https://docs.gtk.org/Pango/pango_markup.html)
with `{variable}` substitution, over the wallpaper. It ticks about once a second and
redraws only when the substituted string changes. Style with inline `<span>` tags; wrap it
in a chip with `bg_color`.

Unlike the other text widgets, when you omit `font_size` markup uses a larger default
(0.20 of the region height) so a bare clock reads at a glance; an explicit `font_size` is
honored as-is. `content_halign` also sets Pango line justification. See the
[widgets](@/docs/plugins/_index.md) overview for the shared font, anchor, and
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
