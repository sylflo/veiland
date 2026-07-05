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

`services.veiland.enable = true` installs the `veiland` binary and the
reference plugins and registers the `veiland` PAM service for you — no
manual `/etc/pam.d/veiland` needed. All that's left is to write a config
(see [Configuration](#configuration)) and bind a key or idle daemon to
`veiland`.

To try it without installing anything:

```sh
nix run github:sylflo/veiland
```

(Run outside NixOS, or before adding the module, this still needs the
PAM service — see [PAM setup](#pam-setup).)

### Arch Linux

Install from the AUR (available from the `v0.1.0` release onward):

```sh
yay -S veiland        # or: paru -S veiland
```

The package installs the `veiland` binary and the reference plugins into
`/usr/bin` and registers the `veiland` PAM service — no manual
`/etc/pam.d/veiland` needed.

To build it yourself from this repo instead:

```sh
cd packaging/arch
makepkg -si
```

### Debian / Ubuntu

Download the `.deb` from the [latest release][releases] and install it
(available from `v0.1.0` onward). `apt` pulls in the runtime libraries:

```sh
sudo apt install ./veiland_0.1.0-1_amd64.deb
```

The package installs the binaries into `/usr/bin` and bundles
`/etc/pam.d/veiland`, so PAM works out of the box. Built for Debian 13
(trixie) and newer, and Ubuntu 24.04 and newer.

To build the `.deb` yourself from this repo:

```sh
cp -r packaging/debian debian
dpkg-buildpackage -b -us -uc
sudo apt install ../veiland_*.deb
```

### Fedora / RHEL

Install the `.rpm` straight from the [latest release][releases]
(available from `v0.1.0` onward); `dnf` resolves the runtime deps:

```sh
sudo dnf install https://github.com/sylflo/veiland/releases/latest/download/veiland-0.1.0-1.x86_64.rpm
```

The package installs the binaries into `/usr/bin` and bundles
`/etc/pam.d/veiland`. To build the `.rpm` yourself, see
`packaging/README.md`.

[releases]: https://github.com/sylflo/veiland/releases

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

The **distro packages** (Arch/Debian/Fedora, above) bundle this file, so
if you installed one of them there is nothing to do here.

On **NixOS**, the [flake module](#nixos-flake-module) handles this. If
you install the package some other way, add:

```nix
security.pam.services.veiland = {};
```

For a **source install on other distributions**, create
`/etc/pam.d/veiland` referencing the system auth stack. Most
distributions (Arch, Fedora, openSUSE):

```
auth     include system-auth
account  include system-auth
```

Debian/Ubuntu use `common-auth` / `common-account` instead:

```
auth     include common-auth
account  include common-account
```

This inherits the system's password policy and stays correct as that
policy changes — the standard approach for a screen locker's PAM stack.
Note that veiland does **password authentication only**: it feeds the
typed password to PAM and cannot drive interactive modules like
fingerprint readers or hardware tokens, so any such lines the include
pulls in are inert for veiland.

## Configuration

Veiland looks for its config at `~/.config/veiland/config.toml` (or
`$XDG_CONFIG_HOME/veiland/config.toml`). See `docs/config.md` for the
full reference. A minimal config:

```toml
[password]
position = "center"

[[plugin]]
name = "wallpaper"
binary = "veiland-wallpaper"
z_index = 0
[plugin.config]
path = "/home/you/Pictures/wallpaper.jpg"

[[plugin]]
name = "clock"
binary = "veiland-clock"
z_index = 1
```

Each plugin's `binary` is a **bare name**: veiland resolves it beside
the installed `veiland` binary (then on `$PATH`), so the same config
works whether your distro installs to `/usr/bin` or, on NixOS, a
`/nix/store/.../bin` directory. To run a specific build instead, give a
path containing a `/` (e.g. `target/debug/veiland-clock`) and it's used
verbatim. Plugin **asset** paths like the wallpaper's `path`, though,
are read directly with no `~` expansion — always give those an absolute
path.

### Example: falling cherry blossoms

`docs/examples/sakura.toml` is a ready-made scene — a dusk-sky
wallpaper, falling cherry-blossom petals, a clock, and a styled password
pill. It uses bare plugin names, so it works on any install without
editing the `binary` lines. The one thing to set is the wallpaper's
absolute path.

```sh
# Copy the config and its wallpaper into place.
mkdir -p ~/.config/veiland
cp docs/examples/sakura.toml            ~/.config/veiland/config.toml
cp docs/examples/assets/sakura-dusk.jpg ~/.config/veiland/
```

Then edit `~/.config/veiland/config.toml` and set the wallpaper `path`
to where you copied the image (a full absolute path — no `~`):

```toml
[[plugin]]
name = "wallpaper"
binary = "veiland-wallpaper"
z_index = -100
[plugin.config]
path = "/home/you/.config/veiland/sakura-dusk.jpg"
```

Lock the screen with `veiland` to see it. If the wallpaper path is
wrong it's harmless — the petals, clock, and password pill still render
over a black background, and veiland logs the bad path.

The bundled `sakura-dusk.jpg` is a photo from
[Unsplash](https://unsplash.com), used under the Unsplash License (free
use, no attribution required). Swap in any PNG or JPEG you like.

Other example configs (including a Shinkai-mockup scene) are in
`docs/examples/`.

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

- [`kmscube`](https://gitlab.freedesktop.org/mesa/kmscube) — canonical GBM + EGL example.
- [`wlroots`](https://gitlab.freedesktop.org/wlroots/wlroots) — production-quality GBM + EGL + dmabuf import. See `render/gles2/` and `render/allocator/gbm.c`.
- [`EGL_EXT_image_dma_buf_import`](https://registry.khronos.org/EGL/extensions/EXT/EGL_EXT_image_dma_buf_import.txt) — the load-bearing EGL extension.
- [`ext-session-lock-v1`](https://wayland.app/protocols/ext-session-lock-v1) — the Wayland protocol veiland uses to lock the screen.

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).

Plugins communicate with the core over a Unix socket, so plugin authors are free to license their plugins under any terms.

## Naming

"Veiland" is from "veil" — something that obscures what's behind it — and the resemblance to "Wayland" is deliberate. Pronounced "veil-land." Unaffiliated with the Wayland project.
