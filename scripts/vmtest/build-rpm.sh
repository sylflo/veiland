#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Build the Fedora package from the working tree, in a fedora:44 container.
#
# NixOS has no rpmbuild, so the .rpm cannot be built natively here. Docker
# supplies the Fedora toolchain without needing a VM. What this does NOT prove
# is that the result installs on a real booted system -- that is fedora.sh's
# job. Build here, install there.
#
# Pinned to fedora:44, while the CI job uses fedora:latest. The RPM is built
# as .fc44 and the test VM is a Fedora 44 cloud image, so pinning keeps the
# build, the artifact, and the test on one release. A rolling `latest` tag is
# how the Debian package broke: it was built and install-checked on a moving
# target and stayed green while it could not install on the release users run.
#
#   ./build-rpm.sh          build; writes dist/veiland-*.rpm
#
# NOTE: rpmbuild packages a `git archive` of HEAD, not the working tree, so
# uncommitted changes are NOT in the resulting RPM. Commit first.
set -euo pipefail

REPO_ROOT=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)
cd "$REPO_ROOT"

if ! command -v docker >/dev/null; then
    echo "!! docker is not on PATH. It supplies the Fedora toolchain;" >&2
    echo "!! NixOS cannot run rpmbuild natively." >&2
    exit 1
fi

if ! git diff --quiet HEAD -- packaging plugins veiland-core veiland-plugin \
        veiland-protocol veiland-text Cargo.toml Cargo.lock 2>/dev/null; then
    echo ">> NOTE: you have uncommitted changes. rpmbuild packages a git"
    echo ">> archive of HEAD, so they will NOT be in this RPM."
fi

mkdir -p dist

echo ">> building the .rpm in a fedora:44 container (compiles the whole"
echo ">> workspace from scratch, so the first run is slow)"

# The build mirrors .github/workflows/packages.yml's `fedora` job step for
# step, other than the pinned tag. Keep them in sync: building here is only
# useful if it predicts what CI will do.
#
# Unlike Debian, Fedora's own cargo/rust are new enough for the dependency
# tree, so there is no rustup step.
docker run --rm \
    -v "$REPO_ROOT":/src \
    -w /src \
    fedora:44 \
    bash -euxc '
        dnf -y install rpm-build rpmdevtools tar git \
            cargo rust pkgconf-pkg-config \
            pam-devel mesa-libGL-devel mesa-libEGL-devel mesa-libgbm-devel \
            libdrm-devel wayland-devel libxkbcommon-devel

        rpmdev-setuptree

        # Derive the version from the spec so the tarball name always matches
        # what %autosetup expects. (It was once hardcoded and drifted behind a
        # version bump, breaking %prep.)
        ver=$(rpmspec -q --qf "%{version}\n" packaging/rpm/veiland.spec | head -1)

        # The repo is bind-mounted from the host, so git refuses to operate on
        # it as root without this.
        git config --global --add safe.directory /src

        # rpmbuild wants the source as %{name}-%{version}.tar.gz whose top dir
        # is %{name}-%{version}. git archive gives exactly that -- and means the
        # RPM is built from committed HEAD, not from whatever is dirty in the
        # working tree.
        git archive --format=tar --prefix="veiland-$ver/" HEAD \
            | gzip > "$HOME/rpmbuild/SOURCES/veiland-$ver.tar.gz"

        rpmbuild -ba packaging/rpm/veiland.spec

        # Only the main package. rpmbuild also emits -debuginfo and -debugsource
        # subpackages; shipping those alongside made a `dnf install ./veiland-*.rpm`
        # glob pull in all three. veiland-$ver-* cannot match them, because their
        # names put -debuginfo-/-debugsource- before the version.
        cp "$HOME"/rpmbuild/RPMS/*/veiland-"$ver"-*.rpm /src/dist/

        # The container runs as root, so hand the artifact back to the invoking
        # user rather than leaving root-owned files in the working tree.
        chown "$(stat -c %u:%g /src)" /src/dist/*.rpm
    '

echo
echo ">> built:"
ls -la dist/*.rpm

echo
echo ">> next: install it on a real Fedora 44 system"
echo ">>   ./scripts/vmtest/fedora.sh --clean   # reprovision"
echo ">>   ./scripts/vmtest/fedora.sh           # boot + install this .rpm"
