+++
title = "Getting started"
description = "Install veiland, copy a scene, lock."
weight = 1
template = "docs-page.html"

[extra]
group = "guide"
+++

Veiland locks your Wayland session and hands the look of the lock screen to
plugins: small programs that render into GPU buffers and hand them back over a
socket. This page gets you from nothing to a locked screen.

## Install

### Arch Linux

From the AUR. The package installs the `veiland` binary and every reference
plugin, and registers the PAM service. Nothing manual.

```sh
yay -S veiland    # or: paru -S veiland
```

### NixOS

Flake module: binary, plugins, and PAM service in two lines of config.

```nix
# flake.nix
inputs.veiland.url = "github:sylflo/veiland";

# configuration.nix (with `inputs` in scope, e.g. via specialArgs)
imports = [ inputs.veiland.nixosModules.default ];
services.veiland.enable = true;
```

Just trying it out? `nix run github:sylflo/veiland` (this still needs the PAM
service; see [PAM setup](#pam-setup) below).

### Debian

Download the `.deb` from the
[latest release](https://github.com/sylflo/veiland/releases), built on and for
Debian 13. PAM config is bundled.

```sh
sudo apt install ./veiland_*_amd64.deb
```

### Fedora / RHEL

Download the `.rpm` from the
[latest release](https://github.com/sylflo/veiland/releases). PAM config is
bundled.

```sh
sudo dnf install ./veiland-*.x86_64.rpm
```

### From source

Linux only. Needs `pkg-config`, Mesa (libgbm, libEGL, libGLESv2), libdrm,
libpam, and a compositor implementing `ext-session-lock-v1`.

```sh
cargo build --release
```

A source build does not set up PAM; see [PAM setup](#pam-setup).

## First lock

With no config file at all, veiland renders the built-in raymarched tunnel with
a styled password pill; the scene is compiled into the binary, so a fresh
install locks with something to look at.

```sh
veiland
```

Bind that to a key, or to an idle daemon (`hypridle`, `swayidle`).

## A scene in three lines

Every gallery scene installs ready to use. Asset paths in the installed copies
already point at the installed files, so no editing is needed:

```sh
mkdir -p ~/.config/veiland
cp /usr/share/veiland/examples/sakura.toml ~/.config/veiland/config.toml
veiland
```

From a source checkout, copy the scene and its assets from the repo instead,
then set the wallpaper `path` to an absolute path (no `~`):

```sh
mkdir -p ~/.config/veiland
cp docs/examples/sakura.toml            ~/.config/veiland/config.toml
cp docs/examples/assets/sakura-dusk.jpg ~/.config/veiland/
# then edit the wallpaper path in config.toml to
# /home/YOU/.config/veiland/sakura-dusk.jpg
```

A wrong wallpaper path is harmless: the rest of the scene and the password
pill still render over black, and veiland logs the bad path.

Next step: [Configuration](@/docs/configuration.md) for the config schema, and
the [plugin reference](@/docs/plugins/_index.md) for every property each
plugin accepts.

## PAM setup

Veiland authenticates against the PAM service named `veiland`, so
`/etc/pam.d/veiland` must exist. Veiland only performs the `auth` and
`account` phases; it does not open a session, so the config is minimal.

The distro packages above bundle this file. On NixOS the flake module handles
it; installing the package some other way needs:

```nix
security.pam.services.veiland = {};
```

For a source install on most distributions (Arch, Fedora, openSUSE), create
`/etc/pam.d/veiland` with:

```
auth     include system-auth
account  include system-auth
```

Debian and Ubuntu use `common-auth` / `common-account` instead:

```
auth     include common-auth
account  include common-account
```

This inherits the system's password policy and stays correct as that policy
changes. Any interactive lines the include pulls in (fingerprint, hardware
tokens) are inert for veiland: it does password authentication only.

## Compatibility

Veiland targets any compositor implementing `ext-session-lock-v1`. Tested
primarily on Hyprland and Sway; other wlroots-based compositors (niri,
Wayfire, river) and KDE Plasma should work but are not regularly tested.
GNOME's support for the protocol has historically been partial, so treat it
as untested.

Whatever happens to veiland itself, the compositor enforces the lock: under
`ext-session-lock-v1`, a crashed lock client leaves the session locked, never
exposed.
