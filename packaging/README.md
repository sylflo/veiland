<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Packaging

Native distro packages for veiland. Nix users are served by the flake
(`flake.nix`) and NixOS module (`nix/module.nix`); this directory is
everything else.

- `arch/` — AUR `PKGBUILD`
- `debian/` — `.deb` (Debian/Ubuntu)
- `rpm/` — `.spec` (Fedora/RHEL)
- `pam/` — the `/etc/pam.d/veiland` files each package installs
  (`veiland.system-auth` for Arch/Fedora, `veiland.common` for Debian)
- `veiland.example.toml` — installed to `/usr/share/veiland/`

All packages build the **real set** from source
(`veiland-core` + wallpaper, clock, particles, vignette, label, sakura)
into `/usr/bin`, and bundle `/etc/pam.d/veiland` so PAM works on
install. Demo plugins (blue/green/red-box, gradient, stress) are not
packaged.

Packages are built and validated in CI (`.github/workflows/packages.yml`)
inside each distro's container