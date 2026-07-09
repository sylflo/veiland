# SPDX-License-Identifier: GPL-3.0-or-later
#
# RPM spec for veiland (Fedora/RHEL). Builds the real set (veiland-core
# + reference plugins) from source into /usr/bin and installs the
# veiland PAM service so password authentication works on install.
#
# The stress test plugin is not built or packaged.

Name:           veiland
Version:        0.1.1
Release:        1%{?dist}
Summary:        Wayland screen locker with process-isolated GPU plugins

License:        GPL-3.0-or-later
URL:            https://github.com/sylflo/veiland

# TODO(release): once v%%{version} is tagged, point Source0 at the
# release tarball and let %%autosetup unpack it:
#   Source0: %%{url}/archive/refs/tags/v%%{version}/%%{name}-%%{version}.tar.gz
# For now CI builds from the checked-out tree staged into %%{_builddir}.
Source0:        %{name}-%{version}.tar.gz

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
BuildRequires:  turbojpeg-devel

# Runtime shared libraries. (rpm also auto-derives these from the ELF
# NEEDED entries; listed explicitly for clarity.) The GPU driver .so is
# the user's system's job, like on Nix.
Requires:       pam
Requires:       mesa-libGL
Requires:       mesa-libEGL
Requires:       mesa-libgbm
Requires:       libdrm
Requires:       wayland
Requires:       libxkbcommon
Requires:       turbojpeg

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
  -p veiland-raymarcher

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

# PAM service (Fedora uses the system-auth variant).
install -Dm0644 packaging/pam/veiland.system-auth \
  %{buildroot}%{_sysconfdir}/pam.d/veiland

# Example config the user copies into ~/.config/veiland/.
install -Dm0644 packaging/veiland.example.toml \
  %{buildroot}%{_datadir}/veiland/config.example.toml

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
%config(noreplace) %{_sysconfdir}/pam.d/veiland
%dir %{_datadir}/veiland
%{_datadir}/veiland/config.example.toml

%changelog
* Thu Jul 10 2026 sylflo <veiland@sylvain-chateau.com> - 0.1.1-1
- Run PAM authentication on a worker thread so a wrong password no longer
  freezes the animation.

* Sat Jul 04 2026 sylflo <veiland@sylvain-chateau.com> - 0.1.0-1
- Initial package: veiland-core + reference plugins, bundled PAM service.
