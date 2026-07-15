+++
title = "Writing plugins"
description = "A plugin is a program that talks to the locker over a socket. Any language that can pass a file descriptor works."
weight = 3
template = "docs-page.html"

[extra]
group = "guide"
+++

A veiland plugin is a standalone program. It connects to the locker over a
Unix socket, renders into a GPU buffer it allocates itself, and hands the
locker a file descriptor. That is the whole contract: no `.so` loading, no
locker rebuild, no upstream approval. Drop your binary next to your config,
point a `[[plugin]]` block at it, done.

By protocol construction a plugin never receives keystrokes and cannot
trigger an unlock; no message carries either in any direction. Your plugin
renders pixels, the core does everything security-critical.

## The shape of a plugin

The reference SDK is the `veiland-plugin` Rust crate. A complete plugin:

```rust
use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer, GbmEgl};

fn main() -> anyhow::Result<()> {
    let mut conn = Connection::connect("my-plugin", env!("CARGO_PKG_VERSION"))?;
    let cfg = match conn.wait_for_configure()? {
        Some(c) => c,
        None => return Ok(()),
    };
    // Own EGL context + GBM device, then a DMA-BUF at the region size.
    let gbm_egl = GbmEgl::new()?;
    let mut dma = DmaBuffer::new(&gbm_egl, cfg.region_w, cfg.region_h)?;
    // set up GL (shaders, VBO) against dma.bind_for_rendering() ...
    let mut pacer = FramePacer::self_paced();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                // render into the DMA-BUF, then hand it to the host.
                conn.submit_frame(&dma, &gbm_egl)?;
                pacer.submitted();
            }
            Frame::Reconfigure(c) => {
                dma.resize_or_keep(&gbm_egl, c.region_w, c.region_h, "my-plugin");
            }
            Frame::Shutdown => return Ok(()),
        }
    }
}
```

The SDK exposes imperative primitives you drive: `Connection::connect` reads
the socket fd from the environment and handshakes, `wait_for_configure`
blocks until the host says what to draw, `FramePacer` encapsulates the
frame-pacing state machine, and `DmaBuffer` handles GBM/EGL buffer
allocation. You own `main()`, the render code, and the event loop.

## Configuration

Whatever the user writes in your plugin's `[plugin.config]` table arrives in
the `VEILAND_PLUGIN_CONFIG` environment variable as JSON. The schema is
yours: veiland does not interpret the contents, so your plugin defines
whatever properties it wants. Parse it, fall back to defaults for anything
missing, and never crash on bad input; log to stderr and run with defaults
instead.

## Not tied to Rust

The wire format is documented in the [protocol reference](@/docs/protocol.md):
an `AF_UNIX` `SOCK_SEQPACKET` socket, tagged messages, and a dmabuf fd passed
via `SCM_RIGHTS`. Any language that can speak a Unix socket and pass a file
descriptor can implement it.

## Reference material

- The [Plugin API](@/docs/plugin-api.md): the full SDK reference, including
  image assets and procedural shader plugins.
- The [Protocol](@/docs/protocol.md): the wire format specification.
- [AI-assisted authoring](@/docs/ai-authoring.md): purpose-built context for
  writing a plugin with a coding assistant. Drop it into your plugin project
  as `CLAUDE.md`, or point your assistant at it: it carries the verified SDK
  signatures, the canonical plugin shape, and the non-negotiable rules an
  assistant needs to get a plugin right on the first try.
- The reference plugins in
  [`plugins/`](https://github.com/sylflo/veiland/tree/master/plugins) are all
  small; `vignette` is a good minimal read, `sakura` shows textures, and
  `raymarcher` shows a heavy procedural shader with thermal knobs.
