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
use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError};

const PLUGIN_NAME: &str = "my-plugin";

fn run() -> Result<(), PluginError> {
    // 1. GPU context + buffer. GbmEgl::new() opens the render node and
    //    sets up EGL. DmaBuffer::new allocates an ARGB8888 GPU buffer
    //    wrapped as an FBO you render into. Start at a fallback size; the
    //    first Configure carries the real region size (see step 4).
    let gbm_egl = GbmEgl::new()?;
    let mut dma = DmaBuffer::new(&gbm_egl, WIDTH, HEIGHT)?;

    // 2. Build your GL program(s). dma.bind_for_rendering() makes the
    //    buffer's FBO the active render target first.
    dma.bind_for_rendering()?;
    let program = /* compile your shaders */;

    // 3. Connect to the host: reads the socket fd from the environment,
    //    negotiates the protocol version + capabilities, sends Hello.
    let mut conn = Connection::connect(PLUGIN_NAME, env!("CARGO_PKG_VERSION"))?;

    // 4. The event loop. FramePacer owns the FrameDone/BufferReleased
    //    pacing; you drive a three-arm match. Pick self_paced() if you
    //    animate, on_demand() if you're mostly static.
    let mut pacer = FramePacer::self_paced();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                // Draw into `dma`, then hand it to the host in one call.
                dma.bind_for_rendering()?;
                // ... gl::Clear, gl::DrawArrays, etc. ...

                // submit_frame builds the wire Buffer message fresh, picks
                // the sync path (fence fd if both sides support it, else
                // glFinish), and sends. This is the whole submit step.
                conn.submit_frame(&dma, &gbm_egl)?;
                pacer.submitted(); // REQUIRED after every submit_frame
            }
            Frame::Reconfigure(c) => {
                // Host re-sent Configure (scale/region/time changed).
                // Resize to the region if it changed; never panics, logs
                // and keeps the old buffer on a transient failure.
                dma.resize_or_keep(&gbm_egl, c.region_w, c.region_h, PLUGIN_NAME);
                // ... update any state derived from the Configure ...
            }
            Frame::Shutdown => return Ok(()),
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("veiland-{PLUGIN_NAME}: {e}");
        std::process::exit(1);
    }
}
```

`conn.submit_frame(&dma, &gbm_egl)` is the one call to reach for. It replaces
the plumbing every plugin used to copy-paste: building the `Buffer` wire
message by hand, caching a `fast_path` bool, and branching on
`glFlush`+fence vs `glFinish`. You almost never construct a `Buffer` yourself
anymore. If you need the raw control (a custom sync scheme, multiple buffers),
the lower-level path is still public and documented below under **The
low-level submit path** — `plugins/stress` uses it.

## The SDK surface — the only types you need

`veiland-plugin` exposes primitives you drive, **not** a framework you
hook into. There is no `run_plugin(trait)`. You own `main()` and the
loop. The public exports:

| Type | What it is |
|---|---|
| `Connection` | The socket to the host. Handshake + send/recv + `submit_frame`. |
| `DmaBuffer` | A GPU buffer wrapped as an FBO you render into. |
| `GbmEgl` | Your EGL context + GBM device. Created first. |
| `FramePacer` | The FrameDone/BufferReleased pacing state machine. |
| `Frame` | One loop turn: `Render`, `Reconfigure(Configure)`, `Shutdown`. |
| `SyncFence` | A GPU fence for the low-level sync path (`submit_frame` uses it for you). |
| `PluginError` | The error type everything returns. |

Plus three free/convenience helpers that most plugins use so they don't
hand-roll the boilerplate:

| Item | What it does |
|---|---|
| `conn.submit_frame(&dma, &gbm_egl)` | Build the `Buffer` message + pick sync path + send, in one call. The normal way to submit. |
| `dma.resize_or_keep(&gbm_egl, w, h, name)` | The `Frame::Reconfigure` one-liner: resize if changed, log, never panic. |
| `veiland_plugin::load_config::<C>(name)` | Read `VEILAND_PLUGIN_CONFIG`, deserialize, fall back to `Default` on missing/invalid. |

`Configure` is not exported by `veiland-plugin` — it's a `veiland-protocol`
type (`use veiland_protocol::Configure`), reached as the `Frame::Reconfigure`
payload. You rarely need to name it directly.

Verified method signatures (do not guess these):

```rust
// Connection
Connection::connect(name: &str, version: &str) -> Result<Connection, PluginError>
conn.wait_for_configure() -> Result<Option<Configure>, PluginError> // None = shutdown before first Configure
conn.submit_frame(&DmaBuffer, &GbmEgl) -> Result<(), PluginError> // the normal submit path
conn.host_supports_fence_fd() -> bool
conn.send_buffer(&Buffer, dmabuf_fd: BorrowedFd, fence_fd: Option<BorrowedFd>) -> Result<(), PluginError> // low-level
conn.send_buffer_destroy(id: u32) -> Result<(), PluginError>

// Convenience helpers (what real plugins use)
veiland_plugin::load_config::<C: DeserializeOwned + Default>(name: &str) -> C
dma.resize_or_keep(&GbmEgl, width: u32, height: u32, name: &str) // never returns/panics

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

2. **Submit with `conn.submit_frame(&dma, &gbm_egl)`.** It builds the
   `Buffer` message, picks the sync path (fence fd if both sides support
   it, else `glFinish`), and sends — correctly, every frame. Prefer it.
   Only if you drop to the low-level `send_buffer` path (see below) do you
   own the sync choice yourself, and then the rule is: pick it once from
   `conn.host_supports_fence_fd() && gbm_egl.supports_fence_fd()` and keep
   it fixed. Sending a fence the host didn't negotiate — or omitting one
   it expects — is a protocol violation.

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

## The low-level submit path

`conn.submit_frame(&dma, &gbm_egl)` is what you should use. This section is
only for the rare plugin that needs manual control (a custom sync scheme,
multiple buffers). `plugins/stress` is the one reference plugin on this path.

Under the hood, `submit_frame` does exactly this — the pattern to replicate
if you go manual:

```rust
// Decide the sync path ONCE, before the loop, and keep it fixed.
let fast_path = conn.host_supports_fence_fd() && gbm_egl.supports_fence_fd();

// Build the Buffer wire message. In v1 one buffer is reused every frame,
// so its fields only change on resize; id = 0, offset = 0 in v1.
let buf_msg = Buffer {
    id: 0,
    width: dma.width(), height: dma.height(),
    format: dma.format(), modifier: dma.modifier(),
    stride: dma.stride(), offset: 0,
};

// In the Render arm, after drawing:
if fast_path {
    unsafe { gl::Flush() };                 // flush without blocking
    let fence = SyncFence::create(&gbm_egl)?;
    conn.send_buffer(&buf_msg, dma.dmabuf_fd(), Some(fence.as_fd()))?;
} else {
    dma.finish();                           // glFinish: GPU fully drained
    conn.send_buffer(&buf_msg, dma.dmabuf_fd(), None)?;
}
pacer.submitted();                          // still REQUIRED
```

If you resize on this path, rebuild `buf_msg` after any `resize_to` that
returns `true` — the new buffer's fd/stride/modifier moved.

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
  `dma.resize_or_keep(&gbm_egl, c.region_w, c.region_h, PLUGIN_NAME)` in
  the `Reconfigure` arm to render at native resolution. It resizes only
  when the size changed, logs the reallocation, and on a transient
  allocation failure keeps the current buffer instead of crashing — it
  never returns an error or panics. You don't rebuild anything by hand:
  `submit_frame` reads the buffer's current fd/stride/modifier fresh each
  frame. (The lower-level `dma.resize_to(...) -> Result<bool, _>` still
  exists if you want to react to the reallocation yourself.)

## Config from the user

The user's config for your plugin arrives as JSON in the
`VEILAND_PLUGIN_CONFIG` environment variable (a JSON-serialized TOML
table). Use the SDK helper — it reads the env var, deserializes, and
falls back to `Default` on missing or invalid config, so it never
crashes:

```rust
#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct MyConfig { /* your fields, each with a sensible Default */ }

let cfg: MyConfig = veiland_plugin::load_config(PLUGIN_NAME);
```

See `plugins/clock` or `plugins/particles` for a real config struct
(fields, defaults, `#[serde(default)]`).

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

- **`plugins/gradient`** — the minimal complete plugin, `submit_frame` +
  `resize_or_keep` + `load_config`. Start here.
- **`plugins/particles`** — a self-paced animated plugin with per-item
  state and config (also `sakura`, `snow`, `rain`, `embers`, `fireflies`
  for more particle/effect variants).
- **`plugins/wallpaper`** — an on-demand plugin that loads an image.
- **`plugins/vignette`, `plugins/blobs`, `plugins/parallax`** —
  transparent overlays; good references for premultiplied-alpha shaders.
- **`plugins/stress`** — the one plugin on the low-level `send_buffer`
  path; read it only if you need manual sync control.
- **`docs/protocol.md`** — the wire protocol (the authority).
- **`docs/plugin-api.md`** — the full SDK API reference.
- **`docs/config.md`** — how users configure plugins.

## Quick checklist before you call it done

- [ ] `pacer.submitted()` after every submit (`submit_frame`/`send_buffer`)
- [ ] Submitting via `conn.submit_frame(&dma, &gbm_egl)` (not hand-rolled sync)
- [ ] `self_paced()`/`on_demand()` matches whether you animate
- [ ] Transparency premultiplied; shader comments ASCII-only
- [ ] No panic/unwrap on host messages; `Disconnected` exits cleanly
- [ ] `region_w`/`region_h` used as physical px (not multiplied by scale)
- [ ] Config parse failures fall back to defaults, never crash
