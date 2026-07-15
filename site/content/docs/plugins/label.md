+++
title = "label"
description = "One static styled text label: names, quotes, kaomoji, vertical captions. Run several instances for several labels."
weight = 41
template = "docs-page.html"

[extra]
one_liner = "any styled text"
category = "text"
image = "previews/label.png"
example = "label.toml"

[[extra.props]]
key = "text"
type = "string"
default = "(placeholder)"
meaning = "The text. Deliberately loud when unconfigured so you notice."

[[extra.props]]
key = "font_family"
type = "string"
default = "`\"Sans\"`"
meaning = "Any installed family name, including CJK fonts (e.g. `\"Noto Sans CJK JP\"`)."

[[extra.props]]
key = "font_weight"
type = "integer"
default = "`400`"
meaning = "CSS numeric scale."

[[extra.props]]
key = "italic"
type = "bool"
default = "`false`"
meaning = "Use the family's italic face. Families without one (many CJK fonts) render upright; no fake slant is synthesized."

[[extra.props]]
key = "font_size"
type = "float"
default = "`0.030`"
meaning = "Size, fraction of surface height (~3%)."

[[extra.props]]
key = "color"
type = "[r,g,b,a]"
default = "`[1.0, 1.0, 1.0, 1.0]`"
meaning = "Text color."

[[extra.props]]
key = "position"
type = "[x, y]"
default = "`[0.5, 0.5]`"
meaning = "Anchor, fractions of the surface; the default is dead center."

[[extra.props]]
key = "halign"
type = "string"
default = "`\"center\"`"
meaning = "Horizontal edge on the anchor. Note the default differs from clock: `\"left\"` / `\"center\"` / `\"right\"`."

[[extra.props]]
key = "valign"
type = "string"
default = "`\"middle\"`"
meaning = "`\"top\"` / `\"middle\"` / `\"bottom\"`."

[[extra.props]]
key = "rotation"
type = "float"
default = "`0.0`"
meaning = "Counter-clockwise rotation in degrees around the anchor; vertical spine text is `90` or `-90`."

[[extra.props]]
key = "letter_spacing"
type = "float"
default = "`0.0`"
meaning = "Extra tracking, fraction of the font size."

[[extra.props]]
key = "shadow_offset"
type = "[x, y]"
default = "absent"
meaning = "Set to enable a drop shadow."

[[extra.props]]
key = "shadow_color"
type = "[r,g,b,a]"
default = "`[0.0, 0.0, 0.0, 0.6]`"
meaning = "Shadow color."

[[extra.props]]
key = "shadow_blur"
type = "float"
default = "`0.0`"
meaning = "Reserved; draws sharp for now."
+++

The shinkai example scene runs four label instances at once: two titles and two quotes,
in two languages.
