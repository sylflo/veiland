// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    ffi::OsStr,
    os::{
        fd::{AsRawFd, OwnedFd},
        unix::process::CommandExt,
    },
    path::Path,
    process::Command,
};

use nix::{
    sys::socket::{AddressFamily, SockFlag, SockType, socketpair},
    unistd::Pid,
};

use super::HostError;

pub struct PluginProcess {
    pub child_pid: Pid,
    pub socket: OwnedFd,
}

/// Spawn a plugin binary via socketpair + `Command` (fork + exec).
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

    let mut cmd = Command::new(binary);

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
    // those will fail at exec anyway, but argv[0] being something
    // sensible-ish makes the failure log readable.
    cmd.arg0(
        binary
            .file_name()
            .unwrap_or_else(|| OsStr::new(name_for_log)),
    );

    // Command inherits the parent environment and adds these on top.
    // argv, argv[0], and the env block are all assembled here in the
    // parent, before the fork — nothing below allocates in the child.
    cmd.env("VEILAND_PLUGIN_SOCKET", "3");
    if let Some(config) = config_json {
        cmd.env("VEILAND_PLUGIN_CONFIG", config);
    }

    // SAFETY (pre_exec contract): the closure runs in the forked child
    // of a multithreaded process, so it must be async-signal-safe — no
    // allocation, no locks, no Rust panic infrastructure. It performs
    // exactly one fcntl or dup2, both async-signal-safe; the rest of
    // the child-side glue between fork and exec is std's, maintained
    // to the same rule.
    //
    // The closure move-captures the OwnedFd so the parent's copy stays
    // open across the fork; when `cmd` drops on return, the parent's
    // copy closes — the child's descriptor at fd 3 is unaffected.
    unsafe {
        cmd.pre_exec(move || {
            let fd = plugin_fd.as_raw_fd();
            let rc = if fd == 3 {
                // dup2(3, 3) is a no-op that would leave FD_CLOEXEC
                // set, silently closing the socket at exec. Clear the
                // flag in place instead.
                nix::libc::fcntl(3, nix::libc::F_SETFD, 0)
            } else {
                // dup2 creates a fresh descriptor without CLOEXEC, so
                // fd 3 survives exec; the CLOEXEC original closes then.
                nix::libc::dup2(fd, 3)
            };
            if rc < 0 {
                // last_os_error() reads errno without allocating (unlike
                // io::Error::new). Command relays this to the parent as
                // a failed spawn().
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }

    // spawn() surfaces fork failures, pre_exec errors, and exec
    // failures (missing or non-executable binary) synchronously — the
    // child reports back over an internal CLOEXEC pipe. No exit-127
    // sentinel, no zombie to reap on the error path.
    let child = cmd.spawn().map_err(HostError::Io)?;

    Ok(PluginProcess {
        // Dropping the std Child handle neither kills nor reaps the
        // process; teardown_one_plugin owns the waitpid/kill lifecycle
        // through this Pid.
        child_pid: Pid::from_raw(child.id() as i32),
        socket: host_fd,
    })
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
    /// Validates that socketpair + Command spawn compiles and runs
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

    /// Spawning a path that doesn't exist: exec fails in the child,
    /// Command relays the error to the parent over its internal pipe,
    /// and spawn_plugin returns Err synchronously — no child is left
    /// behind to reap, no exit-127 sentinel.
    #[test]
    fn spawn_nonexistent_returns_err() {
        let result = spawn_plugin(
            Path::new("/nonexistent/veiland-test-binary"),
            "nonexistent",
            None,
        );

        match result {
            Err(HostError::Io(e)) => {
                assert_eq!(e.kind(), std::io::ErrorKind::NotFound);
            }
            Err(other) => panic!("expected Io(NotFound), got {:?}", other),
            Ok(_) => panic!("expected spawn of a nonexistent binary to fail"),
        }
    }
}
