<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# VM packaging tests

Boot a real Debian, Fedora, or Arch system in QEMU, install the package
built from this tree, and see whether the locker actually locks.

**These are manual. CI does not run them, and cannot** — they need a GPU,
a compositor, and a human to look at the screen. They are the layer above
CI, not a replacement for it. Think of `nix flake check` as "does it
compile", CI's install checks as "does the package resolve its
dependencies", and these as "does it work on a system a user actually
runs".

## Why they exist

The published `veiland_0.1.0-1_amd64.deb` was uninstallable on Debian 13.
`turbojpeg` forced `Depends: libturbojpeg0 (>= 1:3.1.3)` and no Debian
release ships libjpeg-turbo 3.x — but CI was green, because it built and
install-checked on `debian:testing`, a rolling tag that had drifted onto a
Debian carrying 3.x. A circular test: the package was verified against the
only distro it could install on.

The first VM boot found it in minutes. That is the gap these close — a
container tells you the package resolves; only a booted machine tells you
it installs on the release people run, and only a screen tells you it
locks.

## Use

Two steps per distro: build the package in a container, then install it in
a VM.

```sh
# Debian
./scripts/vmtest/build-deb.sh          # -> dist/veiland_*.deb
./scripts/vmtest/debian.sh             # boot, install, run

# Fedora
./scripts/vmtest/build-rpm.sh          # -> dist/veiland-*.rpm
./scripts/vmtest/fedora.sh

# Arch
./scripts/vmtest/build-arch.sh         # -> dist/veiland-*.pkg.tar.zst
./scripts/vmtest/arch.sh
```

The build scripts run in Docker (NixOS has no `dpkg-buildpackage`,
`rpmbuild`, or `makepkg`) and mirror the CI jobs step for step — the same
container, the same packaging rules — so a build that works here predicts
what CI builds. They test a **locally built** package rather than a
released one on purpose: needing a release to find out whether a release
is worth cutting is how the libturbojpeg bug shipped.

Once a release *is* cut, `--release` runs the complementary check against
the artifact users actually download — it fetches the package from the
GitHub release (latest by default, or a given tag) into
`dist/release-<tag>/` and installs that instead. Run `--clean` first so
the fresh guest actually installs it:

```sh
./scripts/vmtest/debian.sh --clean && ./scripts/vmtest/debian.sh --release v0.1.1
```

Needs the `gh` CLI. The `>> package under test:` line always says which
artifact — and which provenance — the VM got.

Each VM boots with the package staged, installs it via the distro's own
package manager, and leaves a breadcrumb:

```sh
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -p 2222 debian@localhost 'cat /var/log/veiland-install.log'
```

| distro | VM script   | SSH port | guest user |
|--------|-------------|----------|------------|
| Debian | `debian.sh` | 2222     | `debian`   |
| Fedora | `fedora.sh` | 2223     | `fedora`   |
| Arch   | `arch.sh`   | 2224     | `arch`     |

Password is `veiland` in every guest — for the console login, and because
veiland is a lock screen, it is also what you type to unlock. Ports differ
so all three can run at once.

`--clean` drops the guest's disk and forces a reprovision, keeping the
downloaded cloud image. **You need it after rebuilding a package**:
cloud-init only applies its config on an instance's *first* boot, so an
already-provisioned guest will happily ignore the new package and sit
there with the old one.

```sh
./scripts/vmtest/debian.sh --clean && ./scripts/vmtest/debian.sh
```

## Seeing it lock

The install check runs headless. To watch the locker actually work, use
the QEMU window, not SSH — veiland needs a real Wayland session.

1. Log in on the console (`debian` / `veiland`).
2. `sway`
3. `Super+Enter` for a terminal.
4. `glxinfo -B | grep renderer` — **check this first** (see below).
5. `veiland`, then unlock by typing the password.

Locked out? The lock screen is doing its job. Kill it from the host:

```sh
ssh -o StrictHostKeyChecking=no -o UserKnownHostsFile=/dev/null \
    -p 2222 debian@localhost 'pkill veiland'
```

## What these do not prove

**The GPU is not a real GPU.** The guests get `virtio-gpu-gl` (virgl),
which forwards GL to the host card. On a single-GPU host that is as close
as a VM gets — passthrough would take the card away from the host
entirely. So:

- `renderer: virgl` — GL is reaching the host GPU. veiland's DMA-BUF path
  has a real chance, and in practice works.
- `renderer: llvmpipe` — software rasterization, no GPU. `eglCreateImage`
  will likely reject the imported dmabuf, the core will close the plugin's
  socket and draw its fallback. **The session still locks** (that path is
  CPU-side), but every plugin layer comes up blank. That is the VM's
  limitation, not a veiland bug — do not go hunting for one.

Either way this is not the Mesa or the driver a user runs, so a *passing*
graphical test here is encouraging, not conclusive. The dmabuf path's real
validation is bare metal, on the two dev boxes.

**Without `/dev/kvm` everything is painfully slow.** The guest CPU is
interpreted (TCG): it boots, but a compositor under emulation is slow
enough that a graphical verdict cannot be trusted. The scripts detect
`/dev/kvm` and switch to hardware acceleration automatically — there is
nothing to configure, but on an AMD host you may need **SVM enabled in the
BIOS** (`SVM disabled (by BIOS) in MSR_VM_CR` in `dmesg` is the tell;
`/proc/cpuinfo` advertising the `svm` flag is *not* proof it is on).

**There is no Ubuntu VM, deliberately.** Ubuntu installs the same `.deb`
as Debian, so a VM would re-prove what the Debian VM already proves. The
one genuinely Ubuntu-specific question — can it satisfy the package's
dependencies — is a container question, and CI answers it. Note the `.deb`
is built on Debian 13, so `dpkg-shlibdeps` stamps `libc6 (>= 2.39)`: it
installs on Ubuntu 24.04+, and cannot install on 22.04 (glibc 2.35).

**NixOS is not here either.** The flake builds and tests it directly, and
`nix flake check` covers what these scripts would.

## Requirements

`qemu`, `cloud-utils` (for `cloud-localds`), `docker`, `python3`, and an
SSH key at `~/.ssh/id_ed25519.pub` or `id_rsa.pub`. On NixOS these come
from the host config, not this repo's dev shell — they are for testing the
*packages*, and a package test should not depend on the Nix build.

Cloud images and guest disks are cached in
`~/.cache/veiland-vmtest/<distro>/`. Delete that directory to reclaim the
space; the next run re-downloads.

## How the package reaches the guest

cloud-init cannot read a file on the host, so each script starts a
throwaway HTTP server on the artifact's directory and the guest fetches
from `10.0.2.2` —
which is always the host, as seen from QEMU's user-mode network. No
virtiofs, no 9p, no scp step to remember. The server dies with the script.
