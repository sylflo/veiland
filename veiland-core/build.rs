// SPDX-License-Identifier: GPL-3.0-or-later
//
// Bakes a git revision string into the binary for `veiland --version`.
//
// Why this exists: `CARGO_PKG_VERSION` only reflects the `version` field in
// Cargo.toml, which changes at release time. Someone who installs veiland by
// pointing their flake at `master` (or a specific commit to test a fix) is many
// commits past the last tag, but a bare version would still say "0.1.0". The
// git rev disambiguates *which* build they are actually running — the first
// thing you want in a bug report.
//
// Resolution order for the rev (first hit wins):
//   1. $VEILAND_GIT_REV set at build time. The flake sets this from `self.rev`
//      / `self.dirtyRev` because a `nix build` copies the working tree WITHOUT
//      `.git/` into the sandbox, so the `git` command below finds no repo.
//      This is the path that makes a flake-off-master build carry its commit.
//   2. `git rev-parse` in the source tree. Covers a plain `cargo build` in the
//      dev shell, where `.git/` is present.
//   3. Empty. A release tarball (.deb/.rpm/AUR) has neither the env var nor a
//      `.git/`, so `--version` prints just the bare version — which is correct,
//      since those only ever build from a tagged release.

use std::process::Command;

fn main() {
    let rev = git_rev();
    // Read by `env!("VEILAND_GIT_REV")` in main.rs. Always emitted (possibly
    // empty) so the `env!` never fails to resolve.
    println!("cargo:rustc-env=VEILAND_GIT_REV={rev}");

    // Rebuild the version string when HEAD moves or the flake passes a new rev,
    // so `--version` never reports a stale commit from an earlier build.
    println!("cargo:rerun-if-env-changed=VEILAND_GIT_REV");
    println!("cargo:rerun-if-changed=../.git/HEAD");
}

/// The short revision, with a `-dirty` suffix when the working tree has
/// uncommitted changes. Empty string when the rev can't be determined.
fn git_rev() -> String {
    // 1. The flake (or any packager) can hand us the rev directly. It knows
    //    its own revision even inside the hermetic build sandbox.
    if let Ok(rev) = std::env::var("VEILAND_GIT_REV") {
        return rev;
    }

    // 2. Fall back to asking git, for a normal checkout / dev-shell build.
    let Some(short) = git(&["rev-parse", "--short", "HEAD"]) else {
        return String::new();
    };

    // `git status --porcelain` prints nothing for a clean tree, one line per
    // change otherwise — so a non-empty result means dirty.
    let dirty = git(&["status", "--porcelain"])
        .map(|s| !s.is_empty())
        .unwrap_or(false);

    if dirty {
        format!("{short}-dirty")
    } else {
        short
    }
}

/// Run a git command in the crate's source tree, returning trimmed stdout on
/// success. `None` on any failure (git missing, not a repo, non-zero exit) —
/// this is best-effort build metadata, never a build blocker.
fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    if !out.status.success() {
        return None;
    }
    Some(String::from_utf8_lossy(&out.stdout).trim().to_string())
}
