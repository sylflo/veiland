# veiland

A Wayland screen locker with process-isolated, GPU-accelerated plugins.

## What it is

Veiland is a screen locker for Wayland compositors that support `ext-session-lock-v1`. It does the locking, the authentication, and the compositing — and delegates everything else (clocks, weather, media controls, animated wallpapers, shader-driven backgrounds) to plugins running in their own processes.

Plugins render with OpenGL into GPU buffers and hand those buffers to the locker via DMA-BUF over a Unix socket. No CPU-side pixel copying. No shared address space. A buggy plugin cannot crash the locker; a malicious plugin cannot read your keystrokes.

## Why it exists

The Wayland screen locker space has a hole in it. Polished lockers like hyprlock are config-driven with no plugin system. Lockers with plugins (gtklock) are GTK-only, locked to one toolkit, and load plugins as `.so` files in the same process — so a buggy module can take down the locker, and there is no isolation between modules and the auth path.

Nobody is shipping a locker with all of:

- **Plugins isolated in separate processes** — a crashing plugin closes its socket; the locker draws a fallback and moves on.
- **No keyboard access for plugins** — the password buffer lives in the trusted core; plugins receive only configuration, time ticks, and (optionally) clicks within their own region. Keystrokes never leave the core.
- **GPU-accelerated rendering** — plugins use OpenGL freely in their own EGL contexts and share results via DMA-BUF. Shader-driven wallpapers, particle effects, and complex animated UIs run at full GPU speed.
- **Compositor-portable** — works on any compositor that supports `ext-session-lock-v1` (wlroots-based compositors, KDE, and others as adoption grows).

The Linux customization community is real and active and wants this. Veiland aims to be the answer.

## Architecture in one paragraph

Veiland-core is a Wayland client that owns the `ext-session-lock-v1` surface, handles PAM authentication, and composites the final image. It spawns plugin processes as children. Plugins are independent programs that connect to the core over a Unix socket, allocate GPU buffers via GBM, render into them with their own OpenGL contexts, and send the buffer file descriptors to the core via `SCM_RIGHTS`. The core imports the fds as `EGLImage` textures and composites them onto the lock surface. No pixel data ever crosses CPU memory. All security-critical operations (input handling, password buffer, PAM, unlock decision) run in the trusted core process; plugins are untrusted and sandboxed by process boundaries.

## Status

M0 (C POC validating cross-process DMA-BUF) is complete and archived in `poc/`. M1 (Rust lock surface with GLES + Escape-to-unlock) and M2 (one plugin process producing a GPU-rendered gradient, core importing the dmabuf and compositing it onto the lock surface) are complete on dual-monitor Hyprland with the NVIDIA proprietary driver. The plugin is started manually for now; the core spawns plugins itself starting at M3. Sway validation still pending. M3 (defining the wire protocol -- `HELLO`, `CONFIGURE`, `BUFFER`, etc. -- and replacing the hand-rolled M2 header) is next.

See [`CLAUDE.md`](CLAUDE.md) for detailed architecture notes and design decisions.

## Design principles

1. **Process isolation is non-negotiable.** Lockscreens are security-sensitive. A crashing or malicious plugin must not be able to compromise the locker.
2. **The plugin API is a one-way door.** Get versioning and capability negotiation right from day one. Plugin authors should not have to rewrite their code every release.
3. **OpenGL only.** Vulkan's complexity buys nothing for compositing a handful of textured quads. OpenGL has a much larger plugin-author community.
4. **The core stays small.** Anything that can be a plugin should be a plugin. The core does locking, auth, compositing, and that's it.
5. **Ship the small thing.** A working locker with three plugins is better than a perfect plugin API with no real users. The shape of the API will be shaped by the first plugin authors who use it.

## Building

Linux only. Requires `pkg-config`, a C compiler, Mesa (libgbm, libEGL, libGLESv2), libdrm, and a Wayland compositor implementing `ext-session-lock-v1`.

NixOS users: `nix-shell` will pull in all dependencies. See `shell.nix`.

## Plugin development

(Coming. Once the protocol stabilizes there will be a `veiland-plugin` library and a sample plugin showing the minimum boilerplate.)

### Do plugins have to use OpenGL?

No. Plugins are processes that produce buffers. *How* a plugin paints into its buffer is up to the plugin author — Cairo (with Pango for text), pure OpenGL via its own EGL context, or anything else that can fill a DMA-BUF. The core composites the result as a texture; it does not know or care how the pixels got there.

Animated, shader-heavy content (live wallpapers, particle effects) is what the GPU path is built for. Static or rarely-updated content (a clock face, a date string) is often easier to draw with Cairo and copy in occasionally — both are fully supported.

## References

The Linux GPU stack (GBM, EGL, dmabuf import) is sparsely documented. There's no canonical tutorial — the reference for these APIs is mostly other open-source code. Useful starting points if you're hacking on veiland-core or writing a plugin from scratch:

**Code to read**

- [`kmscube`](https://gitlab.freedesktop.org/mesa/kmscube) — the canonical headless GBM + EGL example. Renders a spinning cube via DRM/KMS in ~800 lines of C. Closest match to what veiland's plugin processes do.
- [`wlroots`](https://gitlab.freedesktop.org/wlroots/wlroots) — production-quality renderer using GBM + EGL + dmabuf import. See `render/gles2/` for the GLES path and `render/allocator/gbm.c` for buffer allocation. The reference implementation when something in veiland-core misbehaves.
- [`swaylock-plugin`](https://github.com/mstoeckl/swaylock-plugin) — closest project to veiland in spirit. Demonstrates `ext-session-lock-v1` lifecycle and delegating background rendering to another process.
- [`shaderbg`](https://github.com/danielfvm/shaderbg) — smallest readable EGL + `wlr-layer-shell` setup. A good first read.

**Specs and APIs**

- [Khronos EGL Registry](https://registry.khronos.org/EGL/) — canonical specs for every `EGL_*` extension. Dense, exhaustive, the source of truth.
- [`EGL_EXT_image_dma_buf_import`](https://registry.khronos.org/EGL/extensions/EXT/EGL_EXT_image_dma_buf_import.txt) — the load-bearing extension that lets veiland-core import a plugin's GPU buffer as a sampleable texture.
- [`ext-session-lock-v1`](https://wayland.app/protocols/ext-session-lock-v1) — the Wayland protocol veiland-core uses to actually lock the screen.
- [`linux-dmabuf-v1`](https://wayland.app/protocols/linux-dmabuf-v1) — Wayland's standard buffer-sharing protocol. Not used directly (veiland's internal protocol is its own), but the design echoes it.

**Headers as documentation**

In the Mesa/Khronos ecosystem, the headers themselves are often the best docs:

- `<gbm.h>` — comments above each function. There is no separate man page or website; this is *the* documentation.
- `<EGL/egl.h>` and `<EGL/eglext.h>` — same. Extension headers are the only docs for many features.

**Search trick**

When a function isn't documented anywhere readable, clone `kmscube` and `wlroots` and `git grep` for the function name. Three real call sites teach a function faster than any prose ever would.

## Compatibility

Targets any compositor implementing `ext-session-lock-v1`. Tested primarily on Hyprland and Sway during development.

Other compositors that implement the protocol (KDE Plasma, niri, Wayfire, river, and other wlroots-based compositors) should work but are not regularly tested. GNOME's support for `ext-session-lock-v1` has historically been partial — treat it as untested. Compatibility patches welcome.

## Non-goals

- X11 support. Wayland-only by design.
- Cross-platform support. Linux-only — DMA-BUF and GBM are Linux-specific.
- Vulkan plugins. See design principles.
- Hot-reloading plugins without restart.
- Networked plugins.
- A built-in "plugin store." Plugins are programs the user installs and trusts deliberately.

## Future directions (not in scope yet)

Things that aren't planned for the foreseeable future but might make sense later:

- **Lua plugins.** The plugin protocol is language-agnostic (it's a Unix socket and a wire format), so anything that can speak the protocol can be a plugin. A `veiland-lua-runner` host binary that runs Lua scripts as their own plugin processes — same security model as native plugins, much lower barrier to entry for plugin authors who don't want to write Rust. The realistic Lua API would be declarative 2D drawing primitives rather than raw GPU access; shader-heavy plugins stay native. Deferred until after the native plugin API has stabilized and there's real ecosystem feedback to inform the Lua API shape.

  Concerns to validate before committing to a Lua runner:

  1. **Audience mismatch.** Veiland's pitch is GPU acceleration. If the people who actually want to write plugins are shader authors (Shadertoy refugees, Hyprland/niri customizers), they want GLSL and a buffer — not a scripting layer that hides the GPU. A Lua runner serves config-tinkerers writing clocks and widgets, which may or may not be the audience that shows up.
  2. **API surface drift.** A declarative 2D API (draw text, draw image, draw rect) is effectively a second plugin API in parallel with the native one. Two APIs means two sets of capabilities to keep in sync, two sets of docs, and decisions about whether features land in one or both.
  3. **Marginal value over native.** If the native plugin API ends up small and well-documented, the gap Lua fills (avoiding Rust) may be narrower than expected — especially if a `veiland-plugin` C wrapper or a Python binding shows up first from the community.

  How to check whether these are real, post-M7:

  - Look at what plugin authors are actually building. If the first ~10 plugins are shader-heavy, Lua is a distraction. If they're mostly clocks, weather widgets, and notification displays, Lua has a real audience.
  - Watch the issue tracker and any community channels for "how do I write a plugin without learning Rust" — count the askers, not just the asks.
  - Before building anything, sketch the Lua API against three concrete plugins those askers say they want, and check whether the declarative-2D surface actually covers them. If two of the three need an escape hatch to raw GL, the Lua runner isn't the right shape.

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).

Plugins communicate with the core over a Unix socket, so plugin authors are free to license their plugins under whatever terms they like.

## Naming

"Veiland" is from "veil" — something that obscures what's behind it — and the resemblance to "Wayland" is deliberate, indicating what it's built for. Pronounced "veil-land." The project is unaffiliated with the Wayland project.