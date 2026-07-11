# veiland

**A GPU lock screen for Wayland that you compose from layers, each one a separate program.**

[![License: GPL v3](https://img.shields.io/badge/License-GPLv3-blue.svg)](LICENSE)
[![CI](https://github.com/sylflo/veiland/actions/workflows/ci.yml/badge.svg)](https://github.com/sylflo/veiland/actions/workflows/ci.yml)

<!--
  hero.gif : THE shot. Raymarcher scene as a full lock screen with the
  password pill visible, so it reads as a locker, not a demo. Config:
  docs/examples/raymarcher.toml (has a [password] pill). GIF (not video)
  so it autoplays and loops natively on github.com; kept to 900px/10fps
  to hold the size down, since a full-screen shader is worst-case for GIF.
-->
![veiland: a raymarched tunnel behind the lock pill](docs/assets/readme/hero.gif)

Veiland (from "veil", something that obscures what's behind it) locks your
Wayland session and hands the *look* of the lock screen to plugins: small
programs that render into GPU buffers and hand them back over a socket.
Stack them, reorder them, swap them. The locker itself never has to change.
Pronounced "veil-land"; the resemblance to "Wayland" is deliberate, and
veiland is unaffiliated with the Wayland project.

## Why veiland

- **GPU-accelerated *and* extensible.** The whole lock screen is GPU-rendered,
  and you extend it with real plugins, not by shelling out to scripts. You
  don't have to fork veiland or send a PR to add one.
- **Plugins are isolated processes.** Every layer (wallpaper, particles,
  glow, clock) is a separate program, not code loaded into the locker. If
  one crashes or misbehaves, that layer disappears and the rest keeps
  running. By protocol construction no plugin ever receives a keystroke or
  the password, and none can trigger an unlock, that stays in the core. What
  the process boundary does and does not buy against hostile same-UID code is
  spelled out in the [Security model](#security-model).
- **Write your own on your own machine.** A plugin is just a program that
  talks to the core over a socket, so you drop one next to your config and
  point veiland at it. No rebuild of the locker, no upstream approval. The
  reference SDK is Rust today, but the wire format is documented
  ([`docs/protocol.md`](docs/protocol.md)) and not tied to Rust.
- **Stack layers into a scene.** Order plugins by `z_index` like layers in
  an image editor, target specific monitors, and animate them on the GPU at
  your refresh rate. Fourteen plugins ship in the box; see the
  [gallery](#gallery).

## Why another locker?

Short and personal, because veiland scratches a specific itch.

I wanted particle animations on my lock screen. The lockers I tried
are good software, and the comparison below is about architecture, not
quality:

- [swaylock](https://github.com/swaywm/swaylock) is the minimal,
  battle-tested default: a static color or image, on purpose. Wanting
  more has historically meant forking it (swaylock-effects).
- [gtklock](https://github.com/jovanlanik/gtklock) has a real module
  system, but modules are `.so` files loaded into the locker's own
  process, and GTK3 rendering is CPU-side — full-screen animation at
  refresh rate isn't what it's built for.
- [hyprlock](https://github.com/hyprwm/hyprlock) is what I actually
  ran: GPU-accelerated and animated. But its widgets are compiled into
  the binary, so every feature it doesn't have is an upstream PR. I
  wanted *n* plugins, and *n* features shouldn't cost *n* PRs — mine
  or anyone else's.

Veiland is that itch built out: the extension mechanism *is* the
architecture. Plugins are separate, GPU-accelerated programs, so
animation is first-class and adding a layer needs nobody's approval,
including mine. The process isolation started as the way to make that
freedom safe and grew into the [security model](#security-model) — the
freedom came first, the hardening followed.

If this misrepresents one of these projects, open an issue; that is
never the intent.

## Gallery

Every scene below is a ready-made config in [`docs/examples/`](docs/examples).
Copy one, set your wallpaper path, and lock.

| | | |
|---|---|---|
| <!-- gallery-shinkai.gif : flagship lived-with scene, wallpaper + vignette + particles + sakura + clock + labels. 5-8s loop, ~600px wide. Config: docs/examples/shinkai.toml --> ![Shinkai scene](docs/assets/readme/gallery-shinkai.gif)<br>**[shinkai](docs/examples/shinkai.toml)**<br>two monitors, a different scene on each | <!-- gallery-sakura.gif : falling cherry blossoms over a dusk sky. 5-8s loop, ~600px. Config: docs/examples/sakura.toml --> ![Sakura scene](docs/assets/readme/gallery-sakura.gif)<br>**[sakura](docs/examples/sakura.toml)**<br>falling petals | <!-- gallery-snow.gif : procedural six-fold ice crystals over a dark wallpaper. 5-8s loop, ~600px. Config: docs/examples/snow.toml --> ![Snow scene](docs/assets/readme/gallery-snow.gif)<br>**[snow](docs/examples/snow.toml)**<br>dendritic crystals |
| <!-- gallery-rain.gif : wind-slanted motion-blur rain over a moody wallpaper. 5-8s loop, ~600px. Config: docs/examples/rain.toml --> ![Rain scene](docs/assets/readme/gallery-rain.gif)<br>**[rain](docs/examples/rain.toml)**<br>slanted streaks | <!-- gallery-embers.gif : rising sparks + bottom glow over a dark wallpaper. 5-8s loop, ~600px. Config: docs/examples/embers.toml --> ![Embers scene](docs/assets/readme/gallery-embers.gif)<br>**[embers](docs/examples/embers.toml)**<br>rising sparks | <!-- gallery-fireflies.gif : softly glowing wandering lights over a dark wallpaper. 5-8s loop, ~600px. Config: docs/examples/fireflies.toml --> ![Fireflies scene](docs/assets/readme/gallery-fireflies.gif)<br>**[fireflies](docs/examples/fireflies.toml)**<br>wandering glow |
| <!-- gallery-gradient.gif : slow looping color ramp. 5-8s loop, ~600px. Config: docs/examples/gradient.toml --> ![Gradient scene](docs/assets/readme/gallery-gradient.gif)<br>**[gradient](docs/examples/gradient.toml)**<br>flowing color ramp | <!-- gallery-blobs.gif : drifting metaball / lava-lamp field. 5-8s loop, ~600px. Config: docs/examples/blobs.toml --> ![Blobs scene](docs/assets/readme/gallery-blobs.gif)<br>**[blobs](docs/examples/blobs.toml)**<br>lava-lamp metaballs | <!-- gallery-parallax.gif : three bokeh layers drifting over a gradient. 5-8s loop, ~600px. Config: docs/examples/parallax.toml --> ![Parallax scene](docs/assets/readme/gallery-parallax.gif)<br>**[parallax](docs/examples/parallax.toml)**<br>layered bokeh depth |

**The full lineup:** wallpaper, clock, label, vignette, particles, sakura,
snow, rain, embers, fireflies, gradient, blobs, parallax, raymarcher.

**Planned** (roughly in order, not promises):

- **now-playing** — current track, artist, and album art from your media
  player.
- **status** — glanceable battery, keyboard layout, and caps-lock state.
- **weather** — current conditions and temperature for your location.
- **avatar** — profile picture and username, shown on the lock screen.

Writing one of those plugins is the same job as the shipped ones; see
[Plugin development](#plugin-development).

## Quick start

**1. Install** (pick your distro):

<details>
<summary><strong>NixOS</strong> (flake module)</summary>

Add veiland as an input and import the module:

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
reference plugins and registers the `veiland` PAM service for you. No manual
`/etc/pam.d/veiland` needed.

To try it without installing anything:

```sh
nix run github:sylflo/veiland
```

(Run outside NixOS, or before adding the module, this still needs the PAM
service. See [PAM setup](#pam-setup).)
</details>

<details>
<summary><strong>Arch Linux</strong> (AUR)</summary>

Install from the AUR (available from the `v0.1.0` release onward):

```sh
yay -S veiland        # or: paru -S veiland
```

The package installs the `veiland` binary and the reference plugins into
`/usr/bin` and registers the `veiland` PAM service. No manual
`/etc/pam.d/veiland` needed.

To build it yourself from a checkout of this repo instead: the shipped
`PKGBUILD` has an empty `source=()` (it's built from the working tree,
not a release tarball yet), so stage the tree where `build()` expects it
and tell `makepkg` not to re-extract:

```sh
workdir=$(mktemp -d)
cp packaging/arch/PKGBUILD "$workdir/PKGBUILD"
git archive --format=tar HEAD | tar -x -C "$workdir" --one-top-level=src/veiland
cd "$workdir"
makepkg -si --noextract --skipinteg
```

(Once a tagged release tarball is wired into `source=()`, this collapses
back to a plain `cd packaging/arch && makepkg -si`.)
</details>

<details>
<summary><strong>Debian / Ubuntu</strong> (.deb)</summary>

Download the `.deb` from the [latest release][releases] and install it
(available from `v0.1.0` onward). `apt` pulls in the runtime libraries:

```sh
sudo apt install ./veiland_0.1.1-1_amd64.deb
```

The package installs the binaries into `/usr/bin` and bundles
`/etc/pam.d/veiland`, so PAM works out of the box. Built for Debian 13
(trixie) and newer; it needs libjpeg-turbo 3.x, so distributions still
on the 2.x series (including Ubuntu 24.04) are not supported.

To build the `.deb` yourself from this repo:

```sh
cp -r packaging/debian debian
dpkg-buildpackage -b -us -uc
sudo apt install ../veiland_*.deb
```
</details>

<details>
<summary><strong>Fedora / RHEL</strong> (.rpm)</summary>

Install the `.rpm` straight from the [latest release][releases]
(available from `v0.1.0` onward); `dnf` resolves the runtime deps:

```sh
sudo dnf install https://github.com/sylflo/veiland/releases/latest/download/veiland-0.1.1-1.x86_64.rpm
```

The package installs the binaries into `/usr/bin` and bundles
`/etc/pam.d/veiland`. To build the `.rpm` yourself, see
`packaging/README.md`.
</details>

<details>
<summary><strong>From source</strong></summary>

Linux only. Requires `pkg-config`, Mesa (libgbm, libEGL, libGLESv2),
libdrm, libpam, and a Wayland compositor implementing `ext-session-lock-v1`.

```sh
cargo build --release
```

The flake's dev shell provides every build dependency:

```sh
nix develop      # drops you into a shell with the full toolchain
```

A source build does **not** set up PAM. You must create
`/etc/pam.d/veiland` yourself. See [PAM setup](#pam-setup).
</details>

[releases]: https://github.com/sylflo/veiland/releases

**2. Grab a scene.** The `sakura` example needs only its bundled wallpaper:

```sh
mkdir -p ~/.config/veiland
cp docs/examples/sakura.toml            ~/.config/veiland/config.toml
cp docs/examples/assets/sakura-dusk.jpg ~/.config/veiland/
```

Then edit `~/.config/veiland/config.toml` and set the wallpaper `path` to an
**absolute** path (no `~`):

```toml
[[plugin]]
name = "wallpaper"
binary = "veiland-wallpaper"
z_index = -100
[plugin.config]
path = "/home/you/.config/veiland/sakura-dusk.jpg"
```

**3. Lock:**

```sh
veiland
```

Bind that to a key or an idle daemon (`hypridle`, `swayidle`). If the
wallpaper path is wrong it's harmless: the petals, clock, and password pill
still render over black, and veiland logs the bad path. Every scene in the
[gallery](#gallery) installs the same way.

---

## Architecture

Veiland-core owns the lock surface, handles keyboard input and PAM, and
composites the final image. It spawns each plugin as a child process.
Plugins render into their own GPU buffers and hand back a file descriptor,
which the core samples as a texture. No pixel data crosses CPU memory, and
no plugin ever sees a keystroke.

```
                      +--------------------------------------+
                      |  veiland-core (trusted)              |
   keyboard  ------>  |  input, password buffer, PAM,        |
                      |  unlock decision, GL compositing     |
                      +------------------+-------------------+
                                         |  Unix socket (SEQPACKET)
             +---------------------------+---------------------------+
             |                           |                           |
      +------+------+             +------+------+             +------+------+
      |  wallpaper  |             |   sakura    |             |    clock    |  ...
      | (untrusted) |             | (untrusted) |             | (untrusted) |
      +------+------+             +------+------+             +------+------+
             |  dmabuf fd                |  dmabuf fd                |  dmabuf fd
             +---------------> (SCM_RIGHTS, zero-copy) <-------------+
```

Each plugin connects over the socket, allocates GPU buffers via GBM, renders
into them with its own EGL/OpenGL context, and sends the buffer file
descriptors to the core via `SCM_RIGHTS`. The core imports the fds as
`EGLImage` textures and composites them. All security-critical operations
(input handling, the password buffer, PAM, the unlock decision) run in the
trusted core; plugins are untrusted and isolated by the process boundary, so
the kernel gives crash isolation for free. Memory isolation against hostile
same-UID code needs more than the boundary alone, see the
[Security model](#security-model).

The locker is in production and works end to end: an `ext-session-lock-v1`
lock surface, PAM authentication (run on a worker thread so a wrong password
never freezes the animation), a configurable password indicator,
process-isolated GPU plugins over DMA-BUF, multi-monitor support with
per-plugin output targeting, and HiDPI-aware text rendering via
`veiland-text` (cosmic-text backend).

Full write-up, trust boundaries, module map, and the wire format:
[`docs/architecture.md`](docs/architecture.md) and
[`docs/protocol.md`](docs/protocol.md).

## Security model

Two boundaries do the work: the compositor enforces the lock, and the process boundary contains the plugins.

**The session fails closed.** Under [`ext-session-lock-v1`](https://wayland.app/protocols/ext-session-lock-v1), the compositor, not veiland, enforces the lock. The spec is explicit: *"if the client dies while the session is locked, the compositor must not unlock the session in response."* If veiland crashes, the session stays locked and no window content is ever revealed; the worst case is a locked screen you recover from a TTY. That guarantee comes from the compositor, not from veiland being bug-free.

**Plugins sit outside the trust boundary.** The compositor unlocks whenever the lock client asks it to, so what matters is what runs inside that client. In veiland, plugins don't: they are separate processes, not loadable modules, and the protocol between them and the core is deliberately narrow. This is what the process boundary buys, by construction:

- **No unlock path.** No plugin-to-core message maps to "unlock". The API surface is absent, not filtered.
- **No keystrokes.** No protocol message carries keyboard input in either direction; plugins never receive keystrokes at all. The password never appears in any buffer or message a plugin is handed.
- **No code execution in the core.** A plugin hands over GPU buffers; bytes in a buffer become pixel values through a GPU sampler, never instructions. Every field a plugin sends is validated before it reaches EGL or the kernel; implausible sizes, strides, and modifiers are refused.
- **No cross-plugin reads.** Each plugin owns its own dmabufs; the core composites but never redistributes one plugin's buffer to another.

**Where the boundary stops — read this before installing a third-party plugin.** Plugins run as *your* user, the same UID as the core. So the process boundary is not, by itself, a wall against hostile same-user code:

- On a system with `ptrace_scope=0`, a same-UID process can `PTRACE_ATTACH` the core or read `/proc/<pid>/mem`. `mlock` prevents the password buffer from being *swapped to disk*; it does nothing against being *read* by a process allowed to inspect the core. Veiland calls `prctl(PR_SET_DUMPABLE, 0)` at startup, which denies same-UID ptrace/proc-mem and suppresses core dumps of the buffer — set `VEILAND_ALLOW_DUMP=1` to opt out for debugging. This raises the bar significantly, but it is defense-in-depth: root, a kernel bug, or a debugger started with privileges still wins.

  So we draw the line honestly:

  - **First-party plugins** (the ones in this repo) are code we wrote and review. We vouch for them the way we vouch for the core.
  - **Third-party plugins** are same-user code you chose to install, like any other program you run as your user. Process isolation plus a non-dumpable core gives you real containment against *bugs and accidents* and raises the cost of *deliberate* snooping — but against a genuinely hostile third-party plugin we reduce risk, we do not eliminate it. Zero risk there is not something we can guarantee short of a full sandbox (seccomp/landlock), which veiland does not yet ship. Install third-party plugins with the same care you'd give any untrusted binary.

**What a hostile plugin can still try inside the boundary, and how it's bounded.** Malformed messages or resource exhaustion get its socket closed and a fallback drawn for its region; in-flight buffers and dimensions are capped, and the locker never blocks on a dead plugin. A plugin could draw a fake "unlocked" desktop inside its own region, which is why the password UI is painted by the core on top of all plugin output: a plugin can draw beneath it, never over it.

*(One scrub-hygiene footnote, unrelated to plugins: handing the password to PAM requires copying it into a `CString`. The PAM call runs on a dedicated worker thread so a wrong password doesn't freeze the animation; the core scrubs its own copy on that thread when the call returns, but the per-prompt copy PAM receives and libpam's own internal copy are outside the core's control. The copy lives in the same process either way, so it is covered by the same `PR_SET_DUMPABLE, 0` protection as the rest of the address space.)*

## Configuration

Veiland looks for its config at `~/.config/veiland/config.toml` (or
`$XDG_CONFIG_HOME/veiland/config.toml`). See `docs/config.md` for the full
reference. A minimal config:

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

Each plugin's `binary` is a **bare name**: veiland resolves it beside the
installed `veiland` binary (then on `$PATH`), so the same config works
whether your distro installs to `/usr/bin` or, on NixOS, a
`/nix/store/.../bin` directory. To run a specific build instead, give a path
containing a `/` (e.g. `target/debug/veiland-clock`) and it's used verbatim.
Plugin **asset** paths like the wallpaper's `path`, though, are read
directly with no `~` expansion, so always give those an absolute path.

Plugins layer by `z_index` (low to high), and a `monitors = ["DP-1"]` key
targets specific outputs. That's how a scene can run one wallpaper on one
monitor and a different one on another. The example configs in
[`docs/examples/`](docs/examples) are the fastest way to see the full config
surface; `shinkai.toml` composes ten plugin instances across two monitors.

## Plugin development

Plugins are standalone programs that speak the veiland protocol over a Unix
socket. The reference SDK is `veiland-plugin` (Rust), but the wire format is
documented in `docs/protocol.md` and isn't tied to Rust.

A minimal plugin:

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
                // submit_frame picks the sync model (fence fd vs glFinish)
                // for you based on what the host advertised.
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

See `docs/plugin-api.md` for the full API reference, including how to load
image assets and write procedural shader plugins.

**Writing plugins with an AI assistant.** If you use Claude Code (or another
coding assistant) to build a plugin,
[`docs/plugin-authoring-claude.md`](docs/plugin-authoring-claude.md) is
purpose-built context for exactly that: drop it into your plugin project as
`CLAUDE.md`, or point your assistant at it. It carries the verified SDK
signatures, the canonical plugin shape, and the non-negotiable rules (sync-path
choice, `pacer.submitted()`, premultiplied alpha, no-panic-on-host-message) an
assistant needs to get a plugin right on the first try instead of guessing at
the API.

## Compatibility

Targets any compositor implementing `ext-session-lock-v1`. Tested primarily
on Hyprland and Sway.

Other compositors implementing the protocol (KDE Plasma, niri, Wayfire,
river, and other wlroots-based compositors) should work but are not
regularly tested. GNOME's support for `ext-session-lock-v1` has historically
been partial, so treat it as untested.

### Reliability

The failure mode that matters most for a locker is ending up at an exposed
desktop, or a screen you cannot type into, after the machine wakes. On
**Hyprland** (NVIDIA, multi-monitor) the lock survives and stays usable
across:

- suspend / resume,
- display power-off / on (DPMS, e.g. an idle daemon blanking the screen),
- monitor unplug / replug, including unplugging every monitor at once and
  hot-plugging a display in while locked.

Monitor unplug / replug is also tested on **Sway**; the full suspend/DPMS
sweep there is still pending. Unplugging or replugging a single monitor
recovers immediately. The one rough edge is the extreme case of unplugging
*every* monitor at once and plugging back in: the session stays locked
throughout, but the scene takes a moment to repaint (every output and its
plugins are rebuilt from scratch) rather than coming back instantly. And
regardless of the app, `ext-session-lock-v1` means the compositor keeps the
session locked even if veiland itself crashes (see
[Security model](#security-model)).

## Design principles

1. **Process isolation is non-negotiable.** A crashing or malicious plugin
   must not compromise the locker.
2. **OpenGL only.** Vulkan's complexity buys nothing for compositing a
   handful of textured quads.
3. **The plugin API is a one-way door.** Versioning and capability
   negotiation are built in from day one.

## Non-goals

- X11 support. Wayland-only by design.
- Cross-platform. Linux-only, because DMA-BUF and GBM are Linux-specific.
- Vulkan plugins.
- Hot-reloading plugins without restart.
- A built-in plugin store.

Veiland does password authentication only for now: it feeds the typed
password to PAM. Fingerprint and hardware-token support, and pointer-driven
widgets, aren't in this release; they're plausible additions if there's
demand. There is no video playback.

## PAM setup

Veiland authenticates against the PAM service named `veiland`, so
`/etc/pam.d/veiland` must exist. Veiland only performs the `auth` and
`account` phases (verify the password, check the account is valid); it does
not open a session, so the config is minimal.

The **distro packages** (Arch/Debian/Fedora, above) bundle this file, so if
you installed one of them there is nothing to do here.

On **NixOS**, the [flake module](#quick-start) handles this. If you install
the package some other way, add:

```nix
security.pam.services.veiland = {};
```

For a **source install on other distributions**, create `/etc/pam.d/veiland`
referencing the system auth stack. Most distributions (Arch, Fedora,
openSUSE):

```
auth     include system-auth
account  include system-auth
```

Debian/Ubuntu use `common-auth` / `common-account` instead:

```
auth     include common-auth
account  include common-account
```

This inherits the system's password policy and stays correct as that policy
changes, the standard approach for a screen locker's PAM stack. Any
interactive lines the include pulls in (fingerprint, hardware tokens) are
inert for veiland: it does password authentication only.

## References

The Linux GPU stack (GBM, EGL, dmabuf import) is sparsely documented. Useful
starting points:

- [`kmscube`](https://gitlab.freedesktop.org/mesa/kmscube), canonical GBM + EGL example.
- [`wlroots`](https://gitlab.freedesktop.org/wlroots/wlroots), production-quality GBM + EGL + dmabuf import. See `render/gles2/` and `render/allocator/gbm.c`.
- [`EGL_EXT_image_dma_buf_import`](https://registry.khronos.org/EGL/extensions/EXT/EGL_EXT_image_dma_buf_import.txt), the load-bearing EGL extension.
- [`ext-session-lock-v1`](https://wayland.app/protocols/ext-session-lock-v1), the Wayland protocol veiland uses to lock the screen.

## License

GPL-3.0-or-later. See [`LICENSE`](LICENSE).

Plugins communicate with the core over a Unix socket, so plugin authors are
free to license their plugins under any terms.
</content>
