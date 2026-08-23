+++
title = "avatar"
description = "The user's picture cover-cropped into a disc (or rounded square), or a tinted initials disc when no picture is found. Zero-config: reads your account picture automatically."
weight = 51
template = "docs-page.html"

[extra]
binary = "veiland-avatar"
one_liner = "profile disc"
category = "widgets"
image = "previews/avatar.png"
example = "avatar.toml"

[[extra.props]]
key = "name"
type = "string"
default = "your full name"
meaning = "Seeds the initials letter and the disc's tint. Defaults to your GECOS full name, then `$USER`, then `there`."

[[extra.props]]
key = "avatar"
type = "string"
default = "`~/.face`"
meaning = "Path to a picture (`~` expanded), cover-cropped into the disc. Missing or unreadable falls back to `~/.face`, then to a tinted initials disc."

[[extra.props]]
key = "shape"
type = "string"
default = "`\"circle\"`"
meaning = "Disc outline: `circle` or `rounded` (a rounded square)."

[[extra.props]]
key = "ring_color"
type = "[r,g,b,a]"
default = "`[1.0, 1.0, 1.0, 0.22]`"
meaning = "Thin rim around the disc. Alpha `0` draws no ring."
+++

`avatar` shows just the profile disc &mdash; pair it with a [`markup`](@/docs/plugins/markup.md)
greeting for a name/welcome line beside it. With no config it uses your account picture
(`~/.face`) and full name automatically, so it works out of the box.

Only `font_family` and `italic` affect the initials letter; its size comes from the disc
diameter, so `font_size` and `font_weight` have no effect here. See the
[widgets](@/docs/plugins/_index.md) overview for the `content_halign` /
`content_valign` / `debug_border` keys.
