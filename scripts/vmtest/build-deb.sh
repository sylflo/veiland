#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the Debian package from the working tree, in a debian:13 container.
#
# NixOS has no dpkg-buildpackage, so the .deb cannot be built natively here.
# Docker gives us the Debian toolchain without a VM, and pinning the same
# image the CI job uses (debian:13) means a build that works here is the
# build CI runs -- same container, same packaging/debian/ rules, same rustup
# toolchain. What this does NOT prove is that the result installs on a real
# booted system; that is what debian.sh is for. Build here, install there.
#
# Why debian:13 and not debian:testing: `testing` is a moving tag. It was
# trixie when the CI was first written, but trixie became Debian 13 stable
# and the tag rolled forward to the next release -- which carries a newer
# libjpeg-turbo. Building and install-checking on it was circular, and it
# stayed green while the shipped .deb was uninstallable on the Debian people
# actually run.
#
#   ./build-deb.sh          build; writes dist/veiland_*.deb
#
# The .deb lands in dist/ at the repo root, where debian.sh picks it up.
set -euo pipefail

REPO_ROOT=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
cd "$REPO_ROOT"

if ! command -v docker >/dev/null; then
    echo "!! docker is not on PATH. It is what supplies the Debian toolchain;" >&2
    echo "!! NixOS cannot run dpkg-buildpackage natively." >&2
    exit 1
fi

mkdir -p dist

echo ">> building the .deb in a debian:13 container (this compiles the whole"
echo ">> workspace from scratch, so the first run is slow)"

# Mount the repo at /src and build there. --rm because the container is
# disposable; the artifact is what we want, not the container.
#
# The build mirrors .github/workflows/packages.yml's `debian` job step for
# step. Keep the two in sync: the point of building here is that it predicts
# what CI will do.
docker run --rm \
    -v "$REPO_ROOT":/src \
    -w /src \
    debian:13 \
    bash -euxc '
        apt-get update

        # build-essential + debhelper supply dpkg-buildpackage/dh; the rest
        # are the package Build-Depends. Deliberately NOT apt cargo/rustc:
        # debian stable ships rustc 1.85 and the dependency tree needs 1.89
        # (cosmic-text, image, smol_str), so we take a current toolchain from
        # rustup instead and leave Cargo.lock alone.
        DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends \
            build-essential debhelper devscripts pkg-config \
            curl ca-certificates \
            libpam0g-dev libgl-dev libegl-dev libgbm-dev \
            libdrm-dev libwayland-dev libxkbcommon-dev

        curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs \
            | sh -s -- -y --profile minimal --default-toolchain stable
        export PATH="$HOME/.cargo/bin:$PATH"

        # dpkg-buildpackage wants debian/ at the top of the source tree; ours
        # lives under packaging/debian/, so stage a copy there. Build out of
        # a scratch copy rather than the mounted tree: the build writes into
        # the source dir (debian/, target/, .deb in the parent) and we do not
        # want any of that landing in the working tree as root-owned files.
        cp -r /src /build
        cd /build
        cp -r packaging/debian debian

        # -b binary-only, -us -uc unsigned, -d skip the Build-Depends check
        # (they are installed above; Rust deliberately is not among them).
        dpkg-buildpackage -b -us -uc -d

        # dpkg writes the .deb to the PARENT directory.
        cp /*.deb /src/dist/
        # The container runs as root, so the artifact would be root-owned on
        # the host. Hand it back to the invoking user.
        chown "$(stat -c %u:%g /src)" /src/dist/*.deb
    '

echo
echo ">> built:"
ls -la dist/*.deb

echo
echo ">> next: install it on a real Debian 13 system"
echo ">>   ./scripts/vmtest/debian.sh --clean   # reprovision"
echo ">>   ./scripts/vmtest/debian.sh           # boot + install this .deb"
