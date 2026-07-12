# Contributing to veiland

Thanks for your interest in veiland. This file covers how to get a dev
environment running, what CI expects, and what makes a PR easy to
merge. Veiland is maintained by one person and part of its value is
staying small, so the guidance below leans toward "talk first, then
code" for anything non-trivial.

**Want a new lock-screen effect? You probably don't need a PR.**
Veiland is built so that new visuals are plugins: standalone programs
that talk to the locker over a socket. You can write one, keep it in
your own repo, under any license, and nobody has to approve it. See
[Plugins](#plugins) below before opening a feature request for a new
effect.

## Before you write code

- **Open an issue first** for anything beyond a small fix — new
  dependencies, protocol or SDK changes, new reference plugins, or
  behavior changes. It avoids wasted work on both sides.
- **Security vulnerabilities do not go in the issue tracker.** See
  [SECURITY.md](SECURITY.md) for private reporting.

## Development setup

The Nix flake is the reference environment:

```sh
nix develop      # full toolchain + every system library
cargo build
```

Without Nix you need a stable Rust toolchain plus `pkg-config`, Mesa
(libgbm, libEGL, libGLESv2), libdrm, and libpam.

Actually *running* the locker needs a Wayland compositor implementing
`ext-session-lock-v1` (Hyprland and Sway are the tested ones) and the
`veiland` PAM service — see [PAM setup](README.md#pam-setup) in the
README. Run a development build with plugin binaries from the same
build tree by using paths in your config (e.g.
`binary = "target/debug/veiland-clock"`).

## Checks — what CI runs

CI is the flake: `nix flake check` runs rustfmt and clippy over the
whole workspace (warnings are errors), and `nix build` builds the
shipped crates and runs their test suites plus the library crates'
(everything except the stress test plugin, which clippy still
covers). Run both locally before pushing, or the plain-cargo
equivalents:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
```

There is also a fuzzing harness for the wire protocol (not part of
CI): `nix develop .#fuzz`, then `cargo fuzz run client_decode` from
`veiland-protocol/fuzz/`. If you find a crash, please turn the input
into a regression unit test in `veiland-protocol` alongside the fix.

## Pull request expectations

- **Small, focused commits.** One logical change per commit; a
  tightly-related change is one commit, not five.
- **No opportunistic refactors.** If a refactor is needed for your
  change, fine; otherwise propose it separately.
- **Ask before adding dependencies.** Keeping the dependency tree
  small is a feature. New crates need justification in the issue or
  PR description.
- **Conventions:**
  - SPDX identifier at the top of source files
    (`// SPDX-License-Identifier: GPL-3.0-or-later`), no long GPL
    preamble headers.
  - GLSL lives in byte-string literals (`b"..."`). Shader source and
    comments are ASCII-only — no em dashes or smart quotes.
  - **Never panic on plugin input.** Nothing a plugin sent — no
    message field, fd, size, or stride — may reach an `.unwrap()`,
    `.expect()`, or `assert!` in the core. Validate first; on bad
    input, close that plugin's socket, draw a fallback, and keep the
    locker running.
  - Close every fd received from a plugin when done with it.
  - Tag log lines with the plugin name when they concern plugin
    behavior.

**Security-sensitive paths get a stricter review.** Anything touching
the password buffer, PAM, keyboard input routing, the unlock decision,
or the dmabuf import path is held to a higher bar: prefer
obviously-correct code over clever code, and expect the review to walk
through it line by line. The trust boundaries are documented in
[`docs/architecture.md`](docs/architecture.md) — a PR that blurs them
(e.g. forwards any keyboard data to a plugin, however indirect) will
be declined regardless of the feature it enables.

## Plugins

### Writing your own — no PR needed

A plugin is a standalone program: it can live in your own repository
and use any license, because it communicates with the locker over a
socket rather than linking against it. Start with:

- [`docs/plugin-api.md`](docs/plugin-api.md) — the SDK
  (`veiland-plugin`, `veiland-text`) with reading order through the
  reference plugins.
- [`docs/plugin-authoring-claude.md`](docs/plugin-authoring-claude.md)
  — drop-in context if you write the plugin with an AI assistant.
- [`docs/protocol.md`](docs/protocol.md) — the wire format, if you're
  not using the Rust SDK.

### Contributing a reference plugin to this repo

The bar for `plugins/` is higher than "it works": a reference plugin
should have broad appeal as a lock-screen element, add little or no
new dependency weight, and use the reference SDK idiomatically
(`plugins/gradient` is the canonical shape — copy it). Plugin-specific
rules that come up in every review:

- **Premultiplied alpha.** The compositor blends with
  `ONE / ONE_MINUS_SRC_ALPHA`; shaders must emit RGB already
  multiplied by alpha or transparent edges grow halos.
- **Never panic on a host message.** The same untrusted-input rule as
  the core, in the other direction: a malformed or unexpected server
  message ends the plugin cleanly, it doesn't crash it.
- **Respect the assigned region.** Render into the configured buffer
  size; don't assume fullscreen.
- ASCII-only shader source, as above.

Open an issue describing the plugin before building it — "would this
be accepted?" is a cheap question.

### Protocol and SDK changes

These are one-way doors; always start with an issue.

- **The spec wins.** `docs/protocol.md` is authoritative over the
  implementation in `veiland-protocol/`. A protocol change updates the
  spec in the same PR.
- **Backwards compatible only.** Adding optional fields to a message
  is fine; removing or repurposing existing ones is not.
- **The SDK stays imperative.** `veiland-plugin` exposes primitives
  the plugin author drives (`Connection`, `FramePacer`, `DmaBuffer`),
  not a framework that calls hooks. New helpers should follow that
  shape.

## License

Veiland is GPL-3.0-or-later; contributions to this repository are
accepted under the same license. Out-of-tree plugins are your own
work and can be licensed however you like.
