<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# veiland-plugin

Helper library for writing veiland plugins. Hides the four things a
plugin author should not have to care about: the socket dance (version
handshake, `SCM_RIGHTS` for the dmabuf fd, recv-loop framing), the
EGL/GBM setup (render-node open, EGL display/context, GBM device,
making the context current surfacelessly), the FrameDone/BufferReleased
pacing state machine, and the sync-model choice (fence fd vs
`glFinish`). After `GbmEgl::new()` and `Connection::connect()`, plugin
code is essentially "match on `Frame`, render into a `DmaBuffer`, hand
it back with `submit_frame`."

## What this crate is not

- **Not a host.** Validation of plugin output stops at "the bytes decoded";
  the host re-validates everything that comes off the wire.
- **Not a framework.** No callback runner, no event loop owned by the
  crate. The plugin author writes `main()` and the loop themselves;
  `FramePacer` only translates host events into render / reconfigure /
  shutdown outcomes so the loop body stays a few obvious lines.
- **Not a GL helper.** Plugin authors write their own shaders, VBOs, and
  draw calls. This crate hands them a framebuffer to render into.
- **Not a buffer-pool implementation.** v1 is single-buffer.
  `submit_frame` picks the sync model per connection: `glFlush` plus a
  sync-fence fd when both host and plugin support it, `glFinish`
  otherwise. A buffer pool with per-id recycling is future work.

For the plugin-facing API in depth see
[`docs/plugin-api.md`](../docs/plugin-api.md); for the wire protocol see
[`docs/protocol.md`](../docs/protocol.md). For the architectural
rationale (process isolation, threat model, why dmabuf, why not `.so`
plugins) see [`CLAUDE.md`](../CLAUDE.md).

## Canonical event loop

The shape every reference plugin under `plugins/` uses:

```rust
use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError};

const PLUGIN_NAME: &str = "my-plugin";

fn main() -> Result<(), PluginError> {
    // 1. Render setup. Opens the render node, creates the EGL context
    //    (made current surfacelessly) and the GBM device.
    let gbm_egl = GbmEgl::new()?;

    // 2. Connect preamble: socket fd from the environment, version +
    //    capability handshake, Hello.
    let mut conn = Connection::connect(PLUGIN_NAME, env!("CARGO_PKG_VERSION"))?;

    // 3. The first Configure carries the assigned region size — needed
    //    before the dmabuf can be allocated. `None` means the host shut
    //    down before configuring us; exit cleanly.
    let cfg = match conn.wait_for_configure()? {
        Some(c) => c,
        None => return Ok(()),
    };
    let mut dma = DmaBuffer::new(&gbm_egl, cfg.region_w, cfg.region_h)?;

    // 4. Event loop. `on_demand()` redraws only when the host asks
    //    (static content); `self_paced()` redraws continuously at the
    //    compositor's repaint rate (animation).
    let mut pacer = FramePacer::on_demand();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                dma.bind_for_rendering()?;
                // ... your GL draw calls ...
                conn.submit_frame(&dma, &gbm_egl)?;
                pacer.submitted();
            }
            Frame::Reconfigure(c) => {
                dma.resize_or_keep(&gbm_egl, c.region_w, c.region_h, PLUGIN_NAME);
            }
            Frame::Shutdown => return Ok(()),
        }
    }
}
```

`submit_frame` builds the wire `Buffer` message from the `DmaBuffer`
and picks the sync model for you. Call `pacer.submitted()` after every
submit so the pacer knows a buffer is in flight.

## Config

`veiland_plugin::load_config::<C>(PLUGIN_NAME)` deserializes the
plugin's config table (JSON-serialised TOML, passed by the host via the
`VEILAND_PLUGIN_CONFIG` environment variable) into any
`Deserialize + Default` struct. Missing or malformed config logs a line
and falls back to `C::default()` — a bad config value must never take
the plugin down.

## Lower-level layer

`Connection::connect` is exactly `from_env()` + `handshake()` +
`send_hello()`, and `FramePacer` is optional — `recv_event()` and
`send_buffer()` are public for plugins that need a custom loop. Reach
for them only when the canonical shape doesn't fit.

## Dependencies the plugin author adds themselves

This crate intentionally does not re-export `gl`, `khronos-egl`, `gbm`,
or `nix`. Plugin authors who need GL draw calls (most of them) should
add `gl` to their own `Cargo.toml` and call it directly — the GL
context is already current after `GbmEgl::new()`.
