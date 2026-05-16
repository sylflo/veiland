# CLAUDE.md

Context for Claude Code working on veiland. Read this before making changes.

## What veiland is

A Wayland screen locker with process-isolated, GPU-accelerated plugins. Veiland-core handles the lock lifecycle, PAM authentication, keyboard input, and compositing. Plugins handle everything else (clocks, wallpapers, widgets, animated backgrounds) and run as separate processes that communicate with the core over a Unix socket. Plugins render with OpenGL into GPU buffers and share those buffers with the core via DMA-BUF — no CPU-side pixel copying, full process isolation.

## Status

Starting from scratch. Nothing is implemented yet. The architecture in this document is the plan; building it is the work ahead.

The architecturally critical mechanism is cross-process DMA-BUF buffer sharing (a plugin's GPU-rendered buffer being sampled by the core's GL context, with no CPU readback). This is well-established on Mesa with `EGL_EXT_image_dma_buf_import`, but it should be validated on the target hardware before committing to the rest of the system. M0 below exists for that purpose.

## Decisions already made — do not re-litigate

- **Name:** veiland (`veiland-core` for the main binary, `veiland-plugin` for the plugin library, `veiland-<name>` for individual plugins).
- **License:** GPL-3.0-or-later. Plugins communicate over a Unix socket, so plugin authors can use any license they want for their plugins.
- **Language:** Rust for veiland proper. Untrusted IPC input from plugins, long-lived security-sensitive process, concurrent event loops — all of these strongly favor Rust's memory safety and concurrency guarantees over C. The Wayland/EGL/GBM bindings live in a small number of FFI-wrapping modules; the rest of the codebase is safe Rust. **Exception: M0 (the POC) is in C.** Reference codebases for EGL + GBM + `SCM_RIGHTS` are all C, the POC is throwaway, and we want to validate the architecture without also fighting Rust FFI on day one. M1 onward is Rust, written from scratch — do not port the POC.
- **Graphics:** OpenGL only. Not Vulkan. Lockscreens composite a handful of textured quads — Vulkan's complexity buys nothing, and OpenGL has a much larger plugin-author community.
- **Plugin isolation:** separate processes. Not `.so` modules. The security and crash-isolation guarantees are non-negotiable.
- **Plugin rendering:** DMA-BUF buffer sharing as the primary path. Each plugin has its own EGL context and GBM device.
- **OS target:** Linux only. DMA-BUF and GBM are Linux-specific.
- **Compositor target:** any compositor implementing `ext-session-lock-v1`. Tested primarily on Hyprland and Sway during development.

## Why this architecture

Two non-negotiable constraints drive the design:

1. **A crashing plugin must not take down the locker.** A locker crashing back to a logged-in desktop is a security hole.
2. **A malicious or compromised plugin must not be able to read keystrokes meant for the password field.**

Both point to the same conclusion: one plugin = one process. Process isolation is the right primitive — the kernel enforces both crash isolation and memory/fd isolation for free. This rules out dynamically-loaded `.so` plugins; the convenience is not worth the security cost on a lockscreen.

## Trust boundaries — load-bearing, do not blur

- **Veiland-core (trusted):** owns the `ext-session-lock-v1` lock surface, holds keyboard focus, runs PAM, manages the password buffer, owns the unlock decision. Composites plugin output onto the lock surface. Never loads untrusted code.
- **Plugin processes (untrusted):** render pixels into buffers and hand the buffers to the core. Receive only the events the core chooses to forward (configuration, time ticks, optionally clicks within their own region). Never receive raw keyboard input. Never see the password buffer. Never make the unlock decision.

When in doubt about where functionality goes: if it touches auth, it's in the core; if it's UI, it's in a plugin.

## Threat model

A useful frame for what veiland's architecture protects and what it doesn't. Use it to evaluate any change that touches IPC, input routing, or the unlock path.

**What a malicious or compromised plugin *cannot* do**, by construction:

- **Trigger an unlock.** The unlock decision is `keyboard event → password buffer → PAM call → state change`. Plugins receive no keyboard events at all (the protocol forbids it; the core never forwards keyboard data). They have no IPC message that maps to "unlock." There is no API surface for this attack; it isn't sandboxed, it's *absent*.
- **Read the password buffer.** It lives in `mlock`'d memory in the core process. Plugins are separate processes with no shared address space. The kernel's process boundary enforces this for free.
- **Read another plugin's buffer.** Each plugin owns its dmabufs; the core composites but doesn't redistribute. Cross-plugin reads would require kernel-level cooperation that doesn't exist.
- **Execute code in the core's address space.** The dmabuf path is `data → GPU sampler → pixel output`. The bytes inside a dmabuf become pixel values via a fragment shader the core controls. They are never interpreted as instructions. The closest a malicious plugin can get to "code execution" is making the screen show garish patterns.

**What a malicious plugin *can* try**, and how we defend:

- **Denial of service via malformed IPC.** Send a buffer header claiming `width = 0xFFFFFFFF`, an fd that isn't a dmabuf, a modifier that doesn't exist, etc. → core panics on the import → locker dies → bad. Defense: **validate every field of every message before passing it to EGL, GBM, or any kernel call.** Reject implausible values; close the plugin's socket and draw a fallback for that region rather than crashing the locker. Never `.expect()` on plugin input.
- **Resource exhaustion.** Send a million buffer headers; allocate a 16GB texture; connect and stall forever. Defense: bound the number of in-flight buffers per plugin, bound the dimensions, time-out silent plugins. (Comes in alongside the M5 buffer pool — single-buffer M2/M3 already bounds the first one.)
- **Driver-level GPU exploit via pathological-but-accepted parameters.** Even if EGL accepts our values, the driver might integer-overflow. Defense: refuse values that obviously shouldn't make sense (sizes > 8192², stride < width × bpp, unknown modifiers). Belt-and-braces against driver bugs we can't fix.
- **UI deception.** Plugin draws a convincing "Login successful, click to continue" screen to make the user think they're unlocked when they're still locked. This is real, and process isolation doesn't help — the plugin is *supposed* to draw pixels. Defense: reserve a small core-painted "still locked" region of the screen that plugins cannot reach (a small lock icon at a fixed position, or a user-chosen color border around the screen). If it's there, you're locked. CLAUDE.md does not yet specify the trusted-region pattern; that's an M6+ concern when multiple plugins exist.

**Bottom line.** Process isolation alone protects against the dramatic attacks (code execution, password read, unlock trigger). The undramatic attacks (DoS, resource exhaustion, UI deception) are real, and most of the defensive work in M3+ is for them. "Plugins are untrusted input" applies to every byte they send, every fd they pass, every dimension they declare. That phrase from the coding conventions section is the single most important rule in the core.

## How plugin rendering works (conceptually)

OpenGL contexts are per-process. You cannot share a live GL context across processes. The naive workaround — `glReadPixels` into shared memory — forces a GPU→CPU readback every frame and defeats the point of GPU acceleration.

The correct mechanism is DMA-BUF. Each plugin has its own EGL context. The plugin allocates a GPU buffer via GBM (which gives back a file descriptor representing the buffer). The plugin renders into that buffer with its own OpenGL setup, then sends the fd to the core via `SCM_RIGHTS` over a normal Unix socket. The core imports the fd as an `EGLImage`, binds it as a GL texture, and composites it onto the lock surface. No pixel data ever crosses CPU memory. Both processes have their own OpenGL — only the buffer's contents are shared, at zero copy.

The IPC layer (Unix socket + `SCM_RIGHTS`) and the GPU layer (GBM allocation, EGL import) are orthogonal. The socket has no idea the fd is GPU memory. The GPU has no idea the buffer crossed processes. Keep these layers conceptually separate when reasoning about the code.

The required EGL extension is `EGL_EXT_image_dma_buf_import`, well-supported on Mesa.

## Plugin protocol shape (proposed, subject to refinement)

Direction: P→C means plugin to core, C→P means core to plugin.

- `HELLO` (P→C): plugin name, version, supported buffer types (`shm`, `dmabuf`), preferred type, declared input needs (`none`, `clicks-in-region`).
- `CONFIGURE` (C→P): assigned region (x, y, w, h), z-index, scale factor, theme info, current time/timezone.
- `BUFFER` (P→C): fd via `SCM_RIGHTS` + format + stride + size + sync fence fd.
- `FRAME_DONE` (C→P): cue plugin to render the next frame (frame-callback throttling, like Wayland itself).
- `INPUT_EVENT` (C→P): filtered events the plugin opted into. Never raw keyboard.
- `SHUTDOWN` (C→P): clean exit. If plugin doesn't respond, core SIGTERMs it.

Buffer-type negotiation: dmabuf is the primary path. Shm is supported for plugins that explicitly opt in (genuinely-static widgets where GPU readback cost is acceptable). The protocol carries both from v1 — don't ship shm-only and bolt dmabuf on later. The whole pitch of veiland is GPU acceleration.

Keep the protocol small. Resist adding capabilities until plugin authors ask.

## Build incrementally — milestone order

Do not try to build everything at once. Each milestone produces something runnable and validates a specific assumption.

1. **M0 — POC: cross-process DMA-BUF:** two standalone processes, each with its own EGL/OpenGL context. Producer allocates a GBM buffer, renders an animated gradient into it, sends the dmabuf fd to the consumer via `SCM_RIGHTS` over a Unix socket. Consumer imports the fd as an `EGLImage`, binds it as a GL texture, displays it in a normal window (GLFW or similar — not a lock surface yet). Validates that the architecturally critical mechanism works on the target hardware before veiland proper is built. Discard or archive the POC code after M0; do not build M1 on top of it.
2. **M1 — Lock surface:** veiland-core only. Creates an `ext-session-lock-v1` surface, draws a solid color via OpenGL. Pressing Escape calls `unlock_and_destroy`. No plugins, no PAM, no password handling. Validates the lock lifecycle works on Hyprland and Sway.
3. **M2 — Lock + DMA-BUF plugin:** add one hardcoded plugin process the core spawns. Plugin renders an animated gradient into a dmabuf via GBM/EGL. Core imports it, composites it onto the lock surface. Still escape-to-unlock. Validates the full GPU chain inside a real lock surface.
4. **M3 — Real protocol:** define the wire format (`HELLO`, `CONFIGURE`, `BUFFER`, etc.). Plugin discovery via a config file or directory. The hardcoded plugin from M2 becomes a real plugin using the real protocol. The protocol crate (`veiland-protocol`) and a thin plugin-side helper crate (`veiland-plugin`) land here; see "Future direction: login manager" below for the five structural choices that should be baked into M3 so a future login-manager mode stays cheap.
5. **M4 — PAM:** add real password input handling and PAM authentication. Replace escape-to-unlock with proper auth. Password buffer in `mlock`'d memory.
6. **M5 — Buffer pool + sync fences:** replace single-buffer + `glFinish()` with 2–3 buffer pool, release messages, and explicit sync fences (`EGL_KHR_fence_sync` fd via `SCM_RIGHTS`). Production-quality rendering pipeline.
7. **M6 — Multiple plugins, z-order, region clipping:** the real plugin system. Multiple plugins compositing together with z-indexing.
8. **M7 — Reference plugins:** clock, simple wallpaper, shader background. Three plugins to validate the API and seed the ecosystem.

Don't skip ahead. M0 → M1 → M2 → ... in order. The motivation for each step is concrete; the order is chosen so each step validates something specific before building on it.

## Future direction: login manager

Veiland-the-locker and veiland-the-login-manager share ~70-80% of their architecture: both take over the display, gate keyboard input, run PAM, decide who gets access to a session, composite process-isolated GPU plugins. The login-manager port is a plausible long-term direction — the ecosystem hole is real (SDDM/GDM/LightDM/ly have no real plugin system), the audience overlaps with the locker's, and the structural decisions for the locker carry over cleanly. It is, however, **strictly post-1.0**. Login managers have an order of magnitude more system-integration complexity (`systemd-logind`/`elogind` interaction, seat management, VT allocation, session creation rather than session gating) and run as root, which makes mistakes much more expensive. Ship the locker first. Don't advertise the login-manager direction until the locker has real users.

Security risk is higher (root daemon, pre-auth surface) but bounded by the same design that protects the locker: plugins as unprivileged child processes, no `.so` loading, keyboard path never leaves the trusted core, small auditable auth core. The structural choices made for the locker become *more* valuable in the login-manager context, not less.

**M3 should bake in five structural enablers** so the eventual port stays cheap. None of these speculatively add login-manager features; they only avoid one-way doors that would close off the port.

1. **Tagged enum for messages, not flat structs with a type byte.** Easier to add variants without rewriting the parser. The `Message` type in `veiland-protocol` should be `enum`-shaped from day one.
2. **Tagged enum for `INPUT_EVENT` contents.** A login plugin's input events (e.g. "user selected `alice`") are meaningless to a locker; keeping the variant set open lets a future login mode add them without breaking the locker's parser. Cheap now, one-way door later.
3. **Spawn plugins via `socketpair` + `exec`-inherited fd, not via a filesystem socket path.** A login manager spawns plugins *before* any user session exists — `XDG_RUNTIME_DIR` may be unset, the plugin user may have no home, render-node permissions may differ. The `socketpair` form avoids the filesystem entirely and is the standard pattern (`swaylock-plugin` uses it). For the locker it's also simpler — no stale-file unlink, no path negotiation. M2 used the filesystem form because the plugin is started manually; M3 should switch.
4. **`criticality` field on `HELLO`.** Most plugins are decorative — death gets a fallback. Some plugins are essential (the password field in login-manager mode, but also conceivably in locker mode) — death must fail closed. One byte in HELLO, costs nothing now, removes a real problem later.
5. **Structured theme/config data, not free-form strings.** `CONFIGURE`'s theme info should be a typed struct (colors, accent, font), not a string blob. A login manager needs to switch themes mid-flow (system-default before user selection, user-specific after); a typed struct makes that forward-compatible.

**Deliberately *not* in M3:** session/account selection fields, multi-session context, privilege-drop hooks. Those are genuinely login-manager-specific and YAGNI applies — add them when the port actually starts. The five above are different: they're shape decisions, not feature decisions, and reversing them later would be expensive.

## Project structure (intended)

```
veiland/
  CLAUDE.md
  README.md
  LICENSE
  shell.nix              # NixOS dev environment
  Cargo.toml             # workspace root
  veiland-core/          # the locker binary (Rust crate)
    src/
    Cargo.toml
  veiland-plugin/        # library plugins link against (Rust crate)
    src/
    Cargo.toml
  veiland-protocol/      # shared protocol definitions (Rust crate)
    src/
    Cargo.toml
  plugins/               # reference plugins (each is its own crate)
    clock/
    wallpaper/
    shader-bg/
  docs/
    plugin-api.md
    architecture.md
  poc/                   # M0 throwaway POC (C, archived after M0)
```

This is a sketch; refine as the code grows. Don't create empty directories upfront — make them when there's code to put in them.

## Things to be careful about

- **Explicit sync at M5+.** Don't assume a buffer is rendered just because the fd arrived. The plugin should attach a sync fence fd that signals when GPU work is complete. Core waits on the fence before sampling. (M2 can use `glFinish` as a placeholder.)
- **Buffer lifecycle.** Plugin owns a small pool (2–3 buffers). Core sends back a `RELEASE` message when done sampling. Plugin reuses. Don't free buffers while the core might still be sampling.
- **Format negotiation.** Don't hardcode ARGB8888 in the protocol. Plugin advertises supported formats in `HELLO`; core picks one. (Hardcoding ARGB8888 is fine for M1–M3; abstract it at M5.)
- **Plugin death.** A closed plugin socket means the plugin died. Detect via EOF on read. Draw a fallback for that region or skip it. Log the event. Never block the locker on a dead plugin.
- **Plugin spawning.** Core spawns plugins as child processes with restricted environment. Consider seccomp filters and a minimal allowed-syscall set. Not necessary for early milestones; design with it in mind.
- **Region clipping.** Enforce in the core that a plugin can only render into its assigned region. The plugin only sees its own buffer dimensions; the core controls placement on the lock surface.
- **Password buffer hygiene.** `mlock` it so it never swaps to disk. Zero it after PAM call. Never log it. Never put it in a buffer shared with plugins.

## Reference projects worth reading

- **`swaylock-plugin`** — `ext-session-lock-v1` lifecycle and how to delegate background rendering to another process. Closest in spirit to veiland.
- **`shaderbg`** — smallest example of EGL + `wlr-layer-shell` setup. Few hundred lines. Good first read.
- **`mpvpaper`** — loading images/videos as GPU textures.
- **`swaybg`** — minimal `wlr-layer-shell` lifecycle.
- **`gtklock`** — proves the plugin/module idea works socially. Architecture is `.so`-based, which veiland deliberately is not.
- **Wayland's `wp_linux_dmabuf_v1`** — canonical buffer-sharing protocol shape. Veiland's internal protocol echoes its design but is not the same protocol.

## Coding conventions

- Plugin protocol messages: small, versioned, backwards-compatible. Adding fields later is fine; removing them is not.
- Error handling in the core: never `assert()` on anything a plugin sent. Plugins are untrusted input. Validate every field.
- Fds received from plugins: always close them when done. Leaking fds is easy and bad.
- Logging: tag every log line with the plugin name when it relates to plugin behavior. Helps debug which plugin is misbehaving.
- Use SPDX identifiers at the top of source files (`// SPDX-License-Identifier: GPL-3.0-or-later` for Rust/C, `# SPDX-License-Identifier: GPL-3.0-or-later` for shell/Nix). No long GPL preamble headers.

## How to work with me on this project

- **Small focused commits.** One logical change per commit. Easier to review, easier to revert.
- **Ask before adding dependencies.** This project's value is partly in being small. New deps need justification.
- **Security-critical paths get extra scrutiny.** Anything touching the password buffer, PAM, input routing, or the unlock decision: walk through it carefully, add comments explaining the threat model, prefer obvious-correct code over clever code.
- **Don't refactor opportunistically.** If a refactor is needed for the current change, do it; otherwise leave it for a focused refactor commit.
- **Match milestones.** If working on M2, don't add M5 features speculatively. The milestone order exists for a reason.
- **When in doubt, ask.** Especially on protocol shape, security boundaries, or anything that's a one-way door.

## Open questions

These are genuinely undecided. Don't assume an answer.

- Multi-monitor: does each output get its own plugin instance, or does one plugin span all outputs? (Leaning: each output gets its own instance, plugins declare per-output preference.)
- Plugin manifest file (declared capabilities, permissions) vs. relying on `HELLO` alone? (Manifest is more secure long-term but more bureaucracy. Likely defer to post-M7.)
- Sandbox plugins with bubblewrap/landlock by default, or leave to packagers? (Probably leave to packagers initially; document the recommendation.)
- What does a plugin's working directory look like — chrooted? Just `cd`'d to a known location? Full filesystem access? (Decide at M3 when plugin loading is real.)
- Which Rust Wayland crate ecosystem to use — `smithay-client-toolkit` + `wayland-client` (lower-level, more flexible) vs. higher-level abstractions? (Decide at M1. Leaning `smithay-client-toolkit` because that's what most Wayland Rust projects use.)