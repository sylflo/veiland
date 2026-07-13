<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Packaging

Native distro packages for veiland. Nix users are served by the flake
(`flake.nix`) and NixOS module (`nix/module.nix`); this directory is
everything else.

- `arch/` — AUR `PKGBUILD`
- `debian/` — `.deb` (built on and for Debian 13 stable)
- `rpm/` — `.spec` (Fedora/RHEL)
- `pam/` — the `/etc/pam.d/veiland` files each package installs
  (`veiland.system-auth` for Arch/Fedora, `veiland.common` for Debian)
- `veiland.example.toml` — installed to `/usr/share/veiland/`

All packages build the **real set** from source
(`veiland-core` + wallpaper, clock, particles, vignette, label,
sakura, snow, rain, embers, fireflies, gradient, parallax, blobs,
raymarcher)
into `/usr/bin`, and bundle `/etc/pam.d/veiland` so PAM works on
install. The stress test plugin is not packaged.

Packages are built and validated in CI (`.github/workflows/packages.yml`)
inside each distro's container, then install-checked in a pristine
container of the same distro (dependency resolution, file layout, and
`ldd` on every installed binary).

## Building the `.rpm` from a checkout (Fedora)

The spec expects a tarball named `veiland-<version>.tar.gz` whose top
directory is `veiland-<version>` (once releases are tagged, `Source0`
will point at the GitHub release tarball instead — see the
`TODO(release)` note in the spec). Until then, stage it from your
checkout:

```sh
# Build tooling + the -devel headers the -sys crates link against.
sudo dnf install rpm-build rpmdevtools git rust cargo pkgconf-pkg-config \
  pam-devel mesa-libGL-devel mesa-libEGL-devel mesa-libgbm-devel \
  libdrm-devel wayland-devel libxkbcommon-devel

rpmdev-setuptree
ver=$(rpmspec -q --qf '%{version}\n' packaging/rpm/veiland.spec | head -1)
git archive --format=tar --prefix="veiland-$ver/" HEAD \
  | gzip > ~/rpmbuild/SOURCES/veiland-$ver.tar.gz
rpmbuild -ba packaging/rpm/veiland.spec
sudo dnf install ~/rpmbuild/RPMS/*/veiland-$ver-*.rpm
```

`rpmbuild` runs the full test suite in `%check`. The built rpms (main
plus `-debuginfo`/`-debugsource`) land under `~/rpmbuild/RPMS/<arch>/`;
the install line above picks up the main package only.