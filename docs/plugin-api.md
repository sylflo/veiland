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
- **`veiland-text`** — optional. Provides text
  rendering on top of cosmic-text + a GPU glyph atlas. Add it
  to your `Cargo.toml` only if you actually draw text — its
  transitive deps cost ~5 MB of binary size.

The shape of this document mirrors the crate layout: a brief on
`veiland-plugin` (mostly a pointer to the existing reference
plugins), then a fuller walkthrough of `veiland-text`.

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
output scale (`scale`) and surface size (`surface_w`, `surface_h`,
i.e. `dma.width()` / `dma.height()`):

```rust
use veiland_text::{HAlign, Label, Shadow, VAlign};

// font_size_frac and shadow_offset_frac come from the plugin's config
// as fractions of surface height (e.g. 0.067 = ~72px on 1080p).
let font_size_px = font_size_frac * surface_h as f32;
let label = Label {
    text: "Hello veiland".to_string(),
    font_family: "Sans".to_string(),
    font_size: font_size_px,
    color: [0.95, 0.95, 0.95, 1.0],                  // straight-alpha RGBA
    halign: HAlign::Center,
    valign: VAlign::Middle,
    // `Label.position` is in surface pixels. Express position as a fraction
    // in your config and multiply by the surface size here so it tracks any
    // resolution — e.g. centre = (0.5 * surface_w, 0.5 * surface_h).
    position: (0.5 * surface_w as f32, 0.5 * surface_h as f32),
    rotation: 0.0,
    shadow: Some(Shadow {
        offset: (shadow_offset_frac * surface_h as f32,
                 shadow_offset_frac * surface_h as f32),
        color: [0.0, 0.0, 0.0, 0.6],
        blur: 0.0,
    }),
};
```

Field reference:

| Field         | Type            | Notes                                                                  |
| ------------- | --------------- | ---------------------------------------------------------------------- |
| `text`        | `String`        | UTF-8. CJK / RTL / combining marks all work via cosmic-text shaping.   |
| `font_family` | `String`        | `"Sans"` / `"Serif"` / `"Monospace"` or any system family name.        |
| `font_size`   | `f32`           | Physical pixels. Express your config value as a fraction of surface height and multiply by `surface_h` here — e.g. `0.067 * surface_h` ≈ 72px on 1080p, 145px on 4K. |
| `color`       | `[f32; 4]`      | Straight-alpha RGBA, each component in `[0, 1]`.                       |
| `halign`      | `HAlign`        | `Left` / `Center` / `Right` — which edge of the text sits at `position.x`. |
| `valign`      | `VAlign`        | `Top` / `Middle` / `Bottom` — same for `position.y`.                   |
| `position`    | `(f32, f32)`    | Anchor in surface pixels, top-left origin. For resolution-independence, derive it from a config *fraction* × surface size (see note below) rather than absolute pixels. Not scale-multiplied. |
| `rotation`    | `f32`           | Degrees, counter-clockwise around `position`. 0.0 = axis-aligned.      |
| `shadow`      | `Option<Shadow>`| `None` = single pass. `Some` = shadow first, text on top.              |

`Shadow`:

| Field    | Type         | Notes                                                              |
| -------- | ------------ | ------------------------------------------------------------------ |
| `offset` | `(f32, f32)` | Pixel offset from the text. Express as `fraction * surface_h` — e.g. `0.003 * surface_h` ≈ 3px on 1080p. |
| `color`  | `[f32; 4]`   | Straight-alpha RGBA.                                               |
| `blur`   | `f32`        | Reserved; non-zero values are currently ignored with a one-time log.  |

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

The host sends `Configure.scale_120: u32` carrying the output's scale
as 120ths (120 = 1×, 180 = 1.5×, 240 = 2×), matching
`wp_fractional_scale_v1`'s encoding. Convert to a float multiplier with
`scale_120 as f32 / 120.0`. Store it on your plugin state at every
`Configure` and use the current value when building each `Label`. The
convention is: every logical-pixel *size* field (`font_size`,
`letter_spacing`, `shadow.offset`) gets multiplied by that factor;
non-pixel fields (`color`, `rotation`) do not.

`position` is the exception: don't scale it. A label's place on screen
should be a *fraction of the surface* (`[0.5, 0.5]` = centre), multiplied
by the surface size when building the `Label`. Fractions are
resolution-independent — `0.5` is the middle of a 1080p and a 4K buffer
alike — so a label stays put when the host resizes the surface (the
1080p-spawn-fallback → native-4K resend, or a mid-lock mode change). The
reference plugins (`veiland-clock`, `veiland-label`) take `position` as a
`[0.0..=1.0]` fraction in their TOML config for this reason. Absolute
pixels would silently mean "centre" only at one specific resolution.

When the surface is resized, reallocate your dmabuf to the new size with
`DmaBuffer::resize_to(&gbm_egl, w, h)` in your `Frame::Reconfigure` arm
(returns `true` if it reallocated → rebuild your cached `Buffer` message);
otherwise the host stretches your old buffer and text goes soft.

See [`plugins/label`](../plugins/label/src/main.rs)'s
`build_label` for the reference shape.

### Not yet supported

Things you may notice missing in `veiland-text` and not need to file
as bugs:

- **Shadow blur** (`Shadow.blur`) — the field is on the struct
  for forward compatibility but the value is currently ignored;
  shadows are sharp.
- **Stroke / outline** rendering.
- **Per-character colour mixing**, gradients, animated typing.
- **Vertical text** / Mongolian / arbitrary writing modes.
- **Font fallback configuration** — fontdb's automatic fallback
  is what you get. Explicit fallback chains are future work.
- **Subpixel positioning controls** — text is snapped to the
  integer pixel grid.
- **Hot-reloading the user's `font_family`** without restarting
  the plugin.
- **Bitmap fonts** (`pcf`, `bdf`) — TTF/OTF only.

## Loading image assets

Plugins that paint pixels from a file (a wallpaper, an icon, a
splash) follow one pattern: decode once at startup with the
`image` crate, upload the RGBA bytes to a `GL_TEXTURE_2D` via
`glTexImage2D`, then bind that texture each frame and draw a
textured quad. The decoded `Vec<u8>` is dropped once the upload
finishes — pixels live on the GPU after that.

A few non-obvious points:

- **Decode format**: convert to `RgbaImage` via `.to_rgba8()`
  even if the source is RGB. `RGBA8` matches the GL internal
  format you'll upload with and avoids per-pixel conversion at
  sample time.
- **Default `image` features are broad**. Veiland-wallpaper uses
  `default-features = false, features = ["png", "jpeg"]` to
  minimise CVE surface on a security-critical process. Enable
  more formats only when a user asks.
- **Don't decode on the IPC main thread for large images**.
  Today `veiland-wallpaper` does, which blocks the lock surface
  for ~5s on a 4K JPEG; the fix is a worker thread (deferred). Small icons
  (~hundreds of KB) decode in single-digit ms and are fine
  inline.
- **EXIF orientation is not honoured** by `image` by default.
  Phones / cameras embedding an "image is rotated" tag will
  render sideways (deferred).

Reference: [`plugins/wallpaper`](../plugins/wallpaper/src/main.rs).
Short enough to read top-to-bottom.

## Procedural shader plugins

Some effects don't need any input pixels — they're pure functions
of the surface coordinate. A radial-gradient vignette, a starfield,
a glow, a noise field. For these, the plugin's dmabuf is the
framebuffer; the fragment shader produces every output pixel from
uniforms and `gl_FragCoord` (or a `v_uv` varying).

The pattern is one full-buffer quad in the vertex shader plus a
fragment shader that does the per-pixel maths. Uniforms come from
the plugin's `[plugin.config]` table, plumbed via
`VEILAND_PLUGIN_CONFIG` and parsed with serde. Re-render every
`FrameDone` for protocol correctness even if the output is static;
the host's per-frame cost is dominated by other work.

A few non-obvious points:

- **`precision highp float` everywhere**. Mesa's `mediump` (16-bit
  on some drivers) bands on smoothstep sums and other compound
  operations at low gradient values. Veiland-vignette would visibly
  step without highp; the cost is negligible on any modern GPU.
- **Aspect ratio**. A `length(v_uv)` "circle" becomes an ellipse on
  a 1920×1080 buffer because UV space is square but the buffer
  isn't. Pass the buffer's aspect ratio (`w / h`) as a uniform and
  apply it to the X component before computing distances.
- **Y orientation**. The dmabuf's FBO and the host's composite path
  share an orientation such that `v_uv = a_pos * 0.5 + 0.5` lands
  top-left at UV (0, 0) without any extra Y flip — `gl_FragCoord.y`
  grows downward, opposite to the "Y up in clip space" mental model
  that classic GL tutorials teach. (See the wallpaper plugin: the
  first cut had a flip and the image came out upside down; removing
  it was the fix.)
- **Premultiplied-alpha output**. The host composites your buffer using
  premultiplied-alpha blending (`docs/protocol.md` §12.1). Emit
  `vec4(rgb * a, a)` — RGB already scaled by alpha. Transparent
  pixels should be `vec4(0.0)`.

Reference: [`plugins/vignette`](../plugins/vignette/src/main.rs).
~400 lines including the shader source as bytes-literals.
