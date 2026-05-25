<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Veiland plugin API

This document covers the helper crates that veiland plugins link
against. The wire protocol the plugins speak to the host is in
[`protocol.md`](protocol.md); the user-facing config is in
[`config.md`](config.md).

Two crates live on the plugin side:

- **`veiland-plugin`** — required. Wraps the socket dance,
  EGL/GBM setup, dmabuf allocation, and protocol framing.
  Every plugin needs this.
- **`veiland-text`** — optional, added in M10. Provides text
  rendering on top of cosmic-text + a GPU glyph atlas. Add it
  to your `Cargo.toml` only if you actually draw text — its
  transitive deps cost ~5 MB of binary size.

The shape of this document mirrors the crate layout: a brief on
`veiland-plugin` (mostly a pointer to the existing reference
plugins, since the API has been stable since M3), then a fuller
walkthrough of `veiland-text` (new in M10).

## `veiland-plugin`

The canonical reference is the existing plugin source. Read in
order of complexity:

- [`plugins/red-box`](../plugins/red-box/src/main.rs) — minimal
  solid-colour plugin. ~250 lines. Shows the handshake, the
  event-loop shape, and the buffer-lifecycle invariants.
- [`plugins/gradient`](../plugins/gradient/src/main.rs) — adds a
  time-varying uniform and the fast/slow sync-path fork.
- [`plugins/label`](../plugins/label/src/main.rs) — adds
  `veiland-text`, reads its config via `VEILAND_PLUGIN_CONFIG`,
  and defers dmabuf allocation until after the first `Configure`
  so the buffer matches the region.

The key types: `Connection` (socket framing), `GbmEgl` (render
node + EGL context), `DmaBuffer` (the GPU buffer + FBO), and
`SyncFence` (only used on the fast sync path). See the rustdoc
on each.

## `veiland-text`

A plugin author's view of `veiland-text` is two types and a few
small enums. cosmic-text and fontdb sit underneath; you don't
need to know they're there.

### `FontContext`

Owns the font database and the GPU glyph atlas. Construct once
at plugin startup, reuse for the whole session.

```rust
use veiland_text::FontContext;

let mut font_ctx = FontContext::new();
```

`FontContext::new()` is eager: it scans system fonts via fontdb,
which takes ~30–100 ms on a cold cache. Do it once and keep it
around. The atlas and shader program inside `FontContext` are
lazy — they materialize on the first `render` call, when a live
GL context exists.

### `Label`

A label is plain data describing one piece of styled text.
Build a new one each frame from your config + the current
output scale:

```rust
use veiland_text::{HAlign, Label, Shadow, VAlign};

let label = Label {
    text: "Hello veiland".to_string(),
    font_family: "Sans".to_string(),
    font_size: 64.0 * scale as f32,                  // logical px × scale
    color: [0.95, 0.95, 0.95, 1.0],                  // straight-alpha RGBA
    halign: HAlign::Center,
    valign: VAlign::Middle,
    position: (960.0 * scale as f32, 540.0 * scale as f32),
    rotation: 0.0,
    shadow: Some(Shadow {
        offset: (3.0 * scale as f32, 3.0 * scale as f32),
        color: [0.0, 0.0, 0.0, 0.6],
        blur: 0.0,                                   // ignored in M10
    }),
};
```

Field reference:

| Field         | Type            | Notes                                                                  |
| ------------- | --------------- | ---------------------------------------------------------------------- |
| `text`        | `String`        | UTF-8. CJK / RTL / combining marks all work via cosmic-text shaping.   |
| `font_family` | `String`        | `"Sans"` / `"Serif"` / `"Monospace"` or any system family name.        |
| `font_size`   | `f32`           | Physical pixels. Multiply your logical size by `Configure.scale`.      |
| `color`       | `[f32; 4]`      | Straight-alpha RGBA, each component in `[0, 1]`.                       |
| `halign`      | `HAlign`        | `Left` / `Center` / `Right` — which edge of the text sits at `position.x`. |
| `valign`      | `VAlign`        | `Top` / `Middle` / `Bottom` — same for `position.y`.                   |
| `position`    | `(f32, f32)`    | Anchor in surface pixels, top-left origin. Multiply by scale.          |
| `rotation`    | `f32`           | Degrees, counter-clockwise around `position`. 0.0 = axis-aligned.      |
| `shadow`      | `Option<Shadow>`| `None` = single pass. `Some` = shadow first, text on top.              |

`Shadow`:

| Field    | Type         | Notes                                                              |
| -------- | ------------ | ------------------------------------------------------------------ |
| `offset` | `(f32, f32)` | Pixel offset from the text. `(3, 3)` draws down-right.             |
| `color`  | `[f32; 4]`   | Straight-alpha RGBA.                                               |
| `blur`   | `f32`        | Reserved; non-zero values are ignored in M10 with a one-time log.  |

### Rendering

```rust
font_ctx.render(&label, surface_size);
```

`surface_size` is your dmabuf's `(width, height)` in physical
pixels. The label is drawn into the currently-bound framebuffer
— call `dma.bind_for_rendering()?` first, then clear, then call
`render`. Multiple labels per frame are fine; build a `Label`
for each and call `render` once per label.

Alpha blending is enabled inside `render` and left enabled
afterwards. Subsequent draws in the same frame composite on top.

### HiDPI

The host sends `Configure.scale: u32` carrying the output's
`wl_output.scale` (1, 2, or 3). Store it on your plugin state at
every `Configure` and use the current value when building each
`Label`. The convention is: every logical-pixel field
(`font_size`, `position`, `shadow.offset`) gets multiplied by
`scale`; non-pixel fields (`color`, `rotation`) do not.

See [`plugins/label`](../plugins/label/src/main.rs)'s
`build_label` for the reference shape.

### What's not in M10

Things you may notice missing and not need to file as bugs:

- **Shadow blur** (`Shadow.blur`) — the field is on the struct
  for forward compatibility but the value is ignored. Sharp
  shadow only in M10.
- **Stroke / outline** rendering.
- **Per-character colour mixing**, gradients, animated typing.
- **Vertical text** / Mongolian / arbitrary writing modes.
- **Font fallback configuration** — fontdb's automatic fallback
  is what you get. Users who want explicit fallback chains wait
  for M12+.
- **Subpixel positioning controls** — text is snapped to the
  integer pixel grid.
- **Hot-reloading the user's `font_family`** without restarting
  the plugin.
- **Fractional output scale** (`wp_fractional_scale_v1`) — only
  integer `wl_output.scale` in M10.
- **Bitmap fonts** (`pcf`, `bdf`) — TTF/OTF only.

See [`m10-plan.md`](m10-plan.md)'s "Deferred to post-M10"
section for the rationale on each.
