#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the Arch package from the working tree, in an archlinux container.
#
# NixOS has no makepkg, so the package cannot be built natively here. Docker
# supplies the Arch toolchain without needing a VM. What this does NOT prove is
# that the result installs on a real booted system -- that is arch.sh's job.
# Build here, install there.
#
# Unlike the Debian and Fedora scripts, the container tag is deliberately left
# rolling (archlinux:latest). Arch is a rolling distribution: its users really
# do run latest, so `latest` is the release under test, not a moving target
# that drifts away from what people install.
#
#   ./build-arch.sh          build; writes dist/veiland-*.pkg.tar.zst
#
# Note the PKGBUILD runs check() -- the full workspace test suite -- as part of
# the build, so this is slower than a plain compile and will fail on a failing
# test. That is by design: it is what CI and the AUR do.
set -euo pipefail

REPO_ROOT=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
cd "$REPO_ROOT"

if ! command -v docker >/dev/null; then
    echo "!! docker is not on PATH. It supplies the Arch toolchain;" >&2
    echo "!! NixOS cannot run makepkg natively." >&2
    exit 1
fi

mkdir -p dist

echo ">> building the package in an archlinux container (compiles the whole"
echo ">> workspace and runs its tests, so the first run is slow)"

# Mirrors .github/workflows/packages.yml's `arch` job step for step. Keep them
# in sync: building here is only useful if it predicts what CI will do.
#
# makepkg refuses to run as root, so the container adds an unprivileged
# `builder` user and drops to it for the build itself.
#
# The source is mounted read-only: makepkg is given a private copy, so nothing
# it does can write into the working tree. dist/ is mounted separately,
# read-write, purely to receive the finished package.
docker run --rm \
    -v "$REPO_ROOT":/src:ro \
    -v "$REPO_ROOT/dist":/out \
    archlinux:latest \
    bash -euxc '
        # base-devel + the -sys crates makedepends, then the runtime depends
        # of the widget tier: makepkg checks that every depends=() entry is
        # installable before it builds (it aborts on a resolve failure), so
        # the Python/GI packages must be present here even though they are
        # runtime, not build, deps. Keep in sync with the PKGBUILD depends=()
        # and packages.yml.
        pacman -Syu --noconfirm base-devel git rust cargo pkgconf \
            pam libglvnd mesa libdrm wayland libxkbcommon \
            python python-gobject python-cairo python-pillow \
            python-jeepney librsvg pango gdk-pixbuf2
        useradd -m builder

        # makepkg builds out of $srcdir, and every PKGBUILD function does
        # `cd "$srcdir/$pkgname-$pkgver"` (matching the release tarball, whose
        # top dir is veiland-<ver>/). Stage the WORKING TREE under that exact
        # name so the cd succeeds and paths relative to it (target/, python/)
        # resolve.
        #
        # CRUCIAL: source=() in the PKGBUILD points at the published v0.1.0
        # release tarball, so a plain `makepkg` would DOWNLOAD and extract it
        # over $srcdir -- overwriting the staged working tree with released
        # source and building the OLD code (the bug that shipped a new-widgets /
        # old-core mix). --noextract makes makepkg use what we staged and skip
        # the download; --skipinteg skips the checksum of the tarball we are
        # deliberately not fetching. This matches the --noextract note in the
        # PKGBUILD and the CI packages workflow.
        ver=$(. /src/packaging/arch/PKGBUILD; echo "$pkgver")
        workdir=/home/builder/build
        mkdir -p "$workdir/src"
        cp /src/packaging/arch/PKGBUILD "$workdir/PKGBUILD"
        cp -r /src "$workdir/src/veiland-$ver"
        # A target/ copied from the host holds Nix-built artifacts for a
        # different toolchain and would only confuse cargo. Start clean.
        rm -rf "$workdir/src/veiland-$ver/target"
        chown -R builder:builder "$workdir"

        # makepkg refuses to run as root. -f overwrites an existing package
        # file. --noextract/--skipinteg force the staged tree (see above).
        # Deliberately NOT --nocheck: the PKGBUILD runs the workspace test
        # suite in check(), and running it is the point.
        su builder -c "cd $workdir && makepkg -f --noextract --skipinteg"

        cp "$workdir"/*.pkg.tar.zst /out/
        # The container runs as root, so hand the artifact back to the invoking
        # user rather than leaving root-owned files in dist/.
        chown "$(stat -c %u:%g /out)" /out/*.pkg.tar.zst
    '

echo
echo ">> built:"
ls -la dist/*.pkg.tar.zst

echo
echo ">> next: install it on a real Arch system"
echo ">>   ./scripts/vmtest/arch.sh --clean   # reprovision"
echo ">>   ./scripts/vmtest/arch.sh           # boot + install this package"
