#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Boot a Debian 13 VM under QEMU and install the locally-built .deb, to check
# that the package installs on a real booted Debian and that the locker runs.
#
# Build the package first:
#
#   ./scripts/vmtest/build-deb.sh    # produces dist/veiland_*.deb
#   ./scripts/vmtest/debian.sh       # boots a VM and installs it
#
# The two are deliberately separate. build-deb.sh runs the same debian:13
# container CI does, so it predicts what CI will build. This script proves
# what a container cannot: that the artifact installs on a real system, that
# apt can resolve its ${shlibs:Depends} closure from Debian's own repos, and
# that the binary runs. Testing a *locally built* .deb rather than a released
# one means the loop does not require cutting a release to find out whether a
# release is worth cutting -- which is how the libturbojpeg bug shipped.
# Once a release IS cut, --release is the complementary check: it fetches the
# .deb users actually download from the GitHub release and installs that.
#
#   ./debian.sh                  boot the VM (downloads + provisions on first run)
#   ./debian.sh --clean          drop the overlay disk + seed, forcing a reprovision
#   ./debian.sh --release [TAG]  test the released .deb (latest release, or TAG)
#                                instead of the locally-built one
#
# cloud-init only applies its config on an instance's FIRST boot, so after
# rebuilding the .deb -- or when switching to --release -- run --clean first
# to get a fresh install of it.
#
# Once it is up:  ssh -p 2222 debian@localhost
# On the QEMU console:  user `debian`, password `veiland`.
#
# The guest gets a virtio-gpu-gl (virgl) device, which forwards GL to the host
# GPU. Whether veiland's DMA-BUF import survives that is an open question this
# VM exists to answer: if eglCreateImage rejects the imported fd, the core
# closes the plugin socket and draws its fallback -- the session still locks,
# but the zero-copy path went untested. That is a result worth knowing, not a
# broken harness.
set -euo pipefail

REPO_ROOT=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)

VM_DIR="${VEILAND_VM_DIR:-$HOME/.cache/veiland-vmtest/debian}"

# Debian 13 (trixie), the current stable -- the release the .deb targets and
# the one that exposed the libturbojpeg floor no Debian could satisfy.
IMG_URL="https://cloud.debian.org/images/cloud/trixie/latest/debian-13-generic-amd64.qcow2"

BASE_IMG="$VM_DIR/debian-13-base.qcow2"
DISK="$VM_DIR/debian.qcow2"
SEED="$VM_DIR/seed.iso"
SSH_PORT=2222

CPUS=$(( $(nproc) / 2 ))
MEM=4096

if [[ "${1:-}" == "--clean" ]]; then
    rm -f "$DISK" "$SEED"
    echo "removed the overlay disk and seed; the base image is kept."
    echo "delete $VM_DIR to drop the downloaded image too."
    exit 0
fi

# The artifact under test: the locally-built .deb from dist/ by default, or --
# with --release -- the artifact users actually download, fetched from the
# GitHub release into its own directory so the glob below can never silently
# pick a local build when a released one was asked for (or vice versa).
DIST_DIR="$REPO_ROOT/dist"
RELEASE_TAG=""
if [[ "${1:-}" == "--release" ]]; then
    if ! command -v gh >/dev/null; then
        echo "!! --release needs the gh CLI on PATH." >&2
        exit 1
    fi
    RELEASE_TAG="${2:-$(cd "$REPO_ROOT" && gh release view --json tagName -q .tagName)}"
    DIST_DIR="$REPO_ROOT/dist/release-$RELEASE_TAG"
    mkdir -p "$DIST_DIR"
    shopt -s nullglob
    cached=("$DIST_DIR"/veiland_*.deb)
    shopt -u nullglob
    if [[ ${#cached[@]} -eq 0 ]]; then
        echo ">> downloading the $RELEASE_TAG .deb from the GitHub release"
        (cd "$REPO_ROOT" && gh release download "$RELEASE_TAG" \
            --pattern '*.deb' --dir "$DIST_DIR")
    fi
    if [[ -f "$DISK" ]]; then
        echo ">> NOTE: an overlay disk already exists, and cloud-init only installs"
        echo ">> on FIRST boot -- the release .deb will not be installed into it."
        echo ">> Run './debian.sh --clean' first, then rerun with --release."
    fi
fi

shopt -s nullglob
debs=("$DIST_DIR"/veiland_*.deb)
shopt -u nullglob
if [[ ${#debs[@]} -eq 0 ]]; then
    if [[ -n "$RELEASE_TAG" ]]; then
        echo "!! no .deb asset on release $RELEASE_TAG." >&2
    else
        echo "!! no .deb in $REPO_ROOT/dist/. Build one first:" >&2
        echo "!!   ./scripts/vmtest/build-deb.sh" >&2
    fi
    exit 1
fi
DEB_PATH="${debs[0]}"
DEB_NAME=$(basename "$DEB_PATH")
if [[ -n "$RELEASE_TAG" ]]; then
    echo ">> package under test: $DEB_NAME (release $RELEASE_TAG)"
else
    echo ">> package under test: $DEB_NAME (locally built)"
fi

mkdir -p "$VM_DIR"

# 1. Base cloud image: fetched once, never booted directly. The VM runs on a
#    copy-on-write overlay, so a wrecked guest is one --clean away and we never
#    re-download.
if [[ ! -f "$BASE_IMG" ]]; then
    echo ">> downloading the Debian 13 cloud image"
    curl -fL --progress-bar -o "$BASE_IMG.part" "$IMG_URL"
    mv "$BASE_IMG.part" "$BASE_IMG"
fi

# 2. Overlay disk backed by the pristine base.
if [[ ! -f "$DISK" ]]; then
    echo ">> creating the overlay disk"
    qemu-img create -f qcow2 -F qcow2 -b "$BASE_IMG" "$DISK" 20G >/dev/null
fi

# 3. Getting the .deb into the guest.
#
#    cloud-init cannot read a file on the host, so the package has to arrive
#    over a channel the guest can see. QEMU's user-mode network always maps the
#    host to 10.0.2.2, so a throwaway HTTP server on the host is reachable from
#    inside the guest with no extra tooling -- no virtiofs, no 9p, no scp step
#    the human has to remember. It serves the artifact's directory and dies
#    with this script.
HTTP_PORT=8099
python3 -m http.server "$HTTP_PORT" \
    --directory "$DIST_DIR" \
    --bind 127.0.0.1 >/dev/null 2>&1 &
HTTP_PID=$!
trap 'kill "$HTTP_PID" 2>/dev/null || true' EXIT
echo ">> serving ${DIST_DIR#"$REPO_ROOT"/} to the guest on 10.0.2.2:$HTTP_PORT"

# 4. cloud-init seed. A cloud image ships with no user and no password, so this
#    is the only way in.
#
#    Rebuilt on every run, never cached: it costs a fraction of a second, and a
#    cached seed means an edit to this script silently does nothing.
echo ">> building the cloud-init seed"

ssh_key=""
for candidate in "$HOME/.ssh/id_ed25519.pub" "$HOME/.ssh/id_rsa.pub"; do
    if [[ -f "$candidate" ]]; then
        ssh_key=$(cat "$candidate")
        break
    fi
done
if [[ -z "$ssh_key" ]]; then
    echo "!! no SSH public key at ~/.ssh/id_ed25519.pub or ~/.ssh/id_rsa.pub." >&2
    echo "!! create one with: ssh-keygen -t ed25519" >&2
    exit 1
fi

cat > "$VM_DIR/user-data" <<EOF
#cloud-config
hostname: veiland-debian
users:
  - name: debian
    sudo: ALL=(ALL) NOPASSWD:ALL
    shell: /bin/bash
    lock_passwd: false
    # A real password as well as the key: veiland is a *lock screen*, so
    # testing an unlock needs credentials PAM can actually authenticate
    # against, and the QEMU console needs a login that is not SSH.
    plain_text_passwd: veiland
    ssh_authorized_keys:
      - $ssh_key
ssh_pwauth: true
package_update: true
packages:
  # A compositor implementing ext-session-lock-v1 to host the lock surface, a
  # terminal to launch veiland from, and the guest-side Mesa userspace that
  # virgl drives.
  - sway
  - foot
  - mesa-utils
  # curl fetches the .deb from the host below.
  - curl
runcmd:
  # Fetch the .deb from the host's throwaway HTTP server (10.0.2.2 is the host,
  # as seen from QEMU's user-mode network) and install it.
  #
  # Deliberately NOT pre-installing veiland's runtime libraries: \`apt install
  # ./x.deb\` resolves the package's own declared dependencies against Debian's
  # repos, so a missing or unsatisfiable one fails the install right here and
  # the packaging bug surfaces. Pre-seeding the libraries would paper over
  # exactly the defect this VM exists to catch -- and did catch: a libturbojpeg
  # floor (>= 1:3.1.3) that no Debian release can satisfy.
  - [ sh, -c, "curl -fL -o /tmp/$DEB_NAME 'http://10.0.2.2:$HTTP_PORT/$DEB_NAME'" ]
  - [ sh, -c, "apt-get install -y /tmp/$DEB_NAME" ]
  # A breadcrumb to read from the host: did it install, and does it run?
  - [ sh, -c, "veiland --version > /var/log/veiland-install.log 2>&1 || echo 'veiland failed to run' > /var/log/veiland-install.log" ]
EOF

echo "instance-id: veiland-debian" > "$VM_DIR/meta-data"
cloud-localds "$SEED" "$VM_DIR/user-data" "$VM_DIR/meta-data"

# 5. Boot.
#
# Without /dev/kvm the guest CPU is interpreted (TCG). It boots, but a
# compositor driving a GPU pipeline under emulation is slow enough that a
# graphical verdict cannot be trusted -- so say so loudly rather than let a bad
# frame rate get blamed on veiland.
accel=(-cpu max)
if [[ -r /dev/kvm ]]; then
    echo ">> /dev/kvm present: hardware acceleration on"
    accel=(-enable-kvm -cpu host)
else
    echo ">> WARNING: /dev/kvm is missing, so this runs under TCG (software"
    echo ">> emulation). It will boot, but it will be slow, and the graphical"
    echo ">> path may be too slow to judge fairly. Load kvm-amd to fix."
fi

echo ">> booting; ssh -p $SSH_PORT debian@localhost (console: debian / veiland)"
echo ">> when it settles:"
echo ">>   ssh -p $SSH_PORT debian@localhost 'cat /var/log/veiland-install.log'"

# virtio-vga-gl + gl=on is the virgl path: guest GL is forwarded to the host GPU
# rather than software-rasterized. On a single-GPU host this is as close to a
# real GPU as a guest gets -- passthrough would take the card from the host.
#
# blob=on is not optional for veiland, and it defaults to OFF. It turns guest
# GPU buffers into real host dmabufs; without it the guest gets virgl's virtual
# resources, which cannot back a cross-process dmabuf export/import. That
# export/import IS veiland's rendering model (a plugin allocates via GBM and
# passes the fd; the core imports it with eglCreateImage), so a guest without
# blob resources gets no usable 3D at all: guest Mesa refuses virtio_gpu_dri.so,
# falls back to Zink, finds no Vulkan driver, and every GL client degrades.
#
# virtio-vga-gl rather than virtio-gpu-gl, and -vga none: the guest needs
# exactly ONE gpu. Without -vga none QEMU quietly adds its default VGA (bochs)
# as a SECOND gpu, the compositor initializes on that boot VGA -- no render
# node, no GL -- falls back to software rendering and stops advertising
# linux-dmabuf, and every Wayland EGL client (veiland-core included) lands on
# llvmpipe, compositing the plugins' virgl-rendered dmabufs as black. But the
# one gpu cannot be the VGA-less virtio-gpu-gl either: Debian's BIOS GRUB
# reset-loops on a machine with no VGA at all (observed on the serial console:
# an endless "Booting `Debian GNU/Linux'", one reset per ~0.2s, kernel never
# reached -- while Fedora's and Arch's bootloaders shrug it off). virtio-vga-gl
# is the same virtio gpu with a VGA-compatible boot framebuffer bolted on:
# GRUB gets its VGA, the guest still sees a single card (one /dev/dri/card0,
# virgl + blob confirmed in dmesg), and firmware/boot output is visible in
# the window from power-on.
qemu-system-x86_64 \
    "${accel[@]}" \
    -smp "$CPUS" \
    -m "$MEM" \
    -drive file="$DISK",if=virtio,format=qcow2 \
    -drive file="$SEED",if=virtio,format=raw,readonly=on \
    -vga none \
    -device virtio-vga-gl,blob=on \
    -display gtk,gl=on \
    -device virtio-net-pci,netdev=net0 \
    -netdev user,id=net0,hostfwd=tcp::"$SSH_PORT"-:22 \
    -device virtio-keyboard-pci \
    -device virtio-tablet-pci
