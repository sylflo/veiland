# Security Policy

Veiland is a screen locker. A vulnerability here can mean an unlocked
session or a disclosed password, so reports are taken seriously and
handled privately until a fix ships. Thank you for looking.

## Supported versions

Only the **latest release** is supported. Security fixes ship as a new
release, not as backports to older versions. Please reproduce on the
latest release before reporting.

## How to report

**Please do not open a public issue for security problems.**

1. **Preferred:** use GitHub's private vulnerability reporting:
   [Report a vulnerability](https://github.com/sylflo/veiland/security/advisories/new).
   This keeps the report, discussion, and fix private until disclosure.
2. **Fallback:** email `veiland@sylvain-chateau.com` with
   `[veiland security]` in the subject.

In your report, include what you can of:

- The veiland version (the release tag, or the version reported by your
  package manager) and how you installed it (AUR, .deb, .rpm, Nix,
  source).
- Compositor and version (Hyprland, Sway, …) and GPU driver if relevant.
- Steps to reproduce, and what the impact is (e.g. "password visible
  in X", "session unlocks without valid credentials").
- A proof-of-concept plugin or config, if the issue involves the
  plugin protocol.

## What to expect

Veiland is maintained by one person, so timelines are honest rather
than corporate:

- **Acknowledgement** within **7 days**.
- An initial assessment (accepted / needs info / out of scope) within
  **14 days**.
- Fixes for confirmed vulnerabilities are prioritized over all other
  work, and a patched release is published with a GitHub security
  advisory crediting you (unless you prefer to stay anonymous).

Please allow up to **90 days** of coordinated disclosure before
publishing details. If a fix lands sooner, disclose as soon as the
patched release is out. There is no bug bounty — only credit and
gratitude.

## Scope

**In scope** (these are the guarantees veiland claims — breaking any of
them is a vulnerability):

- Unlocking the session without valid PAM credentials.
- Disclosure of the password or keystrokes to anything outside
  `veiland-core` (a plugin, a log, a file, another process) through
  veiland's own behavior. Attacks the kernel permits regardless of
  what veiland does (root, privileged debuggers) are carved out
  under out-of-scope below.
- A plugin achieving code execution in, or memory disclosure from,
  the core process (e.g. via the socket protocol or dmabuf import path).
- A plugin bypassing the core's `PR_SET_DUMPABLE` protection to read
  core memory (e.g. via `ptrace` or `/proc/<pid>/mem`) on a default
  configuration.
- A plugin drawing **over** the core-painted password UI, or otherwise
  spoofing the unlock interaction.
- Crashes in the core that are *triggerable by a plugin* — the session
  stays locked (the compositor enforces that), but a plugin being able
  to kill the locker is still a protocol-hardening bug.

**Out of scope:**

- Bugs in the compositor's `ext-session-lock-v1` implementation
  (report those to Hyprland/Sway/etc.). Veiland's fail-closed guarantee
  is inherited from the compositor.
- Core memory access when the user has opted out of the dumpability
  protection with `VEILAND_ALLOW_DUMP=1` (a debugging escape hatch),
  or via root, a privileged debugger, or a kernel bug.
- A plugin drawing a fake desktop *beneath* the password UI inside its
  own region — documented behavior; the core always paints the real
  password UI on top.
- Attacks requiring an already-compromised user account or root (an
  attacker who can edit `~/.config/veiland/config.toml` or replace
  plugin binaries already has your session).
- Denial of service against your own lock screen via your own config.
- Vulnerabilities in third-party plugins. Plugins are untrusted by
  design and cannot touch the auth path via the plugin protocol — but
  only install plugins you trust, since they run as your user.
- Physical attacks (DMA over Thunderbolt, cold boot, etc.).

## Hardening notes for users

- Install from the official packages or build from a tagged release.
- Treat third-party plugins like any program you run: they never
  receive your password over the plugin protocol, but they execute
  with your user's privileges — veiland hardens against same-user
  memory snooping without fully eliminating it, so only install
  plugins you trust.
- Don't run veiland with `VEILAND_ALLOW_DUMP=1` outside of debugging;
  it disables the protection that keeps the password buffer out of
  core dumps and same-user debuggers.
