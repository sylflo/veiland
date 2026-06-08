// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    ffi::CString,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::ffi::OsStrExt,
    },
    path::Path,
};

use nix::{
    sys::socket::{AddressFamily, SockFlag, SockType, socketpair},
    unistd::{ForkResult, Pid, execv, fork},
};

use super::HostError;

pub struct PluginProcess {
    pub child_pid: Pid,
    pub socket: OwnedFd,
}

/// Spawn a plugin binary via socketpair + fork + exec.
///
/// `config_json`, if `Some`, is exported to the child as
/// `VEILAND_PLUGIN_CONFIG`. The host derives this from the
/// `[plugin.config]` TOML table for this plugin entry (see
/// `host_spawn::try_spawn_one`). Plugins parse the
/// string however they like — JSON is the wire format because it's
/// strictly more compact in transitive dep cost than re-shipping TOML
/// to every plugin, and `serde_json::Value` is a sufficient target for
/// most plugin authors. `None` means the env var is not set; the
/// plugin should fall back to its own defaults.
pub fn spawn_plugin(
    binary: &Path,
    name_for_log: &str,
    config_json: Option<&str>,
) -> Result<PluginProcess, HostError> {
    let (host_fd, plugin_fd) = socketpair(
        AddressFamily::Unix,
        SockType::SeqPacket,
        None,
        SockFlag::SOCK_CLOEXEC,
    )?;

    // SAFETY: post-fork the child runs only async-signal-safe
    // operations before exec. The host is single-threaded at this
    // point in startup; revisit if threads are introduced earlier.
    let fork_result = unsafe { fork() }?;
    match fork_result {
        ForkResult::Parent { child } => {
            drop(plugin_fd);
            Ok(PluginProcess {
                child_pid: child,
                socket: host_fd,
            })
        }
        ForkResult::Child => {
            // SAFETY: only async-signal-safe operations until exec.
            // No allocations, no locks, no Rust panic infrastructure.
            // On any error, _exit(127) — we can't propagate to the parent.

            // Move the plugin's socket end to fd 3. dup2 creates a
            // fresh fd without CLOEXEC, so it survives exec.
            // nix 0.31's safe dup2 won't let us target a specific fd
            // number; libc::dup2 is the escape hatch we need.
            // SAFETY: raw syscall, no Rust invariants in play. dup2
            // closes whatever is at fd 3 and duplicates plugin_fd into
            // its place. The kernel handles all bookkeeping.
            if unsafe { nix::libc::dup2(plugin_fd.as_raw_fd(), 3) } < 0 {
                unsafe { nix::libc::_exit(127) };
            }

            // Build the CStrings for execv before any further fallible work.
            // CString::new fails only on interior nulls — never for real paths
            let path_cstring = match CString::new(binary.as_os_str().as_bytes()) {
                Ok(s) => s,
                Err(_) => unsafe { nix::libc::_exit(127) },
            };
            // Same drill for the optional plugin config. An interior
            // NUL byte in a JSON serialisation should never happen, but
            // if somehow it does we _exit rather than risk passing a
            // truncated string.
            let config_cstring = match config_json {
                Some(s) => match CString::new(s) {
                    Ok(c) => Some(c),
                    Err(_) => unsafe { nix::libc::_exit(127) },
                },
                None => None,
            };
            // argv[0] is the binary's filename — *not* the config name.
            // This makes `pgrep veiland` find every plugin (assuming
            // plugin authors follow the `veiland-<name>` binary
            // convention), regardless of what the user called the plugin
            // in their config. `name_for_log` is still used for host
            // log lines — that's the user's chosen name, which may
            // diverge from the binary name.
            //
            // Fall back to `name_for_log` if file_name() returns None
            // (only possible for paths like "/" or ending in "..");
            // those will fail at execv anyway, but argv[0] being something
            // sensible-ish makes the failure log readable.
            let argv0_bytes = binary
                .file_name()
                .map(|s| s.as_bytes())
                .unwrap_or_else(|| name_for_log.as_bytes());
            let argv0_cstring = match CString::new(argv0_bytes) {
                Ok(s) => s,
                Err(_) => unsafe { nix::libc::_exit(127) },
            };

            // SAFETY: set_var is `unsafe` in Rust 2024 because it can race
            // with other threads in a multi-threaded program. We are
            // single-threaded post-fork, so the race cannot occur.
            unsafe {
                std::env::set_var("VEILAND_PLUGIN_SOCKET", "3");
                if let Some(c) = config_cstring.as_ref() {
                    // OsStr::from_bytes builds a borrowed OsStr without
                    // re-allocating; set_var copies into its own
                    // env-block storage.
                    std::env::set_var(
                        "VEILAND_PLUGIN_CONFIG",
                        std::ffi::OsStr::from_bytes(c.to_bytes()),
                    );
                }
            }

            // execv replaces this process with the plugin binary.
            // Returns only on failure.
            let _ = execv(&path_cstring, &[argv0_cstring]);
            unsafe { nix::libc::_exit(127) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::sys::wait::{WaitStatus, waitpid};

    /// Locate `true` across distros. NixOS has empty /bin and /usr/bin
    /// (except /bin/sh); the system profile under /run/current-system
    /// is the stable path there. Standard Linux finds /bin/true first.
    fn find_true() -> &'static Path {
        for candidate in &[
            "/bin/true",
            "/usr/bin/true",
            "/run/current-system/sw/bin/true",
        ] {
            if Path::new(candidate).exists() {
                return Path::new(candidate);
            }
        }
        panic!("no `true` binary found in any standard location");
    }

    /// Smoke test: spawn `true` and confirm it exits cleanly.
    /// Validates that socketpair + fork + exec compiles and runs
    /// end-to-end. Does not validate fd 3 plumbing — that's covered
    /// by the gradient plugin end-to-end at step 6.
    #[test]
    fn spawn_true_exits_zero() {
        let process =
            spawn_plugin(find_true(), "true", None).expect("spawning `true` should succeed");

        let status =
            waitpid(process.child_pid, None).expect("waitpid should succeed on a known child");

        match status {
            WaitStatus::Exited(_, 0) => {}
            other => panic!("expected clean exit, got {:?}", other),
        }
    }

    /// Spawning a path that doesn't exist: the child reaches execv,
    /// execv fails, the child _exit(127)s. The parent sees a clean
    /// fork return then a child that exited 127.
    #[test]
    fn spawn_nonexistent_exits_127() {
        let process = spawn_plugin(
            Path::new("/nonexistent/veiland-test-binary"),
            "nonexistent",
            None,
        )
        .expect("fork itself should succeed even if exec target is missing");

        let status = waitpid(process.child_pid, None).expect("waitpid should succeed");

        match status {
            WaitStatus::Exited(_, 127) => {}
            other => panic!("expected exit 127, got {:?}", other),
        }
    }
}
