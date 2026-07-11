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

- [`plugins/gradient`](../plugins/gradient/src/main.rs) — minimal
  animated fullscreen-shader plugin. ~350 lines. Shows the
  handshake, the `FramePacer` event-loop shape, config loading,
  and fence-aware frame submission via `Connection::submit_frame`.
- [`plugins/vignette`](../plugins/vignette/src/main.rs) — the same
  shape for a static effect: `FramePacer::on_demand()` instead of
  self-paced, re-rendering only when the host asks.
- [`plugins/label`](../plugins/label/src/main.rs) — adds
  `veiland-text` for glyph rendering on top of the same
  lifecycle.

The key types: `Connection` (socket framing + `submit_frame`), `GbmEgl`
(render node + EGL context), `DmaBuffer` (the GPU buffer + FBO),
`FramePacer` (the FrameDone/BufferReleased pacing your event loop drives),
and `SyncFence` (the fast sync path — `submit_frame` chooses the sync model
and handles the fence for you, so you only touch `SyncFence` directly on the
low-level `send_buffer` path). See the rustdoc on each.

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
    letter_spacing: 0.0, // extra tracking in px; 0.0 = font's natural spacing
    font_weight: 400,    // 400 Normal, 700 Bold
    italic: false,
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
`scale_120 as f32 / 120.0`. The surface itself always arrives in
*physical* pixels (`region_w`/`region_h` are device pixels), so how you
use `scale_120` depends on how your plugin expresses sizes. Two
conventions ship in the reference plugins, and both are correct:

**Logical-pixel × scale** — for absolute-sized primitives. A particle
plugin means a literal "3-pixel dot," which must grow with the display
scale or it looks hairline-thin on a 2× monitor. Store `scale_120` on
plugin state at every `Configure`, latch the new value on re-`Configure`,
and multiply every logical-pixel *size* value by `scale_120 / 120.0` at
render time; non-pixel fields (`color`) do not scale. `veiland-particles`,
`veiland-sakura`, `veiland-snow`, `veiland-rain`, `veiland-embers`, and
`veiland-fireflies` all follow this — their `radius_px` / `size_px` config
keys are "logical px at scale = 1."

**Fraction of surface** — for elements sized relative to the display. A
text plugin usually wants "the clock is ~1/15th of the screen tall,"
expressed as a fraction and multiplied by the *physical* surface height
when building the `Label`. This is both resolution- and scale-independent
in one step: because the surface is already physical-sized, a 2× monitor
delivers a 2×-taller buffer and the glyph grows automatically — so these
plugins never read `scale_120` at all. `veiland-label` and `veiland-clock`
size `font_size`, `letter_spacing`, and `shadow.offset` this way (config
values are fractions of surface height, not logical pixels).

`position` follows the fraction model in every plugin: a place on screen
should be a *fraction of the surface* (`[0.5, 0.5]` = centre), multiplied
by the surface size when building the frame. Fractions are
resolution-independent — `0.5` is the middle of a 1080p and a 4K buffer
alike — so a label stays put when the host resizes the surface (the
1080p-spawn-fallback → native-4K resend, or a mid-lock mode change). The
reference plugins (`veiland-clock`, `veiland-label`) take `position` as a
`[0.0..=1.0]` fraction in their TOML config for this reason. Absolute
pixels would silently mean "centre" only at one specific resolution.

When the surface is resized, reallocate your dmabuf to the new size with
`DmaBuffer::resize_or_keep(&gbm_egl, w, h, plugin_name)` in your
`Frame::Reconfigure` arm; otherwise the host stretches your old buffer and
text goes soft. `resize_or_keep` reallocates only when the size changed,
logs it, and keeps the current buffer on a transient failure — it never
returns an error or panics, and you don't rebuild anything by hand
(`submit_frame` reads the buffer's fields fresh each frame). Reach for the
lower-level `DmaBuffer::resize_to(&gbm_egl, w, h) -> Result<bool, _>` only
if you want to react to the reallocation yourself.

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
- **Keep the decoder surface small**. On a security-critical
  process, decode only the formats you need. `veiland-wallpaper`
  decodes JPEG via `turbojpeg` and PNG via the `image` crate
  (`default-features = false, features = ["png"]`), sniffing the
  format by magic bytes rather than trusting the file extension.
  Add more formats only when a user asks.
- **Decode large images off the render path**. A 4K image can take
  a noticeable fraction of a second to decode; doing it inline would
  leave the region on its fallback colour that whole time.
  `veiland-wallpaper` decodes on a worker thread and renders black
  until the pixels arrive (a non-blocking `try_recv` in the render
  loop), so the handshake and first frames never wait on it. Small
  icons (~hundreds of KB) decode in single-digit ms and are fine
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
