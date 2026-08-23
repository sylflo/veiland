<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Veiland Python plugin API

This document covers `veiland_plugin.py` and its companion modules: the
Python SDK for writing veiland plugins. The wire protocol the plugin
speaks to the host is in [`protocol.md`](protocol.md); the user-facing
config is in [`config.md`](config.md); the Rust SDK is in
[`plugin-api.md`](plugin-api.md).

A veiland plugin is a standalone program that renders pixels into a GPU
buffer and hands the buffer to the locker over a Unix socket. Nothing
about that is Rust-specific: the interface is the *protocol*, not the
crates, and the Python SDK is a second, independent implementation of
[`protocol.md`](protocol.md) that speaks the same wire format. Same
nouns, same lifecycle, an idiomatic Python surface.

## When to reach for Python

Veiland's authoring ladder runs config presets, then shader files, then
Python widgets, then Rust. The Python SDK is the **CPU-widget tier**:
small, mostly-static things that draw with a 2D library and read a data
source — a battery indicator, a clock, an avatar, a now-playing card, a
weather panel. It renders on the CPU and hands the host a finished
buffer.

Use **Rust** (see [`plugin-api.md`](plugin-api.md)) for anything
GPU-intensive: full-screen animation at your refresh rate, particle
systems, procedural shaders. GPU rendering from Python is deliberately
not an SDK goal. A rule of thumb: if the plugin redraws every frame
forever, it wants Rust and the GPU; if it draws occasionally and mostly
sits still, Python is the shorter path.

## Vendoring

The core SDK is a **single file with no dependencies** —
`python/veiland_plugin.py`, pure standard library plus `ctypes` (libgbm
is loaded at runtime). The intended install is to copy that one file
next to your plugin. No `pip`, no virtualenv, no build step; it works on
NixOS and in an air-gapped dotfiles repo the same as anywhere.

(A `pip install` path is likely to come later as an additive convenience,
but veiland is pre-1.0 and it is not a goal yet; copy-the-file is the
supported route for now, and being stdlib-only it stays a clean drop-in
regardless.)

The companions (`veiland_text`, `veiland_svg`, `veiland_layout`,
`veiland_dbus`) are **separate opt-in files**, each carrying its own
third-party dependency. Vendor only the ones you use; the core never
imports them.

Requirements: **Python >= 3.9** (the SDK uses `socket.send_fds` /
`recv_fds`, added in 3.9). The reference widgets are installed as
`veiland-avatar`, `veiland-weather`, `veiland-now-playing`, and so on by
the distribution packages; when writing your own, vendor the file.

The plugin file must have the **execute bit** set (`chmod +x`): the host
launches a plugin by `exec`ing its `binary` path, and a script without
`+x` fails at spawn with `Permission denied`. The `#!/usr/bin/env
python3` shebang alone is not enough.

## The four nouns

The core SDK is four types you drive yourself — imperative primitives,
not a framework. You own `main()` and the event loop.

- **`Connection`** — the handshake and the framed Unix-socket transport.
- **`GbmDevice`** — a GBM allocator on a DRM render node.
- **`LinearBuffer`** — one linear ARGB8888 GPU buffer you draw into.
- **`FramePacer`** — the frame-pacing state machine and event loop.

Here is a complete minimal plugin. It reads its config, allocates a
buffer sized to its region, and repaints on a timer:

```python
#!/usr/bin/env python3
import json
import os
from typing import Any

import veiland_plugin as vp


def main() -> None:
    # Plugin config arrives as a JSON object in an env var. The core SDK does
    # NOT parse it for you -- read and decode it yourself (an absent var is an
    # empty table, never an error).
    cfg_table: dict[str, Any] = json.loads(os.environ.get("VEILAND_PLUGIN_CONFIG") or "{}")

    conn = vp.Connection.connect("my-widget", "0.1.0")  # env fd, handshake, Hello
    cfg = conn.wait_for_configure()                     # first Configure (a dataclass)
    dev = vp.GbmDevice()                                # opens the first render node
    chain = vp.BufferChain(dev, cfg.region_w, cfg.region_h)

    pacer = vp.FramePacer.on_demand()                   # or .self_paced() for animation
    for ev in pacer.events(conn, timeout=30.0):
        if ev.kind is vp.Event.RENDER:
            buf = chain.acquire()                       # the buffer NOT in flight
            with buf.map() as (mem, stride):
                draw(mem, stride, cfg, cfg_table)       # your drawing code
            chain.send(conn)                            # ship it and flip
            pacer.submitted()                           # tell the pacer it's in flight
        elif ev.kind is vp.Event.RECONFIGURE and ev.configure is not None:
            cfg = ev.configure
            chain = chain.resize_or_keep(dev, cfg)
            pacer.mark_dirty()
        elif ev.kind is vp.Event.TIMEOUT:
            pacer.mark_dirty()                          # re-read source, request a repaint
        elif ev.kind is vp.Event.SHUTDOWN:
            break

    chain.close()
    dev.close()
    conn.close()


if __name__ == "__main__":
    main()
```

`Connection.connect(name, version)` reads the socket fd from the
`VEILAND_PLUGIN_SOCKET` env var, runs the version + capability
handshake, and sends Hello. Do it before any heavy imports or device
setup: the host applies a spawn deadline. `wait_for_configure()` blocks
until the first `Configure`, a frozen dataclass with `region_x/y/w/h`,
`scale_120` (and a `scale` property returning the float multiplier),
`time_unix_seconds`, `time_tz_offset_seconds`, and `output_name`. The
region dimensions are **already physical pixels** — do not multiply them
by the scale.

## The buffer contract

`LinearBuffer` is one linear ARGB8888 GBM buffer. You allocate it once
at the region size and reuse it; the format is fixed (there is no format
argument). It exposes `width`, `height`, `fd` (the exported dmabuf fd),
`stride`, and `modifier`.

There are two ways to fill it:

- **`buf.map()`** — a context manager yielding `(memoryview,
  map_stride)`. The memoryview aliases GPU-visible memory with no copy;
  draw straight into it. This is the zero-copy path a cairo or Qt
  surface writes through.
- **`buf.upload(pil_image)`** — a convenience that premultiplies an RGBA
  `PIL.Image` and copies it in. Pillow is imported lazily, so the SDK
  loads fine without it.

Two things that bite hand-rolled plugins, both handled for you but worth
knowing:

- **Two strides.** `buf.map()` yields a `map_stride` (the row pitch for
  CPU writes) that is distinct from `buf.stride` (the value that goes in
  the Buffer message). They often agree, but step your `memoryview` rows
  by the `map_stride`, and send `buf.stride`.
- **Premultiplied, little-endian, top-down.** The host composites with
  premultiplied-alpha blending, so write premultiplied pixels. The
  little-endian ARGB8888 layout matches cairo's `FORMAT_ARGB32` and Qt's
  `Format_ARGB32_Premultiplied` byte-for-byte, so those draw with zero
  conversion. Rows are written top-down.

To draw with cairo through `map()`:

```python
import cairo

with buf.map() as (mem, stride):
    surface = cairo.ImageSurface.create_for_data(mem, cairo.FORMAT_ARGB32,
                                                  buf.width, buf.height, stride)
    cr = cairo.Context(surface)
    # ... draw into cr; it writes straight into the GPU-visible buffer ...
    surface.flush()
```

### One buffer or two: `BufferChain`

A plugin that **draws once and idles** (a static card, an SVG icon) can
use a single `LinearBuffer`. A plugin that **redraws** its content must
not: the host samples your dmabuf live and keeps sampling the last
buffer you sent, so redrawing a single buffer in place mutates the
memory currently on screen and produces a probabilistic half-drawn
flicker.

`BufferChain` is the fix: two `LinearBuffer`s, always handing out the
one the host is *not* sampling.

```python
chain = vp.BufferChain(dev, cfg.region_w, cfg.region_h)
# each RENDER:
buf = chain.acquire()          # the free buffer
# ... draw into buf via map()/upload() ...
chain.send(conn)               # ship it, then flip to the other buffer
pacer.submitted()
```

Call `send()` exactly once per `acquire()`. On a resize, `chain.resize_or_keep(dev, cfg)`
returns a resized chain (or the same one if the dimensions match); the
pacer has already drained the in-flight buffer's release before it
hands you the `RECONFIGURE` event, so it is safe to reallocate there.

## The event loop

`FramePacer` owns the pacing state machine. A frame renders only when
all three gates open: the host released the last buffer, the host sent a
frame cue, and the content is dirty. `FramePacer.self_paced()` re-arms
dirty after every send (animation); `FramePacer.on_demand()` renders
only after `mark_dirty()` (widgets that change rarely).

`pacer.events(conn, timeout=None, extra_fds=())` is a generator yielding
typed `FrameEvent`s. Its `kind` is one of:

| `Event` | Meaning | You do |
| --- | --- | --- |
| `RENDER` | all three gates open | draw, `send`, `pacer.submitted()` |
| `RECONFIGURE` | the region resized (drain already done) | `resize_or_keep`, `mark_dirty()` — `.configure` is set |
| `TIMEOUT` | the `timeout` elapsed | re-read your source, `mark_dirty()` |
| `FD_READY` | one of your `extra_fds` is readable | handle it, `mark_dirty()` — `.fd` is set |
| `SHUTDOWN` | host is going away | break the loop |

`SHUTDOWN` is yielded once and then the generator ends; the host closing
the socket (EOF) is the same clean end, never a traceback.

The `timeout` and `extra_fds` are how a widget integrates a timer and an
external event source **without threads**. A weather widget passes
`timeout=900` for a 15-minute refresh; a now-playing widget passes its
D-Bus socket in `extra_fds` so a track change wakes it. The pacer
`select()`s over the host socket, your fds, and the timeout together.

## Companions

Each companion is a separate opt-in file with its own dependency. Vendor
the ones you use.

- **`veiland_text`** — shaped, end-ellipsized single-line text via
  PangoCairo (dep: PyGObject + Pango). `font_from_config(cfg)` reads
  `font_family` / `font_size` / `font_weight` / `italic` from a config
  dict into a `FontSpec` (using the same key names as the Rust label
  plugin); `draw_ellipsized`, `draw_ellipsized_centered`, and
  `draw_ellipsized_right` draw a line onto a cairo context, ellipsizing
  past a pixel width. `font_size` is a fraction of a box you pick, not
  pixels.
- **`veiland_svg`** — render an SVG onto a cairo context via librsvg
  (dep: PyGObject + librsvg). `load_svg(path)` loads and caches a
  handle; `draw_svg(cr, handle, x, y, w, h, tint=None)` renders it to
  fit a box (optionally tinting a monochrome glyph one color);
  `draw_svg_centered` and `draw_pill` are conveniences for status-icon
  widgets. `parse_color(cfg, key, default)` reads an `[r, g, b, a]`
  config color.
- **`veiland_layout`** — the content-anchor convention and a debug
  border (dep: cairo only). `anchor_from_config(cfg)` reads
  `content_halign` / `content_valign`; `anchor_offset(...)` is the pure
  math that places a content block at that anchor within the region;
  `draw_debug_border(cr, w, h, rgba)` strokes the region outline so you
  can see and tune placement. This is a shared convention plugins honor
  themselves — the core never sees these keys.
- **`veiland_dbus`** — a thin, read-only wrapper over jeepney's blocking
  connection (dep: jeepney, pure Python, no typelib). `DBusConnection.connect("SESSION"|"SYSTEM")`,
  then `subscribe(...)` to wake on a signal, `call` / `get_prop` /
  `get_all_props` / `get_managed_objects` to read. Every call is
  best-effort — a D-Bus hiccup returns `None`/`{}`, never an exception
  into the render loop. Pass `conn.fileno()` to the pacer's `extra_fds`
  so a signal wakes the plugin.

## Reference widgets

The `python/examples/` directory is the canonical reference. Read in
order of complexity:

- [`battery.py`](../python/examples/battery.py) — the minimal SDK
  widget: reads `/sys/class/power_supply`, draws with Pillow via
  `upload()`, paces `on_demand()` with a timer. The shortest complete
  plugin.
- [`battery_svg.py`](../python/examples/battery_svg.py) — the same
  reading, drawn as an SVG status pill via `veiland_svg`. The copy-me
  template for the status-icon pattern.
- [`markup.py`](../python/examples/markup.py) — dynamic Pango markup
  text with variable substitution (`veiland_text`).
- [`now_playing.py`](../python/examples/now_playing.py) — the richest:
  MPRIS over D-Bus (`veiland_dbus` in `extra_fds`), album art, a
  progress bar via a timer tick, PangoCairo text with real shaping and
  ellipsization drawn zero-copy through `map()`. Exercises every
  deliberate design choice at once.

For the "no SDK at all, just the wire protocol"
version, [`docs/examples/battery_nosdk.py`](examples/battery_nosdk.py)
speaks `protocol.md` directly — useful for understanding what the SDK
absorbs, not as a template.

## The traps

Things a hand-rolled plugin gets wrong, collected in one place:

- **Execute bit.** The host `exec`s the file; without `chmod +x` it
  fails at spawn. `git` tracks the mode — commit the file `100755`.
- **`memoryview` needs `.cast("B")`** before slice assignment if you get
  a non-byte format; `LinearBuffer.map()` already casts, so this only
  bites raw ctypes-buffer code.
- **The two strides** (map stride vs bo stride) — see the buffer
  contract above.
- **Config is your job.** The core SDK has no config loader; read
  `VEILAND_PLUGIN_CONFIG` (a JSON object, or `"{}"` if absent) and decode
  it yourself, as the skeleton shows.
- **EOF is normal.** The host closing the socket is a clean session end.
  The SDK surfaces it as `SHUTDOWN` / a stopped generator, never a
  traceback — don't wrap the loop in a bare `except`.

## Not yet supported

- **GPU rendering from Python.** The SDK is CPU-only by design; the GPU
  tier is Rust (`plugin-api.md`) and veiland-shader. There is no EGL/GL
  surface and no fence path — every Buffer carries exactly one fd (the
  dmabuf), never a sync fence.
- **Interactive widgets.** Plugins receive no pointer or keyboard input;
  there is no click message, so a now-playing card is read-only. This is
  a protocol property, not a Python limitation, and a future design
  round for all SDKs at once.
- **An shm buffer path.** Buffers are GBM dmabufs; there is no
  shared-memory alternative.
