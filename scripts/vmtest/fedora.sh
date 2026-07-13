#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
#
# Boot a Fedora 44 VM under QEMU and install the locally-built .rpm, to check
# that the package installs on a real booted Fedora and that the locker runs.
#
# Build the package first:
#
#   ./scripts/vmtest/build-rpm.sh    # produces dist/veiland-*.rpm
#   ./scripts/vmtest/fedora.sh       # boots a VM and installs it
#
# The two are deliberately separate. build-rpm.sh runs a fedora:44 container,
# so it predicts what CI builds. This script proves what a container cannot:
# that the artifact installs on a real system, that dnf can resolve the spec's
# Requires from Fedora's own repos, and that the binary runs. Testing a
# *locally built* package rather than a released one means the loop does not
# require cutting a release to find out whether a release is worth cutting --
# which is how the Debian libturbojpeg bug shipped.
#
#   ./fedora.sh          boot the VM (downloads + provisions on first run)
#   ./fedora.sh --clean  drop the overlay disk + seed, forcing a reprovision
#
# cloud-init only applies its config on an instance's FIRST boot, so after
# rebuilding the .rpm you want --clean to get a fresh install of it.
#
# Once it is up:  ssh -p 2223 fedora@localhost
# On the QEMU console:  user `fedora`, password `veiland`.
#
# To see the locker actually lock: log in on the QEMU console, run `sway`, open
# a terminal (Super+Enter), then run `veiland`. Unlock with the password above.
# Check `glxinfo -B | grep renderer` first: `virgl` means GL is reaching the
# host GPU, `llvmpipe` means software rendering and the DMA-BUF path may fall
# back to a blank layer -- a limitation of the VM, not of veiland.
set -euo pipefail

REPO_ROOT=$(git -C "$(dirname "${BASH_SOURCE[0]}")" rev-parse --show-toplevel)

VM_DIR="${VEILAND_VM_DIR:-$HOME/.cache/veiland-vmtest/fedora}"

# Fedora 44 Cloud. Must match the .fc44 the RPM is built as: an .fc44 package's
# dependencies are resolved against Fedora 44's repos.
#
# The trailing compose number (-1.7) is baked into the filename and is not
# stable across respins -- there is no "latest" symlink the way Debian has one.
# If this 404s, list the images directory and update the number.
IMG_URL="https://download.fedoraproject.org/pub/fedora/linux/releases/44/Cloud/x86_64/images/Fedora-Cloud-Base-Generic-44-1.7.x86_64.qcow2"

BASE_IMG="$VM_DIR/fedora-44-base.qcow2"
DISK="$VM_DIR/fedora.qcow2"
SEED="$VM_DIR/seed.iso"

# Distinct from Debian's 2222 so both VMs can run at once without clashing.
SSH_PORT=2223

CPUS=$(( $(nproc) / 2 ))
MEM=4096

if [[ "${1:-}" == "--clean" ]]; then
    rm -f "$DISK" "$SEED"
    echo "removed the overlay disk and seed; the base image is kept."
    echo "delete $VM_DIR to drop the downloaded image too."
    exit 0
fi

# The artifact under test, built by build-rpm.sh. Exclude the -debuginfo and
# -debugsource subpackages: only the main package is what a user installs.
shopt -s nullglob
rpms=("$REPO_ROOT"/dist/veiland-[0-9]*.rpm)
shopt -u nullglob
if [[ ${#rpms[@]} -eq 0 ]]; then
    echo "!! no .rpm in $REPO_ROOT/dist/. Build one first:" >&2
    echo "!!   ./scripts/vmtest/build-rpm.sh" >&2
    exit 1
fi
RPM_PATH="${rpms[0]}"
RPM_NAME=$(basename "$RPM_PATH")
echo ">> package under test: $RPM_NAME"

mkdir -p "$VM_DIR"

# 1. Base cloud image: fetched once, never booted directly. The VM runs on a
#    copy-on-write overlay, so a wrecked guest is one --clean away and we never
#    re-download.
if [[ ! -f "$BASE_IMG" ]]; then
    echo ">> downloading the Fedora 44 cloud image"
    curl -fL --progress-bar -o "$BASE_IMG.part" "$IMG_URL"
    mv "$BASE_IMG.part" "$BASE_IMG"
fi

# 2. Overlay disk backed by the pristine base.
if [[ ! -f "$DISK" ]]; then
    echo ">> creating the overlay disk"
    qemu-img create -f qcow2 -F qcow2 -b "$BASE_IMG" "$DISK" 20G >/dev/null
fi

# 3. Getting the .rpm into the guest.
#
#    cloud-init cannot read a file on the host, so the package has to arrive
#    over a channel the guest can see. QEMU's user-mode network always maps the
#    host to 10.0.2.2, so a throwaway HTTP server on the host is reachable from
#    inside the guest with no extra tooling -- no virtiofs, no 9p, no scp step
#    the human has to remember. It serves dist/ and dies with this script.
#
#    A different port from debian.sh's, so both VMs can be up at once.
HTTP_PORT=8100
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
hostname: veiland-fedora
users:
  - name: fedora
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
  - glx-utils
  - mesa-dri-drivers
  # curl fetches the .rpm from the host below.
  - curl
runcmd:
  # Fetch the .rpm from the host's throwaway HTTP server (10.0.2.2 is the host,
  # as seen from QEMU's user-mode network) and install it.
  #
  # Deliberately NOT pre-installing veiland's runtime libraries: dnf resolves
  # the package's own declared Requires against Fedora's repos, so a missing or
  # unsatisfiable one fails the install right here and the packaging bug
  # surfaces. Pre-seeding the libraries would paper over exactly the defect
  # this VM exists to catch -- and did catch on Debian: a libturbojpeg floor
  # (>= 1:3.1.3) that no Debian release could satisfy.
  - [ sh, -c, "curl -fL -o /tmp/$RPM_NAME 'http://10.0.2.2:$HTTP_PORT/$RPM_NAME'" ]
  - [ sh, -c, "dnf install -y /tmp/$RPM_NAME" ]
  # A breadcrumb to read from the host: did it install, and does it run?
  - [ sh, -c, "veiland --version > /var/log/veiland-install.log 2>&1 || echo 'veiland failed to run' > /var/log/veiland-install.log" ]
EOF

echo "instance-id: veiland-fedora" > "$VM_DIR/meta-data"
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

echo ">> booting (console login: fedora / veiland)"
echo ">> check the install once cloud-init settles:"
echo ">>   ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \\"
echo ">>       -p $SSH_PORT fedora@localhost 'sudo cloud-init status; rpm -q veiland'"

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
