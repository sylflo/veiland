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
- reliability: Hyprland (NVIDIA) survives suspend/resume, DPMS off/on, and monitor unplug/replug (incl. fast replug, unplug-all, hotplug-in while locked). Unplug/replug also passes on Sway; Sway suspend/DPMS sweep still pending. See the two "Formerly-known issue" notes below for the hotplug crashes found and fixed here.


Reference plugins:
- wallpaper
- clock
- label
- vignette
- particle family (particles, sakura, snow, rain, embers, fireflies)
- ambient backgrounds/overlays (gradient, parallax, blobs)
- plus one test plugin: stress (load generator).

The codebase is structured as a Cargo workspace:
- `veiland-core` (locker binary)
- `veiland-plugin` (SDK library)
- `veiland-protocol` (wire format)
- `veiland-text` (text rendering helper)
- and per-plugin crates under `plugins/`.

**Known limitations / open work:**

- **Per-plugin frame rate:** all plugins run at the compositor's repaint rate. Deferred.

**Formerly-known issue, now resolved:** the Hyprland fast-replug crash (unplug + replug within ~5–10s panicking at `eglSwapBuffers` with `invalid object N`) was caused by the `update_output` rebind path handling Hyprland re-advertising a surviving monitor's `wl_output` under a new global. The output-tracking refactor `cb0e608` (2026-06-13) tracks outputs by registry numeric ID and dropped that compositor-specific rebind path, so the quirk now falls out as a plain remove + add. Testing on Hyprland (2026-07-09) no longer reproduces the crash, including fast/no-wait replug. Keep an eye on it under the dmabuf-import care bar, but it is not a documented open limitation anymore.

**Formerly-known issue, now resolved (2026-07-10):** unplugging *all* monitors then replugging panicked at `eglSwapBuffers: BadSurface` (`lock.rs` bootstrap swap) on both Hyprland and Sway — during the rapid teardown+rebuild the freshly-created EGL surface was already stale by the time we swapped, and both swap sites `.expect()`ed. Fix: both swaps (`lock.rs` bootstrap, `mod.rs` repaint) now match on `Err(egl::Error::BadSurface)` and skip+retry (log, `return`, leave `needs_paint`) instead of crashing; other EGL errors still panic. Session stayed locked throughout regardless (compositor compliance). Residual, non-crash: after unplug-*all* the scene takes a moment to repaint (outputs + plugins rebuilt from scratch; the skip-retry doesn't request an immediate frame callback). Single-monitor unplug is instant. That recovery latency is a deferred polish item, not a bug.

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

**What a plugin cannot do by construction** (holds even against a hostile plugin, because the mechanism is absent, not filtered):

- **Trigger an unlock.** The unlock decision is `keyboard event → password buffer → PAM call → state change`. Plugins receive no keyboard events. No IPC message maps to "unlock." The API surface is absent, not just sandboxed.
- **Receive keystrokes / see the password in a message.** No protocol message carries keyboard input in any direction. The password never appears in any buffer or message a plugin is handed.
- **Read another plugin's buffer.** Each plugin owns its dmabufs; the core composites but doesn't redistribute.
- **Execute code in the core's address space.** The dmabuf path is `data → GPU sampler → pixel output`. Bytes inside a dmabuf become pixel values, never instructions.

**What the process boundary does NOT do — be precise about this.** Plugins run as the same UID as the core. The boundary gives crash isolation and accidental-bug containment (both genuinely valuable), but it is **not** a security boundary against hostile same-user code:

- The password buffer being `mlock`'d prevents *swapping*, not *reading*. With `ptrace_scope=0`, a same-UID plugin could `PTRACE_ATTACH` the core or read `/proc/<pid>/mem`. The core calls `prctl(PR_SET_DUMPABLE, 0)` at startup (see `main.rs` §0, opt out with `VEILAND_ALLOW_DUMP=1`) to deny that and suppress core dumps of the buffer — defense-in-depth, not an absolute wall. Root, a kernel bug, or a privileged debugger still wins. There is no seccomp/landlock sandbox yet.
- So: **first-party plugins** (this repo) are code we review and vouch for. **Third-party plugins** are same-user code the user chose to install; we reduce risk, we don't guarantee zero. Do not write prose (README, docs, comments) that claims a plugin "cannot read the password" as an unqualified fact — qualify it as "cannot by protocol construction; a hostile same-UID plugin is a residual risk we harden against but do not eliminate."

**What a malicious plugin can try**, and how we defend:

- **DoS via malformed IPC.** Validate every field before passing to EGL, GBM, or any kernel call. Reject implausible values; close the plugin's socket and draw a fallback rather than crashing the locker. Never `.expect()` on plugin input.
- **Resource exhaustion.** Bound in-flight buffers per plugin, bound dimensions, time-out silent plugins.
- **Driver-level GPU exploit.** Refuse values that obviously shouldn't make sense at the codec (sizes > 8192², `stride < width`). Format and modifier acceptability are host-stack concerns: the codec accepts any format/modifier and lets `eglCreateImage` be the gate — an import failure closes the plugin's socket and draws a fallback.
- **UI deception.** Plugin draws a "Login successful" screen while the lock is still active. The core composites the password UI *last*, on top of all plugin output, so a plugin can't draw over it. This is a paint-order guarantee, not a reserved exclusive region — plugins still render everywhere beneath and around it (a stronger reserved region is a possible future hardening).

**Bottom line.** Process isolation makes the dramatic attacks (unlock trigger, code execution in the core, seeing the password in a message) absent by construction, and `PR_SET_DUMPABLE` raises the bar against casual same-UID memory snooping. It is not a substitute for a sandbox against a determined hostile third-party plugin. DoS, resource exhaustion, and UI deception are real. "Plugins are untrusted input" applies to every byte they send, every fd they pass, every dimension they declare.

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
  flake.nix             # package + dev shell + checks; CI = `nix flake check` (fmt+clippy) + `nix build` (build+tests)
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
    label/
    vignette/
    particles/ sakura/ snow/ rain/ embers/ fireflies/   # particle family
    gradient/ parallax/ blobs/      # ambient backgrounds/overlays
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
- **Format negotiation.** Don't hardcode ARGB8888 in new protocol work. `Hello` carries only name + version — there is no format list. Each `Buffer` states its own `format`/`modifier`, and the codec accepts any value; `eglCreateImage` is the acceptance gate at import. (If explicit negotiation is ever wanted, a format list on `Hello` is the natural place — it does not exist today.)
- **Plugin death.** A closed socket means the plugin died. Detect via EOF on read. Draw a fallback for that region. Log the event. Never block the locker on a dead plugin.
- **Region clipping.** Enforce in the core that a plugin can only render into its assigned region. The plugin only sees its own buffer dimensions.
- **Password buffer hygiene.** `mlock`'d, zeroed after PAM call, never logged, never in any buffer shared with plugins.
- **No panic on plugin input.** Never `.expect()` / `.unwrap()` on anything a plugin sent or any fd it passed. Validate first; on bad input, close the socket and continue.

## Future direction: login manager (not on the near-term roadmap)

A login manager shares maybe ~70–80% of the locker's architecture, so the port is *conceivable* someday — but it is **not planned for a good while** and nothing in the current work is being shaped toward it. Treat it as a distant "maybe," not a roadmap item: don't add login-manager scaffolding, don't preserve hooks "for later," and don't let it influence protocol or API decisions now. If it ever happens it's well post-1.0, and it carries an order of magnitude more system-integration complexity (`systemd-logind`, seat management, VT allocation, session creation) and runs as root.

For the record, two things the codebase already has would transfer cleanly if that day comes — the tagged message enums (`ClientMessage`/`ServerMessage`, extensible without breaking v1) and `socketpair`-spawning — but they exist because the *locker* needs them, not as login-manager prep. Things a login manager would additionally want (an input-event message, a plugin `criticality` field on `Hello`, a typed theme struct in `Configure`) are **not** in the codebase and should not be added speculatively.

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
