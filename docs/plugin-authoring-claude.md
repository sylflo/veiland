# Writing a veiland plugin — guide for Claude Code

Drop this file into your plugin project as `CLAUDE.md` (or point your
assistant at it). It is the context an AI assistant needs to write a
**correct** veiland plugin on the first try. Every signature here is
verified against the `veiland-plugin` SDK; if something in your SDK
version differs, the SDK and `docs/protocol.md` win.

## What a veiland plugin is

A veiland plugin is a **standalone program** that renders pixels into a
GPU buffer and hands the buffer to the veiland locker over a Unix
socket. It runs as its **own process** — the locker spawns it, talks to
it over a socketpair, and composites its output onto the lock screen.

Key consequences to internalize before writing code:

- **You render into a DMA-BUF, not onto the screen.** You allocate a GPU
  buffer, draw into it with your own OpenGL context, and send the
  buffer's file descriptor to the host. The host samples it as a
  texture. No pixels are ever copied through CPU memory.
- **You are untrusted, by design.** You never receive keyboard input,
  never see the password, never make the unlock decision. No protocol
  message carries a keystroke in either direction. Don't look for an
  input API for keys — it does not exist.
- **You get your own EGL context and GBM device.** Your GL is entirely
  separate from the host's. You can render anything — 2D, 3D, shaders,
  animation — the host just receives a finished frame.

The reference SDK is `veiland-plugin` (Rust). Plugins can be written in
any language that can speak the wire protocol (`docs/protocol.md`) and
allocate a dmabuf, but this guide covers the Rust SDK.

## The canonical plugin shape — copy this exactly

Every plugin follows this structure. Deviating from it is almost always
a mistake. This is the verified shape (see `plugins/gradient` for a
complete, minimal, working example):

```rust
use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, SyncFence};
use veiland_protocol::Buffer;

fn run() -> Result<(), PluginError> {
    // 1. GPU context + buffer. GbmEgl::new() opens the render node and
    //    sets up EGL. DmaBuffer::new allocates an ARGB8888 GPU buffer
    //    wrapped as an FBO you render into.
    let gbm_egl = GbmEgl::new()?;
    let dma = DmaBuffer::new(&gbm_egl, WIDTH, HEIGHT)?;

    // 2. Build your GL program(s). dma.bind_for_rendering() makes the
    //    buffer's FBO the active render target first.
    dma.bind_for_rendering()?;
    let program = /* compile your shaders */;

    // 3. Connect to the host: reads the socket fd from the environment,
    //    negotiates the protocol version + capabilities, sends Hello.
    let mut conn = Connection::connect("my-plugin", env!("CARGO_PKG_VERSION"))?;

    // 3b. Decide the sync path ONCE, and keep it fixed for the whole
    //     connection. Fast path needs BOTH sides to support fence fds.
    let fast_path = conn.host_supports_fence_fd() && gbm_egl.supports_fence_fd();

    // 4. Build the Buffer wire message. In v1 it's constant (one buffer
    //    reused every frame), so build it once. id = 0 in v1.
    let buf_msg = Buffer {
        id: 0,
        width: dma.width(),
        height: dma.height(),
        format: dma.format(),
        modifier: dma.modifier(),
        stride: dma.stride(),
        offset: 0,
    };

    // 5. The event loop. FramePacer owns the FrameDone/BufferReleased
    //    pacing; you drive a three-arm match. Pick self_paced() if you
    //    animate, on_demand() if you're mostly static.
    let mut pacer = FramePacer::self_paced();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                // draw into `dma`, then submit the buffer:
                dma.bind_for_rendering()?;
                // ... gl::Clear, gl::DrawArrays, etc. ...
                if fast_path {
                    // gl::Flush(); then attach a fence:
                    let fence = SyncFence::create(&gbm_egl)?;
                    conn.send_buffer(&buf_msg, dma.dmabuf_fd(), Some(fence.as_fd()))?;
                } else {
                    dma.finish(); // glFinish — GPU fully drained before send
                    conn.send_buffer(&buf_msg, dma.dmabuf_fd(), None)?;
                }
                pacer.submitted(); // REQUIRED after every send_buffer
            }
            Frame::Reconfigure(c) => {
                // Host re-sent Configure (scale/region/time changed).
                // Update your state; no render is forced this turn.
            }
            Frame::Shutdown => return Ok(()),
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("veiland-my-plugin: {e}");
        std::process::exit(1);
    }
}
```

## The SDK surface — the only types you need

`veiland-plugin` exposes primitives you drive, **not** a framework you
hook into. There is no `run_plugin(trait)`. You own `main()` and the
loop. The public exports:

| Type | What it is |
|---|---|
| `Connection` | The socket to the host. Handshake + send/recv. |
| `DmaBuffer` | A GPU buffer wrapped as an FBO you render into. |
| `GbmEgl` | Your EGL context + GBM device. Created first. |
| `FramePacer` | The FrameDone/BufferReleased pacing state machine. |
| `Frame` | One loop turn: `Render`, `Reconfigure(Configure)`, `Shutdown`. |
| `SyncFence` | A GPU fence for the fast sync path. |
| `PluginError` | The error type everything returns. |

Verified method signatures (do not guess these):

```rust
// Connection
Connection::connect(name: &str, version: &str) -> Result<Connection, PluginError>
conn.wait_for_configure() -> Result<Option<Configure>, PluginError> // None = shutdown before first Configure
conn.host_supports_fence_fd() -> bool
conn.send_buffer(&Buffer, dmabuf_fd: BorrowedFd, fence_fd: Option<BorrowedFd>) -> Result<(), PluginError>
conn.send_buffer_destroy(id: u32) -> Result<(), PluginError>

// GbmEgl
GbmEgl::new() -> Result<GbmEgl, PluginError>
gbm_egl.supports_fence_fd() -> bool
gbm_egl.make_current() -> Result<(), PluginError>

// DmaBuffer
DmaBuffer::new(&GbmEgl, width: u32, height: u32) -> Result<DmaBuffer, PluginError>
dma.bind_for_rendering() -> Result<(), PluginError>
dma.resize_to(&GbmEgl, width: u32, height: u32) -> Result<bool, PluginError> // true if it reallocated
dma.finish() // glFinish (slow-path sync)
dma.dmabuf_fd() -> BorrowedFd
dma.width()/height()/stride() -> u32,  dma.modifier() -> Modifier,  dma.format() -> Fourcc

// FramePacer
FramePacer::self_paced() -> FramePacer   // animated plugins
FramePacer::on_demand()  -> FramePacer   // mostly-static plugins
pacer.next(&mut Connection) -> Result<Frame, PluginError>
pacer.submitted()  // call after every send_buffer

// SyncFence
SyncFence::create(&GbmEgl) -> Result<SyncFence, PluginError>
fence.as_fd() -> BorrowedFd
```

## Non-negotiable rules — get these wrong and it breaks subtly

1. **Call `pacer.submitted()` immediately after every `send_buffer`.**
   The pacer tracks whether a buffer is in flight; skipping this makes
   it render again before the host has released the buffer, corrupting
   the single-buffer handshake.

2. **Pick the sync path once, keep it fixed.** `fast_path =
   conn.host_supports_fence_fd() && gbm_egl.supports_fence_fd()`. If
   fast: `gl::Flush()`, then `send_buffer(..., Some(fence.as_fd()))`. If
   slow: `dma.finish()`, then `send_buffer(..., None)`. Never mix per
   frame. Sending a fence the host didn't negotiate — or omitting one it
   expects — is a protocol violation.

3. **`self_paced()` vs `on_demand()` is your plugin's personality:**
   - `self_paced()` — you animate. Renders again on every
     BufferReleased (after the first FrameDone), so your animation runs
     at the compositor's repaint rate. Use for particles, gradients,
     anything moving.
   - `on_demand()` — you're mostly static. Renders only when the host
     asks (FrameDone). Use for a wallpaper, clock, label. No wasted
     redraws.

4. **Premultiply alpha for any transparency.** The host composites under
   `glBlendFunc(ONE, ONE_MINUS_SRC_ALPHA)`. A transparent plugin MUST
   emit premultiplied alpha (`gl_FragColor = vec4(rgb * a, a)`), not
   straight alpha, or edges will halo. Opaque plugins (`a = 1.0`) are
   unaffected.

5. **Shader source comments must be ASCII only.** GLSL lives in `b"..."`
   byte-string literals. No em dashes, smart quotes, or any non-ASCII
   character inside shader source — it breaks compilation on some
   drivers.

6. **Never `.unwrap()`/`.expect()`/panic on a host message.** The host
   is more trusted than you are to it, but a disconnect or an unexpected
   message must be a clean exit, not a crash. `PluginError::Disconnected`
   from `recv`/`pacer.next` means the host went away — return `Ok(())`,
   don't panic. Propagate errors with `?` up to `run()` and exit
   non-zero from `main`.

## The Configure message — what the host tells you

`Frame::Reconfigure(Configure)` (and the first `wait_for_configure()`)
carries:

```rust
struct Configure {
    region_x: i32, region_y: i32,     // your region's top-left, physical px
    region_w: u32, region_h: u32,     // your region's size, PHYSICAL px
    scale_120: u32,                   // scale as 120ths: 120=1x, 180=1.5x, 240=2x
    time_unix_seconds: i64,           // wall-clock time (host re-sends every ~30s)
    time_tz_offset_seconds: i32,      // local tz offset, for clocks
    output_name: String,              // e.g. "DP-1" — which monitor you're on
}
```

Notes that trip people up:

- **`region_w`/`region_h` are already in physical pixels.** Do NOT
  multiply by scale. Use `scale_120 as f32 / 120.0` only if you need the
  float multiplier for DPI-aware sizing (e.g. font size).
- **Time comes from the host, not `clock_gettime`.** A clock plugin
  reads `time_unix_seconds` and re-renders when Configure re-arrives
  (~every 30s). Don't reach for the system clock yourself — stay a pure
  function of host events.
- **`output_name` is how you do per-monitor behaviour** (different
  wallpaper per screen, different timezone per screen). Ignore it if you
  don't care.
- **Resizing:** on a cold lock the host may spawn you at a 1080p
  fallback, then re-send Configure with the true size. Call
  `dma.resize_to(&gbm_egl, c.region_w, c.region_h)` in the
  `Reconfigure` arm to render at native resolution. It returns `true` if
  it reallocated (rebuild your cached `Buffer` message then, since
  fd/stride/modifier move with the new buffer).

## Config from the user

The user's config for your plugin arrives as JSON in the
`VEILAND_PLUGIN_CONFIG` environment variable (a JSON-serialized TOML
table). Read it once at startup:

```rust
let cfg: MyConfig = std::env::var("VEILAND_PLUGIN_CONFIG")
    .ok()
    .and_then(|s| serde_json::from_str(&s).ok())
    .unwrap_or_default(); // missing/invalid config -> defaults, never crash
```

See `plugins/clock` or `plugins/particles` for the full pattern
(parse, warn on failure, fall back to defaults).

## Coordinate math — the part that looks scarier than it is

The locker composites flat textured rectangles. There are **no
matrices** in the reference plugins. Everything reduces to four patterns:

1. **The unit quad** is always the same 12 numbers — a square from
   `(-1,-1)` to `(1,1)`, six vertices (two triangles). Copy it; never
   recompute it.
2. **A vertex shader places that square.** `unit01 = a_pos * 0.5 + 0.5`
   normalizes a corner to `[0,1]`; that's the whole trick. For a
   full-region plugin you often just write
   `gl_Position = vec4(a_pos, 0.0, 1.0)` — fill the buffer edge to edge.
3. **Pixel to clip:** `(px / size) * 2.0 - 1.0`. Always this shape.
4. **The Y-flip:** clip-space y=+1 is the top, but images/pixels put
   y=0 at the top. When a Y coordinate crosses between those worlds,
   negate it (or `1.0 - y`). X never flips, only Y.

If your plugin fills its whole region (most do), you often need only
pattern 2's simplest form and none of the rest — the fragment shader
does all the interesting work per pixel.

## Where to look

- **`plugins/gradient`** — the minimal complete plugin. Start here.
- **`plugins/particles`** — a self-paced animated plugin with per-item
  state and config.
- **`plugins/wallpaper`** — an on-demand plugin that loads an image.
- **`docs/protocol.md`** — the wire protocol (the authority).
- **`docs/plugin-api.md`** — the full SDK API reference.
- **`docs/config.md`** — how users configure plugins.

## Quick checklist before you call it done

- [ ] `pacer.submitted()` after every `send_buffer`
- [ ] Sync path chosen once and consistent (fence+Flush OR finish+no-fence)
- [ ] `self_paced()`/`on_demand()` matches whether you animate
- [ ] Transparency premultiplied; shader comments ASCII-only
- [ ] No panic/unwrap on host messages; `Disconnected` exits cleanly
- [ ] `region_w`/`region_h` used as physical px (not multiplied by scale)
- [ ] Config parse failures fall back to defaults, never crash
