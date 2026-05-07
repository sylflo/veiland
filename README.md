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

Early — design phase. Nothing is implemented yet. The first milestone (M0) is a small C proof-of-concept that validates cross-process DMA-BUF buffer sharing on the target hardware before the Rust core is built on top of it.

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

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).

Plugins communicate with the core over a Unix socket, so plugin authors are free to license their plugins under whatever terms they like.

## Naming

"Veiland" is from "veil" — something that obscures what's behind it — and the resemblance to "Wayland" is deliberate, indicating what it's built for. Pronounced "veil-land." The project is unaffiliated with the Wayland project.