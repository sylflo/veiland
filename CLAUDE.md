# CLAUDE.md

Context for Claude Code working on veiland. Read this before making changes.

## What veiland is

A Wayland screen locker with process-isolated, GPU-accelerated plugins. Veiland-core handles the lock lifecycle, PAM authentication, keyboard input, and compositing. Plugins handle everything else (clocks, wallpapers, widgets, animated backgrounds) and run as separate processes that communicate with the core over a Unix socket. Plugins render with OpenGL into GPU buffers and share those buffers with the core via DMA-BUF — no CPU-side pixel copying, full process isolation.

## Current status

The locker works: 
- `ext-session-lock-v1`
- surface
- PAM authentication
- keyboard input
- password indicator
- process-isolated GPU plugins over DMA-BUF,
- multi-monitor
- hotplug is buggy


Reference plugins:
- wallpaper
- clock
- particles
- vignette
- label
- plus test plugins (blue-box, green-box, red-box, gradient, stress, sakura).

The codebase is structured as a Cargo workspace:
- `veiland-core` (locker binary)
- `veiland-plugin` (SDK library)
- `veiland-protocol` (wire format)
- `veiland-text` (text rendering helper)
- and per-plugin crates under `plugins/`.

**Known limitations / open work:**

- **Per-plugin frame rate:** all plugins run at the compositor's repaint rate. Deferred.
- **Hyprland fast-replug:** unplug + replug within ~5–10s sometimes panics at `eglSwapBuffers` with `invalid object N`. Lock survives via compositor compliance; recovery is TTY-kill. Deferred to the Wayland-integration refactor.

The architecturally critical mechanism — cross-process DMA-BUF buffer sharing — is validated and in production. Changes touching the dmabuf import/sampling path warrant the same care as the auth path.

## Decisions already made — do not re-litigate

- **Name:** veiland. The crate is `veiland-core` (it pairs with `veiland-plugin` and names the trusted core in the workspace/threat-model), but the installed binary users invoke is `veiland` — set via `[[bin]]` in `veiland-core/Cargo.toml`. `veiland-plugin` is the plugin library, `veiland-<name>` the individual plugins.
- **License:** GPL-3.0-or-later. Plugins communicate over a Unix socket, so plugin authors can use any license they want.
- **Language:** Rust. Untrusted IPC input from plugins, long-lived security-sensitive process, concurrent event loops. The Wayland/EGL/GBM bindings live in a small number of FFI-wrapping modules; the rest is safe Rust.
- **Graphics:** OpenGL only. Not Vulkan. Lockscreens composite a handful of textured quads — Vulkan's complexity buys nothing.
- **Plugin isolation:** separate processes. Not `.so` modules. Non-negotiable.
- **Plugin rendering:** DMA-BUF buffer sharing as the primary path. Each plugin has its own EGL context and GBM device.
- **OS target:** Linux only. DMA-BUF and GBM are Linux-specific.
- **Compositor target:** any compositor implementing `ext-session-lock-v1`. Tested on Hyprland and Sway.

## Trust boundaries — load-bearing, do not blur

- **Veiland-core (trusted):** owns the `ext-session-lock-v1` lock surface, holds keyboard focus, runs PAM, manages the password buffer, owns the unlock decision. Composites plugin output onto the lock surface. Never loads untrusted code.
- **Plugin processes (untrusted):** render pixels into buffers and hand the buffers to the core. Receive only the events the core chooses to forward (configuration, time ticks, optionally clicks within their own region). Never receive raw keyboard input. Never see the password buffer. Never make the unlock decision.

When in doubt about where functionality goes: if it touches auth, it's in the core; if it's UI, it's in a plugin.

## Threat model

**What a malicious or compromised plugin cannot do**, by construction:

- **Trigger an unlock.** The unlock decision is `keyboard event → password buffer → PAM call → state change`. Plugins receive no keyboard events. No IPC message maps to "unlock." The API surface is absent, not just sandboxed.
- **Read the password buffer.** It lives in `mlock`'d memory in the core process. Process boundary enforces this.
- **Read another plugin's buffer.** Each plugin owns its dmabufs; the core composites but doesn't redistribute.
- **Execute code in the core's address space.** The dmabuf path is `data → GPU sampler → pixel output`. Bytes inside a dmabuf become pixel values, never instructions.

**What a malicious plugin can try**, and how we defend:

- **DoS via malformed IPC.** Validate every field before passing to EGL, GBM, or any kernel call. Reject implausible values; close the plugin's socket and draw a fallback rather than crashing the locker. Never `.expect()` on plugin input.
- **Resource exhaustion.** Bound in-flight buffers per plugin, bound dimensions, time-out silent plugins.
- **Driver-level GPU exploit.** Refuse values that obviously shouldn't make sense (sizes > 8192², stride < width × bpp, unknown modifiers).
- **UI deception.** Plugin draws a "Login successful" screen while the lock is still active. Reserve a small core-painted trusted region plugins cannot reach.

**Bottom line.** Process isolation protects against the dramatic attacks (code execution, password read, unlock trigger). DoS, resource exhaustion, and UI deception are real. "Plugins are untrusted input" applies to every byte they send, every fd they pass, every dimension they declare.

## How plugin rendering works

OpenGL contexts are per-process. Each plugin has its own EGL context and GBM device. The plugin allocates a GPU buffer via GBM (yielding a file descriptor), renders into it with its own GL setup, then sends the fd to the core via `SCM_RIGHTS` over a Unix socket. The core imports the fd as an `EGLImage`, binds it as a GL texture, and composites it onto the lock surface. No pixel data crosses CPU memory. Both processes have their own OpenGL — only the buffer's contents are shared, at zero copy.

The IPC layer (Unix socket + `SCM_RIGHTS`) and the GPU layer (GBM allocation, EGL import) are orthogonal. Keep them conceptually separate. The required EGL extension is `EGL_EXT_image_dma_buf_import`, well-supported on Mesa.

## Protocol

The wire format is specified in `docs/protocol.md`. The Rust implementation is in `veiland-protocol/`. If they disagree, the spec wins.

Key protocol facts for reasoning about new work:
- Transport: `AF_UNIX SOCK_SEQPACKET`, spawned via `socketpair` + `exec`. No filesystem socket path.
- Messages are tagged enums. `ClientMessage` (plugin→host): `Hello`, `Buffer`, `BufferDestroy`. `ServerMessage` (host→plugin): `Configure`, `FrameDone`, `BufferReleased`, `Shutdown`.
- `Buffer` carries a dmabuf fd (and optionally a sync-fence fd) via `SCM_RIGHTS`. All other messages carry zero fds.
- Plugins never receive keyboard input. No protocol message carries keystrokes in any direction — this is a protocol property, not a runtime filter.
- Plugin config is passed via `VEILAND_PLUGIN_CONFIG` environment variable (JSON-serialised TOML table).

## Plugin SDK shape

`veiland-plugin` exposes **imperative primitives the author drives**, not a framework the author hooks into:

- `Connection::connect(name, version)` — reads fd from env, handshakes, sends Hello.
- `Connection::wait_for_configure()` — blocks until the first Configure.
- `FramePacer::self_paced()` / `FramePacer::on_demand()` — encapsulates the FrameDone/BufferReleased state machine. Yields `Frame::Render`, `Frame::Reconfigure`, or `Frame::Shutdown`.
- `DmaBuffer` — GBM/EGL buffer allocation helpers.

The plugin author owns `main()`, the render code, and the event loop. If a `run_plugin()` framework is ever wanted, it would be a thin layer over these — don't add it speculatively.

## Project structure

```
veiland/
  CLAUDE.md
  README.md
  LICENSE
  shell.nix
  Cargo.toml            # workspace root
  veiland-core/         # locker binary
    src/
      app/              # Wayland event loop, lock surface, output, input
      auth/             # PAM session
      plugin/           # host-side plugin lifecycle (spawn, connection, state, dmabuf import)
      renderer.rs       # GL compositing
      config.rs
      region.rs
      main.rs
  veiland-plugin/       # SDK library plugins link against
    src/
    tests/
  veiland-protocol/     # wire format types and codec
    src/
  veiland-text/         # text rendering helper (cosmic-text + GL atlas)
    src/
  plugins/              # reference plugins
    wallpaper/
    clock/
    particles/
    vignette/
    label/
    sakura/
    gradient/
    blue-box/ green-box/ red-box/   # test/demo
    stress/             # load generator (ignores region by design)
  docs/
    protocol.md
    plugin-api.md
    config.md
    examples/
  poc/                  # archived M0 C POC
```

## Things to be careful about

- **Explicit sync.** Don't assume a buffer is rendered just because the fd arrived. The plugin attaches a sync fence fd; the core waits on it before sampling.
- **Buffer lifecycle.** The core releases a buffer before the plugin sends the next. Never free a buffer the core may still be sampling.
- **Format negotiation.** Don't hardcode ARGB8888 in new protocol work. Plugin advertises supported formats in `Hello`; core picks one.
- **Plugin death.** A closed socket means the plugin died. Detect via EOF on read. Draw a fallback for that region. Log the event. Never block the locker on a dead plugin.
- **Region clipping.** Enforce in the core that a plugin can only render into its assigned region. The plugin only sees its own buffer dimensions.
- **Password buffer hygiene.** `mlock`'d, zeroed after PAM call, never logged, never in any buffer shared with plugins.
- **No panic on plugin input.** Never `.expect()` / `.unwrap()` on anything a plugin sent or any fd it passed. Validate first; on bad input, close the socket and continue.

## Future direction: login manager

Veiland-the-locker and veiland-the-login-manager share ~70–80% of their architecture. The port is a plausible long-term direction but is **strictly post-1.0** — login managers have an order of magnitude more system-integration complexity (`systemd-logind`, seat management, VT allocation, session creation) and run as root.

The five structural enablers (tagged message enums, open `INPUT_EVENT` variant, `socketpair`-spawning, `criticality` field on `Hello`, typed theme struct in `Configure`) are already in the codebase. No further login-manager prep is needed before 1.0.

## Coding conventions

- Plugin protocol messages: small, versioned, backwards-compatible. Adding fields is fine; removing them is not.
- Error handling in the core: never `assert()` on anything a plugin sent. Plugins are untrusted input.
- Fds received from plugins: always close when done. Leaking fds is easy and bad.
- Logging: tag every log line with the plugin name when it relates to plugin behavior.
- SPDX identifiers at the top of source files (`// SPDX-License-Identifier: GPL-3.0-or-later`). No long GPL preamble headers.
- GLSL source lives in byte-string literals (`b"..."`). Keep shader comments ASCII-only — no em dashes, smart quotes, or non-ASCII characters.

## How to work with me on this project

- **Small focused commits.** One logical change per commit.
- **Ask before adding dependencies.** This project's value is partly in being small. New deps need justification.
- **Security-critical paths get extra scrutiny.** Anything touching the password buffer, PAM, input routing, or the unlock decision: walk through it carefully, prefer obvious-correct code over clever code.
- **Don't refactor opportunistically.** If a refactor is needed for the current change, do it; otherwise leave it for a focused refactor commit.
- **When in doubt, ask.** Especially on protocol shape, security boundaries, or anything that's a one-way door.
