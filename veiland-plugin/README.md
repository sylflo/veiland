<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# veiland-plugin

Helper library for writing veiland plugins. Hides the three things a
plugin author should not have to care about: the socket dance (version
handshake, `SCM_RIGHTS` for the dmabuf fd, recv-loop framing), the
EGL/GBM setup (render-node open, EGL display/context, GBM device,
making the context current surfacelessly), and the protocol dispatch
(message decode, error mapping). After `GbmEgl::new()` and
`Connection::from_env()`, plugin code is essentially "match on
`ServerMessage`, render into a `DmaBuffer`, hand it back to the host."

## What this crate is not

- **Not a host.** Validation of plugin output stops at "the bytes decoded";
  the host re-validates everything that comes off the wire.
- **Not a framework.** No callback runner, no event loop owned by the
  crate. The plugin author writes the `loop { match conn.recv_event()? }`
  themselves so the cadence is theirs to control.
- **Not a GL helper.** Plugin authors write their own shaders, VBOs, and
  draw calls. This crate hands them a framebuffer to render into.
- **Not a buffer-pool implementation.** v1 is single-buffer with
  `glFinish` between render and submit; explicit sync fences and pool
  recycling are M5.

For the wire protocol itself see [`docs/protocol.md`](../docs/protocol.md).
For the architectural rationale (process isolation, threat model, why
dmabuf, why not `.so` plugins) see [`CLAUDE.md`](../CLAUDE.md).

## Canonical event loop

```rust
use veiland_plugin::{Connection, DmaBuffer, GbmEgl, PluginError};
use veiland_protocol::{Buffer, ServerMessage};

fn main() -> Result<(), PluginError> {
    // 1. Connect and handshake.
    let mut conn = Connection::from_env()?;
    conn.handshake()?;
    conn.send_hello("my-plugin", "0.1")?;

    // 2. Render setup. Opens /dev/dri/renderD128, creates EGL ctx.
    let gbm_egl = GbmEgl::new()?;
    let dma = DmaBuffer::new(&gbm_egl, 512, 512)?;

    let buffer_msg = Buffer {
        id: 0,
        width: dma.width(),
        height: dma.height(),
        format: dma.format(),
        modifier: dma.modifier(),
        stride: dma.stride(),
        offset: 0,
    };

    // 3. Event loop.
    loop {
        match conn.recv_event()? {
            ServerMessage::Configure(_) => { /* size/scale/time tick */ }
            ServerMessage::FrameDone => {
                dma.bind_for_rendering()?;
                // ... your GL draw calls ...
                dma.finish();
                conn.send_buffer(&buffer_msg, dma.dmabuf_fd())?;
            }
            ServerMessage::BufferReleased(_) => { /* v1: usually noop */ }
            ServerMessage::Shutdown => return Ok(()),
        }
    }
}
```

## Dependencies the plugin author adds themselves

This crate intentionally does not re-export `gl`, `khronos-egl`, `gbm`,
or `nix`. Plugin authors who need GL draw calls (most of them) should
add `gl` to their own `Cargo.toml` and call it directly — the GL
context is already current after `GbmEgl::new()`.
