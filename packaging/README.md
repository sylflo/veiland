<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Packaging

Native distro packages for veiland. Nix users are served by the flake
(`flake.nix`) and NixOS module (`nix/module.nix`); this directory is
everything else.

- `arch/` — AUR `PKGBUILD`
- `debian/` — `.deb` (built on and for Debian 13 stable; also installs on
  Ubuntu 24.04+, see below)
- `rpm/` — `.spec` (Fedora/RHEL)
- `pam/` — the `/etc/pam.d/veiland` files each package installs
  (`veiland.system-auth` for Arch/Fedora, `veiland.common` for Debian)
- `veiland.example.toml` — installed to `/usr/share/veiland/`
  (as `config.example.toml`), alongside the ready-made scenes from
  `docs/examples/` under `/usr/share/veiland/examples/` (minus the
  hotplug repro config, and with asset paths rewritten from their
  repo-relative form to the installed locations)

All packages build the **real set** from source
(`veiland-core` + wallpaper, clock, particles, vignette, label,
sakura, snow, rain, embers, fireflies, gradient, parallax, blobs,
raymarcher)
into `/usr/bin`, and bundle `/etc/pam.d/veiland` so PAM works on
install. The stress test plugin is not packaged.

Packages are built and validated in CI (`.github/workflows/packages.yml`)
inside each distro's container, then install-checked in a pristine
container (dependency resolution, file layout, and `ldd` on every
installed binary). The install check is deliberately a *separate*
container from the build: only there must the package's declared
dependencies resolve against the distro's own repos, which is what
catches a missing or unsatisfiable one.

The Debian and Fedora containers are pinned to a release (`debian:13`,
`fedora:44`) rather than a rolling tag. This matters: `debian:testing`
was trixie when first used, but trixie became Debian 13 stable and the
tag rolled on to the next release — so the package was built on, and
install-checked on, a Debian nobody runs. That circular test stayed
green while the shipped `.deb` could not install on real Debian stable.
`archlinux:latest` stays rolling on purpose: Arch users genuinely do run
latest.

**Ubuntu** installs the Debian `.deb` — there is no separate Ubuntu
build, because the package carries nothing distro-specific. CI
install-checks that same artifact on Ubuntu 24.04 and current.

Ubuntu 22.04 is **not** supported: the `.deb` is built on Debian 13, so
`dpkg-shlibdeps` stamps `libc6 (>= 2.39)` from that glibc. 24.04 ships
exactly 2.39 and satisfies it; 22.04 ships 2.35 and cannot. Supporting
it would mean building a second `.deb` on an older host, which we do not
do.

## Building the `.rpm` from a checkout (Fedora)

`Source0` points at the GitHub release tarball, so for a tagged
release `spectool -g -R packaging/rpm/veiland.spec` downloads it into
`SOURCES/` for you. To build unreleased work instead, stage a tarball
of your checkout under the same name (rpmbuild resolves `Source0` by
basename):

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