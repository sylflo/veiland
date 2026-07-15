+++
title = "clock"
description = "The current time and date as two independently styled labels. Time comes from the host, the plugin never reads the system clock, and it follows your timezone."
weight = 40
template = "docs-page.html"

[extra]
one_liner = "time + date"
category = "text"
image = "previews/clock.png"
used_in = "shinkai.toml"

[[extra.props]]
key = "time_format"
type = "string"
default = "`\"%H:%M\"`"
meaning = "[chrono strftime](https://docs.rs/chrono/latest/chrono/format/strftime/) pattern for the time. `\"%I:%M %p\"` for 12-hour."

[[extra.props]]
key = "date_format"
type = "string"
default = "`\"%B %d, %Y\"`"
meaning = "strftime pattern for the date line."

[[extra.props]]
key = "font_family"
type = "string"
default = "`\"Sans\"`"
meaning = "`\"Sans\"`, `\"Serif\"`, `\"Monospace\"`, or any installed family name. Unknown names fall back to the system sans."

[[extra.props]]
key = "font_weight"
type = "integer"
default = "`400`"
meaning = "CSS numeric scale: 100 thin, 300 light, 400 normal, 700 bold."

[[extra.props]]
key = "time_font_size"
type = "float"
default = "`0.067`"
meaning = "Time size, fraction of surface height (~7%)."

[[extra.props]]
key = "date_font_size"
type = "float"
default = "`0.013`"
meaning = "Date size, fraction of surface height."

[[extra.props]]
key = "time_color"
type = "[r,g,b,a]"
default = "`[0.91, 0.96, 0.97, 0.9]`"
meaning = "Time color."

[[extra.props]]
key = "date_color"
type = "[r,g,b,a]"
default = "`[0.66, 0.84, 0.91, 0.6]`"
meaning = "Date color."

[[extra.props]]
key = "time_position"
type = "[x, y]"
default = "`[0.026, 0.046]`"
meaning = "Time anchor, fractions of the surface."

[[extra.props]]
key = "date_position"
type = "[x, y]"
default = "`[0.026, 0.150]`"
meaning = "Date anchor."

[[extra.props]]
key = "halign"
type = "string"
default = "`\"left\"`"
meaning = "Which horizontal edge of the text sits on the anchor, for both labels: `\"left\"` / `\"center\"` / `\"right\"`."

[[extra.props]]
key = "valign"
type = "string"
default = "`\"top\"`"
meaning = "Vertical counterpart: `\"top\"` / `\"middle\"` / `\"bottom\"`."

[[extra.props]]
key = "time_letter_spacing"
type = "float"
default = "`0.0`"
meaning = "Extra tracking for the time, fraction of its font size."

[[extra.props]]
key = "date_letter_spacing"
type = "float"
default = "`0.0`"
meaning = "Extra tracking for the date."

[[extra.props]]
key = "shadow_offset"
type = "[x, y]"
default = "absent"
meaning = "Set to enable a drop shadow on both labels; each component is a fraction of surface height."

[[extra.props]]
key = "shadow_color"
type = "[r,g,b,a]"
default = "`[0.0, 0.0, 0.0, 0.9]`"
meaning = "Shadow color."

[[extra.props]]
key = "shadow_blur"
type = "float"
default = "`0.0`"
meaning = "Reserved; any value draws a sharp-edged shadow for now and logs a one-time warning."
+++

The current time and date as two independently styled labels. Time comes from the host, the plugin never reads the system clock, and it follows your timezone.

An invalid strftime pattern does not error; chrono renders the unrecognized parts
literally. If you see a stray `%q` on your lockscreen, check the pattern.
