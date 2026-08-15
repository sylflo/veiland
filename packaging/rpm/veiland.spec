# SPDX-License-Identifier: GPL-3.0-or-later
#
# RPM spec for veiland (Fedora/RHEL). Builds the real set (veiland-core
# + reference plugins) from source into /usr/bin and installs the
# veiland PAM service so password authentication works on install.
#
# The stress test plugin is not built or packaged.

Name:           veiland
Version:        0.1.0
Release:        1%{?dist}
Summary:        Wayland screen locker with process-isolated GPU plugins

License:        GPL-3.0-or-later
URL:            https://github.com/sylflo/veiland

# The v%%{version} release tarball (Fedora's github-archive URL form;
# the trailing filename is arbitrary to GitHub and names the download).
# rpmbuild resolves Source0 by basename in SOURCES/, so CI keeps
# staging the checked-out tree there under this exact name — the URL
# is fetched only by humans running `spectool -g`.
Source0:        %{url}/archive/v%{version}/%{name}-%{version}.tar.gz

ExclusiveArch:  x86_64 aarch64

# Build tooling: the Rust toolchain + pkg-config, plus the -devel
# headers the -sys crates link against.
BuildRequires:  cargo
BuildRequires:  rust
BuildRequires:  pkgconf-pkg-config
BuildRequires:  pam-devel
BuildRequires:  mesa-libGL-devel
BuildRequires:  mesa-libEGL-devel
BuildRequires:  mesa-libgbm-devel
BuildRequires:  libdrm-devel
BuildRequires:  wayland-devel
BuildRequires:  libxkbcommon-devel

# Runtime shared libraries are auto-derived by rpm from the binaries'
# ELF NEEDED entries (as sonames, e.g. libwayland-client.so.0), so the
# resolved package names stay correct whatever Fedora calls them. An
# explicit list proved wrong in practice: no binary package is named
# "wayland" (the install check caught the unresolvable Requires). Only
# pam stays explicit, because the bundled /etc/pam.d/veiland includes
# the system-auth stack — a config dependency, not a library one. The
# GPU driver .so is the user's system's job, like on Nix.
Requires:       pam
# The Python widget tier (the veiland-* status/info widgets): the interpreter,
# the gi bindings + drawing libs (pycairo, Pillow), the D-Bus client, and the
# C libraries whose typelibs the widgets load through gi. Unlike the Rust
# binaries above (whose libs rpm auto-derives from ELF NEEDED), a .py script
# has no ELF to scan, so the whole Python stack is declared explicitly.
# gdk-pixbuf2 is pulled in transitively by librsvg2's typelib but listed for
# clarity. On Fedora the typelibs land in the default GI path (no wrapper).
Requires:       python3
Requires:       python3-gobject
Requires:       python3-cairo
Requires:       python3-pillow
Requires:       python3-jeepney
Requires:       librsvg2
Requires:       pango
Requires:       gdk-pixbuf2

%description
Veiland is a Wayland screen locker whose visual layers (wallpaper,
clock, animations, widgets) run as separate, process-isolated plugins
that render on the GPU and share buffers with the locker via DMA-BUF.
The locker itself owns PAM authentication, keyboard input, and the
unlock decision; plugins never see keystrokes or the password.

This package installs veiland-core and the reference plugins
(wallpaper, clock, particles, vignette, label, sakura, snow, rain,
embers, fireflies, gradient, parallax, blobs, raymarcher).

%prep
%autosetup -n %{name}-%{version}

%build
export CARGO_HOME=%{_builddir}/cargo-home
cargo build --release --locked \
  -p veiland-core \
  -p veiland-wallpaper \
  -p veiland-clock \
  -p veiland-particles \
  -p veiland-vignette \
  -p veiland-label \
  -p veiland-sakura \
  -p veiland-snow \
  -p veiland-rain \
  -p veiland-embers \
  -p veiland-fireflies \
  -p veiland-gradient \
  -p veiland-parallax \
  -p veiland-blobs \
  -p veiland-raymarcher

%check
export CARGO_HOME=%{_builddir}/cargo-home
# spawn_true_exits_zero needs /bin/true; present here, so no skip.
# The last three -p flags are the library crates compiled into the
# binaries: `cargo test -p` only runs the named packages' tests, so
# they need naming here or their suites never run. Test-only; they
# produce no installed artifact.
cargo test --release --locked \
  -p veiland-core \
  -p veiland-wallpaper \
  -p veiland-clock \
  -p veiland-particles \
  -p veiland-vignette \
  -p veiland-label \
  -p veiland-sakura \
  -p veiland-snow \
  -p veiland-rain \
  -p veiland-embers \
  -p veiland-fireflies \
  -p veiland-gradient \
  -p veiland-parallax \
  -p veiland-blobs \
  -p veiland-raymarcher \
  -p veiland-protocol \
  -p veiland-plugin \
  -p veiland-text

%install
# Binaries -> /usr/bin. The veiland-core crate builds a binary named
# `veiland` (see its [[bin]] in Cargo.toml); the plugins keep their
# crate names.
install -Dm0755 target/release/veiland "%{buildroot}%{_bindir}/veiland"
for c in veiland-wallpaper veiland-clock veiland-particles \
         veiland-vignette veiland-label veiland-sakura \
         veiland-snow veiland-rain veiland-embers \
         veiland-fireflies veiland-gradient veiland-parallax \
         veiland-blobs veiland-raymarcher; do
  install -Dm0755 "target/release/$c" "%{buildroot}%{_bindir}/$c"
done

# Python widgets. Stash the SDK + companions + renamed widget scripts + icons
# under %{_libdir}/veiland/python, PRESERVING the python/examples/ layout so
# each widget's own sys.path shim (dirname(dirname(__file__))) and ICON_DIR
# (<dir>/icons) resolve unmodified -- no .py edit. Each veiland-<name> in
# %{_bindir} is a tiny exec shim into the stashed script: a shim, NOT a symlink
# (a symlink resolves __file__ back to bindir and breaks the shims), and no env
# wrapper (Fedora's gi loader finds the default GI path). Mirrors the Arch
# PKGBUILD, the deb rules, and the flake, kept in lockstep.
#
# NOTE: %{_libdir} is /usr/lib64 on 64-bit Fedora; the shim hardcodes that same
# macro-expanded path, so bindir shim and stashed script always agree.
install -dm0755 %{buildroot}%{_libdir}/veiland/python/examples
install -m0644 python/veiland_plugin.py python/veiland_svg.py \
  python/veiland_text.py python/veiland_layout.py python/veiland_dbus.py \
  %{buildroot}%{_libdir}/veiland/python/
cp -r python/examples/icons %{buildroot}%{_libdir}/veiland/python/examples/icons
for pair in veiland-now-playing:now_playing.py veiland-weather:weather.py \
            veiland-wifi:wifi.py veiland-ethernet:ethernet.py \
            veiland-bluetooth:bluetooth.py veiland-avatar:avatar.py \
            veiland-markup:markup.py veiland-shape:shape.py \
            veiland-battery:battery_svg.py; do
  name="${pair%%:*}"; src="${pair##*:}"
  install -m0755 "python/examples/$src" \
    "%{buildroot}%{_libdir}/veiland/python/examples/$name"
  printf '#!/bin/sh\nexec %{_libdir}/veiland/python/examples/%s "$@"\n' "$name" \
    > "%{buildroot}%{_bindir}/$name"
  chmod 0755 "%{buildroot}%{_bindir}/$name"
done

# PAM service (Fedora uses the system-auth variant).
install -Dm0644 packaging/pam/veiland.system-auth \
  %{buildroot}%{_sysconfdir}/pam.d/veiland

# Example config the user copies into ~/.config/veiland/.
install -Dm0644 packaging/veiland.example.toml \
  %{buildroot}%{_datadir}/veiland/config.example.toml

# Wallpaper referenced by the example config (Unsplash License: free
# commercial use and redistribution, no attribution required).
install -Dm0644 docs/examples/assets/sakura-dusk.jpg \
  %{buildroot}%{_datadir}/veiland/sakura-dusk.jpg

# Ready-made example scenes. The hotplug repro config is a dev tool, and the
# two battery demo scenes reference widgets not installed as commands
# (battery.py / battery_cairo.py) -- drop all three. The scenes already
# reference every plugin by its bare veiland-<name> (Rust and Python alike),
# so only the repo-relative asset paths need rewriting to the installed
# wallpaper.
install -dm0755 %{buildroot}%{_datadir}/veiland/examples
install -m0644 docs/examples/*.toml %{buildroot}%{_datadir}/veiland/examples/
rm %{buildroot}%{_datadir}/veiland/examples/hotplug-repro.toml \
   %{buildroot}%{_datadir}/veiland/examples/battery_python.toml \
   %{buildroot}%{_datadir}/veiland/examples/battery_cairo.toml
sed -i 's|docs/examples/assets/|%{_datadir}/veiland/|' \
  %{buildroot}%{_datadir}/veiland/examples/*.toml

%files
%license LICENSE
%{_bindir}/veiland
%{_bindir}/veiland-wallpaper
%{_bindir}/veiland-clock
%{_bindir}/veiland-particles
%{_bindir}/veiland-vignette
%{_bindir}/veiland-label
%{_bindir}/veiland-sakura
%{_bindir}/veiland-snow
%{_bindir}/veiland-rain
%{_bindir}/veiland-embers
%{_bindir}/veiland-fireflies
%{_bindir}/veiland-gradient
%{_bindir}/veiland-parallax
%{_bindir}/veiland-blobs
%{_bindir}/veiland-raymarcher
# Python widget command shims + the stashed SDK/companions/scripts/icons tree.
%{_bindir}/veiland-now-playing
%{_bindir}/veiland-weather
%{_bindir}/veiland-wifi
%{_bindir}/veiland-ethernet
%{_bindir}/veiland-bluetooth
%{_bindir}/veiland-avatar
%{_bindir}/veiland-markup
%{_bindir}/veiland-shape
%{_bindir}/veiland-battery
%{_libdir}/veiland/
%config(noreplace) %{_sysconfdir}/pam.d/veiland
%dir %{_datadir}/veiland
%{_datadir}/veiland/config.example.toml
%{_datadir}/veiland/sakura-dusk.jpg
%{_datadir}/veiland/examples/

%changelog
* Sat Jul 04 2026 sylflo <veiland@sylvain-chateau.com> - 0.1.0-1
- Initial package: veiland-core + reference plugins, bundled PAM service.
- Run PAM authentication on a worker thread so a wrong password no longer
  freezes the animation.
