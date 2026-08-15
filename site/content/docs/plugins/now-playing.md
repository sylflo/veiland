+++
title = "now-playing"
description = "A glanceable now-playing card: album art, title and artist, progress bar. Read-only, from any MPRIS media player over D-Bus. No transport controls."
weight = 53
template = "docs-page.html"

[extra]
binary = "veiland-now-playing"
one_liner = "current track"
category = "widgets"
example = "now_playing.toml"

[[extra.props]]
key = "layout"
type = "string"
default = "`\"compact\"`"
meaning = "`compact` (a chip: cover + title/artist + progress) or `star` (a large centered cover with a blurred-cover backdrop). Only `star` is special-cased; any other value renders `compact`."

[[extra.props]]
key = "fetch_remote_art"
type = "bool"
default = "`false`"
meaning = "Allow fetching `http(s)://` cover art (e.g. Spotify). Off = a locked screen makes no network request; `file://` covers still decode locally."
+++

`now-playing` reads whatever your MPRIS-capable player (Spotify, mpv, browsers, ...) is
playing, over the session D-Bus. It is **read-only**: there are no play/pause/skip buttons,
because clicks are not yet forwarded to plugins. The accent color is sampled from the album
art.

Only `font_family` and `italic` affect the text; each line's size is derived from the card
geometry, so `font_size` and `font_weight` have no effect. This widget centers itself and
does not read `content_halign` / `content_valign` (it does honor `debug_border`). See the
[widgets](@/docs/plugins/_index.md) overview.
