# veiland

A Wayland screen locker with process-isolated, GPU-accelerated plugins.

## What it is

Veiland is a screen locker for Wayland compositors that support `ext-session-lock-v1`. It handles locking, PAM authentication, and compositing — and delegates everything else (clocks, wallpapers, widgets, animated backgrounds) to plugins running in their own processes.

Plugins render with OpenGL into GPU buffers and hand those buffers to the locker via DMA-BUF over a Unix socket. No CPU-side pixel copying. No shared address space. A crashing plugin cannot take down the locker; a malicious plugin cannot read your keystrokes.

## Why it exists

Veiland was built around one design constraint: plugins must run in separate processes. That decision has cascading consequences — a crashing plugin can't take down the locker, a malicious plugin can't read keystrokes, and GPU acceleration falls out naturally because each plugin has its own EGL context and shares results via DMA-BUF rather than copying pixels across process boundaries.

## Status

The locker works. The full feature set is in production:

- `ext-session-lock-v1` lock surface with PAM authentication
- Password indicator (configurable dots)
- Process-isolated GPU plugins over DMA-BUF
- Multi-monitor support with per-plugin output targeting
- Text rendering via `veiland-text` (cosmic-text backend, HiDPI-aware)

**Reference plugins:** wallpaper (PNG/JPEG), clock (time + date), particles (animated dots), vignette (corner darkening), label (arbitrary text), sakura (falling cherry blossoms).

**Known limitations:**  Hyprland fast-replug (~5–10s unplug+replug) sometimes panics at `eglSwapBuffers` (lock survives, recovery is TTY-kill).

## Architecture

Veiland-core is a Wayland client that owns the lock surface, handles PAM, and composites the final image. It spawns plugin processes as children. Each plugin connects over a Unix socket, allocates GPU buffers via GBM, renders into them with its own OpenGL context, and sends the buffer file descriptors to the core via `SCM_RIGHTS`. The core imports the fds as `EGLImage` textures and composites them onto the lock surface. No pixel data crosses CPU memory.

All security-critical operations (input handling, password buffer, PAM, unlock decision) run in the trusted core process. Plugins are untrusted and sandboxed by process boundaries — the kernel enforces both crash isolation and memory isolation for free.

## Installing

### NixOS (flake module)

Veiland ships a flake with a NixOS module. Add veiland as an input and
import the module:

```nix
# flake.nix
{
  inputs.veiland.url = "github:sylflo/veiland";
}
```

```nix
# configuration.nix (with `inputs` in scope, e.g. via specialArgs)
{
  imports = [ inputs.veiland.nixosModules.default ];
  services.veiland.enable = true;
}
```

`services.veiland.enable = true` installs `veiland-core` and the
reference plugins and registers the `veiland` PAM service for you — no
manual `/etc/pam.d/veiland` needed. All that's left is to write a config
(see [Configuration](#configuration)) and bind a key or idle daemon to
`veiland-core`.

To try it without installing anything:

```sh
nix run github:sylflo/veiland
```

(Run outside NixOS, or before adding the module, this still needs the
PAM service — see [PAM setup](#pam-setup).)

### From source

Linux only. Requires `pkg-config`, Mesa (libgbm, libEGL, libGLESv2),
libdrm, libpam, and a Wayland compositor implementing
`ext-session-lock-v1`.

```sh
cargo build --release
```

The flake's dev shell provides every build dependency:

```sh
nix develop      # drops you into a shell with the full toolchain
```

A source build does **not** set up PAM — you must create
`/etc/pam.d/veiland` yourself. See [PAM setup](#pam-setup).

## PAM setup

Veiland authenticates against the PAM service named `veiland`, so
`/etc/pam.d/veiland` must exist. Veiland only performs the `auth` and
`account` phases (verify the password, check the account is valid) — it
does not open a session, so the config is minimal.

On **NixOS**, the [flake module](#nixos-flake-module) handles this. If
you install the package some other way, add:

```nix
security.pam.services.veiland = {};
```

On **other distributions**, create `/etc/pam.d/veiland` referencing the
system auth stack. Most distributions (Arch, Fedora, openSUSE):

```
auth     include system-auth
account  include system-auth
```

Debian/Ubuntu use `common-auth` / `common-account` instead:

```
auth     include common-auth
account  include common-account
```

This inherits whatever policy the system already uses (fingerprint
readers, hardware tokens, etc.) and stays correct as that policy changes.
It is the same approach swaylock and hyprlock use.

## Configuration

See `docs/config.md` for the full config reference. A minimal config:

```toml
[password]
position = "center"

[[plugin]]
name = "wallpaper"
binary = "/path/to/veiland-wallpaper"
z_index = 0
[plugin.config]
path = "/path/to/wallpaper.jpg"

[[plugin]]
name = "clock"
binary = "/path/to/veiland-clock"
z_index = 1
```

Each plugin's `binary` is an absolute path. When installed via the
NixOS module the plugins are on `PATH`, so resolve the store path with
`readlink -f "$(which veiland-clock)"` (or point `binary` at the plugin
directly, e.g. `${pkgs.veiland}/bin/veiland-clock` if you generate the
config in Nix).

Example configs (including a Shinkai-mockup scene) are in `docs/examples/`.

## Plugin development

Plugins are standalone programs that speak the veiland protocol over a Unix socket. They can be written in any language; the reference SDK is `veiland-plugin` (Rust).

A minimal plugin:

```rust
use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer};

fn main() -> anyhow::Result<()> {
    let mut conn = Connection::connect("my-plugin", env!("CARGO_PKG_VERSION"))?;
    let cfg = match conn.wait_for_configure()? {
        Some(c) => c,
        None => return Ok(()),
    };
    // allocate a DMA-BUF at the configured region size, set up GL ...
    let mut pacer = FramePacer::self_paced();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                // render, then:
                conn.send_buffer(&buf_msg, dmabuf_fd, fence)?;
                pacer.submitted();
            }
            Frame::Reconfigure(c) => { /* update scale/size */ }
            Frame::Shutdown => return Ok(()),
        }
    }
}
```

See `docs/plugin-api.md` for the full API reference, including how to load image assets and write procedural shader plugins.

## Design principles

1. **Process isolation is non-negotiable.** A crashing or malicious plugin must not compromise the locker.
2. **OpenGL only.** Vulkan's complexity buys nothing for compositing a handful of textured quads.
3. **The plugin API is a one-way door.** Versioning and capability negotiation are built in from day one.

## Compatibility

Targets any compositor implementing `ext-session-lock-v1`. Tested primarily on Hyprland and Sway.

Other compositors implementing the protocol (KDE Plasma, niri, Wayfire, river, and other wlroots-based compositors) should work but are not regularly tested. GNOME's support for `ext-session-lock-v1` has historically been partial — treat it as untested.

## Non-goals

- X11 support. Wayland-only by design.
- Cross-platform. Linux-only — DMA-BUF and GBM are Linux-specific.
- Vulkan plugins.
- Hot-reloading plugins without restart.
- A built-in plugin store.

## References

The Linux GPU stack (GBM, EGL, dmabuf import) is sparsely documented. Useful starting points:

- [`swaylock-plugin`](https://github.com/mstoeckl/swaylock-plugin) — closest project in spirit; demonstrates `ext-session-lock-v1` lifecycle and delegating background rendering to another process.
- [`kmscube`](https://gitlab.freedesktop.org/mesa/kmscube) — canonical GBM + EGL example.
- [`wlroots`](https://gitlab.freedesktop.org/wlroots/wlroots) — production-quality GBM + EGL + dmabuf import. See `render/gles2/` and `render/allocator/gbm.c`.
- [`EGL_EXT_image_dma_buf_import`](https://registry.khronos.org/EGL/extensions/EXT/EGL_EXT_image_dma_buf_import.txt) — the load-bearing EGL extension.
- [`ext-session-lock-v1`](https://wayland.app/protocols/ext-session-lock-v1) — the Wayland protocol veiland uses to lock the screen.

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).

Plugins communicate with the core over a Unix socket, so plugin authors are free to license their plugins under any terms.

## Naming

"Veiland" is from "veil" — something that obscures what's behind it — and the resemblance to "Wayland" is deliberate. Pronounced "veil-land." Unaffiliated with the Wayland project.
