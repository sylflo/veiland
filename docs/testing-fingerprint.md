<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Testing fingerprint unlock without a reader

libfprint ships a **virtual reader**: fprintd treats a Unix socket as a
fingerprint device, and you "touch" it by writing commands to the socket. That
exercises the real path (fprintd + pam_fprintd + veiland's
`veiland-fingerprint` service) on a laptop with no sensor. nixpkgs builds
libfprint with all drivers, so this works on NixOS out of the box.

The unit tests (`cargo test -p veiland-core fingerprint`) cover the retry
policy and the conversation rules; this guide covers what they can't.

## 1. NixOS setup

```nix
services.fprintd.enable = true;

# The virtual reader. fprintd's unit sandboxes /tmp, so the socket lives
# in a RuntimeDirectory (root-owned: talk to it with sudo).
systemd.services.fprintd.environment.FP_VIRTUAL_DEVICE = "/run/fprintd-virtual/sock";
systemd.services.fprintd.serviceConfig.RuntimeDirectory = "fprintd-virtual";

# With fprintd on, every PAM service gets pam_fprintd by default. Keep it
# out of sudo, or sudo itself waits for the fake finger.
security.pam.services.sudo.fprintAuth = false;

# veiland's PAM services. The flake's NixOS module (services.veiland)
# does this for you; add it by hand if your system pins an older veiland.
security.pam.services.veiland.fprintAuth = false;
security.pam.services.veiland-fingerprint.text = ''
  auth required ${config.services.fprintd.package}/lib/security/pam_fprintd.so
'';
```

Rebuild, then check `/etc/pam.d/veiland` has no `pam_fprintd` line and
`/etc/pam.d/veiland-fingerprint` has only that one.

## 2. Talking to the reader

Put this in your shell (needs `socat`, e.g. `nix shell nixpkgs#socat`):

```sh
fp() { printf '%s' "$*" | sudo socat - UNIX-CONNECT:/run/fprintd-virtual/sock; }
```

- One command per connection.
- Use `printf`, not `echo`: the device doesn't strip the newline, so `echo`
  would make the finger id `good\n`.
- The socket only exists while fprintd has the device open. `fp SET_KEEP_ALIVE 1`
  keeps it after that.

## 3. Enroll a fake finger

In one terminal:

```sh
sudo fprintd-enroll -f right-index-finger "$USER"
```

In another, once it's waiting:

```sh
fp SET_KEEP_ALIVE 1
fp SCAN good    # 5 times; every stage must use the same id
```

`fprintd-list "$USER"` should now show the finger.

## 4. Lock inside a nested sway

Run veiland inside a nested compositor, so the lock only covers that window
and your real session stays usable to send scans:

```sh
sway &                            # or: nix shell nixpkgs#sway -c sway &
ls "$XDG_RUNTIME_DIR"/wayland-*   # the newest socket is the nested one
```

A test config with no plugins is enough (black screen + password box):

```toml
# ~/veiland-fp-test.toml
[fingerprint]
enabled = true
```

From the repo's dev shell:

```sh
WAYLAND_DISPLAY=wayland-2 VEILAND_CONFIG="$HOME/veiland-fp-test.toml" cargo run --bin veiland
```

Run it from your local graphical session, not over SSH: fprintd only lets
the active local session verify.

## 5. What to check

| Do (from your real session)              | Expect                                                         |
|------------------------------------------|----------------------------------------------------------------|
| nothing                                  | hint "Password or fingerprint" in the box                      |
| `fp SCAN good`                           | unlocks; log `fingerprint matched`                             |
| `fp SCAN bad`                            | red flash, still locked                                        |
| `fp SCAN bad` 6 times                    | hint disappears, log `too many fingerprint mismatches`; password still unlocks |
| type the password + Enter (no scan)      | unlocks right away, no wait for the reader                     |
| wait 30 s, then `fp SCAN good`           | still unlocks (veiland restarted the scan after the timeout)   |
| `enabled = false` in the test config     | no hint; `journalctl -u fprintd` shows no device claim         |
| remove `veiland-fingerprint`, rebuild    | log `PAM service is not installed`; password works             |

The last safety check: temporarily set the fingerprint service to
`auth sufficient ${pkgs.pam}/lib/security/pam_permit.so` and rebuild. veiland
**must not unlock**; it logs `succeeded without asking for a finger` and the
password stays the only way in. Restore the real line afterwards.

## 6. Clean up

Remove the `FP_VIRTUAL_DEVICE` / `RuntimeDirectory` lines (and the fake print:
`sudo fprintd-delete "$USER"`) when you're done. On a laptop with a real
reader, the virtual one would otherwise show up next to it.
