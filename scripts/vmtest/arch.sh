#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Boot an Arch VM under QEMU and install the locally-built package, to check
# that it installs on a real booted Arch and that the locker runs.
#
# Build the package first:
#
#   ./scripts/vmtest/build-arch.sh   # produces dist/veiland-*.pkg.tar.zst
#   ./scripts/vmtest/arch.sh         # boots a VM and installs it
#
# The two are deliberately separate. build-arch.sh runs an archlinux container,
# so it predicts what CI builds. This script proves what a container cannot:
# that the package installs on a real system, that pacman can resolve its
# depends=() from Arch's own repos, and that the binary runs. Testing a
# *locally built* package rather than a released one means the loop does not
# require cutting a release to find out whether a release is worth cutting --
# which is how the Debian libturbojpeg bug shipped.
#
#   ./arch.sh          boot the VM (downloads + provisions on first run)
#   ./arch.sh --clean  drop the overlay disk + seed, forcing a reprovision
#
# cloud-init only applies its config on an instance's FIRST boot, so after
# rebuilding the package you want --clean to get a fresh install of it.
#
# Once it is up:  ssh -p 2224 arch@localhost
# On the QEMU console:  user `arch`, password `veiland`.
#
# To see the locker actually lock: log in on the QEMU console, run `sway`, open
# a terminal (Super+Enter), then run `veiland`. Unlock with the password above.
# Check `glxinfo -B | grep renderer` first: `virgl` means GL is reaching the
# host GPU, `llvmpipe` means software rendering and the DMA-BUF path may fall
# back to a blank layer -- a limitation of the VM, not of veiland.
set -euo pipefail

REPO_ROOT=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)

VM_DIR="${VEILAND_VM_DIR:-$HOME/.cache/veiland-vmtest/arch}"

# Arch's official cloud image. Unlike Debian and Fedora there is no release to
# pin to -- Arch is rolling, and `latest` is genuinely what users run, so this
# tracks it deliberately rather than as an oversight.
IMG_URL="https://geo.mirror.pkgbuild.com/images/latest/Arch-Linux-x86_64-cloudimg.qcow2"

BASE_IMG="$VM_DIR/arch-base.qcow2"
DISK="$VM_DIR/arch.qcow2"
SEED="$VM_DIR/seed.iso"

# Distinct from Debian's 2222 and Fedora's 2223 so all three can run at once.
SSH_PORT=2224

CPUS=$(( $(nproc) / 2 ))
MEM=4096

if [[ "${1:-}" == "--clean" ]]; then
    rm -f "$DISK" "$SEED"
    echo "removed the overlay disk and seed; the base image is kept."
    echo "delete $VM_DIR to drop the downloaded image too."
    exit 0
fi

# The artifact under test, built by build-arch.sh. Exclude the -debug
# subpackage: only the main package is what a user installs.
shopt -s nullglob
pkgs=("$REPO_ROOT"/dist/veiland-[0-9]*.pkg.tar.zst)
shopt -u nullglob
if [[ ${#pkgs[@]} -eq 0 ]]; then
    echo "!! no package in $REPO_ROOT/dist/. Build one first:" >&2
    echo "!!   ./scripts/vmtest/build-arch.sh" >&2
    exit 1
fi
PKG_PATH="${pkgs[0]}"
PKG_NAME=$(basename "$PKG_PATH")
echo ">> package under test: $PKG_NAME"

mkdir -p "$VM_DIR"

# 1. Base cloud image: fetched once, never booted directly. The VM runs on a
#    copy-on-write overlay, so a wrecked guest is one --clean away and we never
#    re-download.
if [[ ! -f "$BASE_IMG" ]]; then
    echo ">> downloading the Arch cloud image"
    curl -fL --progress-bar -o "$BASE_IMG.part" "$IMG_URL"
    mv "$BASE_IMG.part" "$BASE_IMG"
fi

# 2. Overlay disk backed by the pristine base.
if [[ ! -f "$DISK" ]]; then
    echo ">> creating the overlay disk"
    qemu-img create -f qcow2 -F qcow2 -b "$BASE_IMG" "$DISK" 20G >/dev/null
fi

# 3. Getting the package into the guest.
#
#    cloud-init cannot read a file on the host, so the package has to arrive
#    over a channel the guest can see. QEMU's user-mode network always maps the
#    host to 10.0.2.2, so a throwaway HTTP server on the host is reachable from
#    inside the guest with no extra tooling -- no virtiofs, no 9p, no scp step
#    the human has to remember. It serves dist/ and dies with this script.
#
#    A different port from the other two scripts', so all three can be up.
HTTP_PORT=8101
python3 -m http.server "$HTTP_PORT" \
    --directory "$REPO_ROOT/dist" \
    --bind 127.0.0.1 >/dev/null 2>&1 &
HTTP_PID=$!
trap 'kill "$HTTP_PID" 2>/dev/null || true' EXIT
echo ">> serving dist/ to the guest on 10.0.2.2:$HTTP_PORT"

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
hostname: veiland-arch
users:
  - name: arch
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
bootcmd:
  # The image's pacman-init.service (pacman-key --init + --populate) is gated
  # on ConditionFirstBoot=yes, and a fresh overlay boot has been observed to
  # come up with that condition unmet -- systemd generated a fresh machine-id
  # yet never set the first-boot flag -- leaving the keyring absent and every
  # signed pacman transaction failing (\`error: keyring is not writable\`).
  # Don't bet the provision on winning that upstream coin toss: initialize the
  # keyring ourselves. bootcmd runs before the packages module; the guard
  # makes reboots a no-op.
  - [ sh, -c, "[ -d /etc/pacman.d/gnupg ] || { pacman-key --init && pacman-key --populate; }" ]
packages:
  # A compositor implementing ext-session-lock-v1 to host the lock surface, a
  # terminal to launch veiland from, and the guest-side Mesa userspace that
  # virgl drives.
  - sway
  - foot
  - mesa-utils
  # curl fetches the package from the host below.
  - curl
runcmd:
  # Fetch the package from the host's throwaway HTTP server (10.0.2.2 is the
  # host, as seen from QEMU's user-mode network) and install it.
  #
  # Deliberately NOT pre-installing veiland's runtime libraries: pacman -U
  # resolves the package's own declared depends=() against Arch's repos, so a
  # missing or unsatisfiable one fails the install right here and the packaging
  # bug surfaces. Pre-seeding the libraries would paper over exactly the defect
  # this VM exists to catch -- and did catch on Debian: a libturbojpeg floor
  # (>= 1:3.1.3) that no Debian release could satisfy.
  - [ sh, -c, "curl -fL -o /tmp/$PKG_NAME 'http://10.0.2.2:$HTTP_PORT/$PKG_NAME'" ]
  # Bounded retry around pacman -U: provisioning has been observed to leave a
  # stale, empty /var/lib/pacman/db.lck behind after the packages module ran
  # (owner unknown; the guest clock stepped during first boot, so lock-file
  # mtime forensics are useless). Clear the lock only when no pacman is
  # actually alive, and give up after ~30s rather than hanging the provision.
  - [ sh, -c, "for i in 1 2 3 4 5 6 7 8 9 10; do pacman -U --noconfirm /tmp/$PKG_NAME && break; sleep 3; pgrep -x pacman >/dev/null || rm -f /var/lib/pacman/db.lck; done" ]
  # A breadcrumb to read from the host: did it install, and does it run?
  - [ sh, -c, "veiland --version > /var/log/veiland-install.log 2>&1 || echo 'veiland failed to run' > /var/log/veiland-install.log" ]
EOF

echo "instance-id: veiland-arch" > "$VM_DIR/meta-data"
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
    echo ">> path may be too slow to judge fairly. Enable SVM in the BIOS."
fi

echo ">> booting (console login: arch / veiland)"
echo ">> check the install once cloud-init settles:"
echo ">>   ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \\"
echo ">>       -p $SSH_PORT arch@localhost 'sudo cloud-init status; pacman -Q veiland'"

# virtio-gpu-gl + gl=on is the virgl path: guest GL is forwarded to the host GPU
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
# (Observed: with blob off, `eglinfo` in the guest reported llvmpipe; with it
# on, virgl on the host's actual GPU.)
qemu-system-x86_64 \
    "${accel[@]}" \
    -smp "$CPUS" \
    -m "$MEM" \
    -drive file="$DISK",if=virtio,format=qcow2 \
    -drive file="$SEED",if=virtio,format=raw,readonly=on \
    -device virtio-gpu-gl,blob=on \
    -display gtk,gl=on \
    -device virtio-net-pci,netdev=net0 \
    -netdev user,id=net0,hostfwd=tcp::"$SSH_PORT"-:22 \
    -device virtio-keyboard-pci \
    -device virtio-tablet-pci
