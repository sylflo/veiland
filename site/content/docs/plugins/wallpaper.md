+++
title = "wallpaper"
description = "Displays one JPEG or PNG, stretched to fill its region. Any failure logs the reason and renders solid black. A wrong path never breaks the lock."
weight = 10
template = "docs-page.html"

[extra]
one_liner = "a single image"
category = "backgrounds"
image = "previews/wallpaper.jpg"
used_in = "sakura.toml"

[[extra.props]]
key = "path"
type = "string"
default = "`\"\"`"
meaning = "Absolute path to the image (no `~` expansion). JPEG/PNG, detected by content."
+++

Displays one JPEG or PNG, stretched to fill its region. Any failure logs the reason and renders solid black. A wrong path never breaks the lock.

The image is stretched to the region with no cover or contain modes, so pick an image
matching your monitor's aspect ratio. Decoding runs on a worker thread; the first frames
may be black before the image pops in.

Remember the pitfall from [configuration](@/docs/configuration.md): asset paths get no `~`
or `$HOME` expansion, so always give a full absolute path.
