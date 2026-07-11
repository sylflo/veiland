// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-side plugin lifecycle helpers: spawn one plugin instance
//! (socketpair + handshake + Hello + initial Configure) and tear one
//! down (Shutdown → grace → SIGTERM → SIGKILL).
//!
//! These sit one layer above `spawn.rs` (the raw socketpair + spawn):
//! `spawn.rs` produces a `PluginProcess`, and `try_spawn_one` drives the
//! protocol handshake on top of it to produce a fully-connected
//! `PluginSlot`. They live in the `plugin` module because they are the
//! host's side of the plugin protocol; `app/` calls them through the
//! `plugin::` re-exports. Moved verbatim from main.rs; no logic change.

use std::path::{Path, PathBuf};
use std::time::Duration;

use khronos_egl as egl;

use veiland_protocol::{ClientMessage, Configure, HostCapabilities};

use crate::config;

use super::spawn::PluginProcess;
use super::{HostConnection, HostError, PluginSlot, PluginState, ReceivedFds, spawn_plugin};

/// Receive deadline for the spawn window: the two blocking reads in
/// `connect_spawned` (version request, then `Hello`) must complete
/// within this budget or the spawn fails and the child is killed.
/// These are the only reads the host ever does without calloop
/// readiness, and they run on the main thread — an unbounded wait here
/// freezes the locked screen, keyboard dispatch included (the threat
/// model's "time-out silent plugins"). The normal case is single-digit
/// milliseconds: `Connection::connect` is the first thing the SDK does,
/// before any GPU setup. 2 s leaves three orders of magnitude of margin
/// for a cold-cache exec on a slow disk, and bounds the worst case
/// (every configured plugin hung) at a few seconds of delay, once.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(2);

/// Resolve a config `binary` value to the path we actually `execv`.
///
/// A value containing a `/` (absolute like `/usr/bin/veiland-clock`, or
/// relative like `target/debug/veiland-clock`) is used verbatim — the
/// escape hatch for dev builds and unusual layouts. `execv` handles the
/// absolute case directly and the relative case against the core's cwd.
///
/// A bare name (no `/`, e.g. `veiland-clock`) is resolved so the shipped
/// examples and the README are portable across distros without hardcoding
/// a bindir:
///   1. Beside the locker itself: `dirname(current_exe())/<name>`. On every
///      packaged install the plugins ship in the same bindir as `veiland`
///      (verified on NixOS: both in the same `/nix/store/.../bin`; on
///      Debian/Arch both in `/usr/bin`). This is preferred so a bare name
///      means "the plugin that shipped with *this* veiland", not "whatever
///      is first on `$PATH`".
///   2. `$PATH` fallback: the first `<dir>/<name>` that is a regular file.
///      Covers dev shells with `target/debug` on `$PATH`.
///
/// Resolution runs in the parent, before the spawn — the post-fork child
/// is async-signal-safe-only and cannot stat the filesystem or read the
/// env. Whatever this returns is a concrete path containing a `/`, so
/// `Command` execs exactly one file, chosen by a rule the core controls
/// (a bare name would trigger `Command`'s `execvp`-style `$PATH` search,
/// which we never rely on). Returns `BinaryNotFound` if a bare name
/// matches nothing; the caller treats that as a non-fatal per-plugin
/// spawn failure.
fn resolve_binary(binary: &Path) -> Result<PathBuf, HostError> {
    use std::os::unix::ffi::OsStrExt;

    // A `/` anywhere => the author means an exact path; don't second-guess it.
    if binary.as_os_str().as_bytes().contains(&b'/') {
        return Ok(binary.to_path_buf());
    }

    // An empty `binary` has no `/` but must not be exec'd as "".
    if binary.as_os_str().is_empty() {
        return Err(HostError::BinaryNotFound(String::new()));
    }

    // 1. Beside the locker. current_exe() may fail (e.g. the exe was
    //    unlinked); if so, skip this step and fall through to $PATH rather
    //    than aborting the spawn.
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        let candidate = dir.join(binary);
        if candidate.is_file() {
            return Ok(candidate);
        }
    }

    // 2. $PATH, first regular-file match wins.
    if let Some(path) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&path) {
            if dir.as_os_str().is_empty() {
                continue;
            }
            let candidate = dir.join(binary);
            if candidate.is_file() {
                return Ok(candidate);
            }
        }
    }

    Err(HostError::BinaryNotFound(
        binary.to_string_lossy().into_owned(),
    ))
}

/// Does this plugin entry's `monitors` filter (if any) admit the
/// given output? `None` means "any output"; `Some(list)` means
/// "exactly the names in this list" (case-sensitive, exact match).
pub fn entry_matches_output(entry: &config::PluginEntry, output_name: &str) -> bool {
    match &entry.monitors {
        None => true,
        Some(list) => list.iter().any(|name| name == output_name),
    }
}

pub fn try_spawn_one(
    entry: &config::PluginEntry,
    output_name: &str,
    scale_120: u32,
    surface_size: Option<(u32, u32)>,
    host_capabilities: HostCapabilities,
    egl: &egl::Instance<egl::Static>,
    display: egl::Display,
) -> Result<PluginSlot, HostError> {
    // Serialise the plugin's [plugin.config] table to JSON if present.
    // Failure here is a host bug (every TOML value round-trips through
    // serde_json cleanly); we log and proceed with no config rather
    // than refusing to spawn the plugin, since the plugin may have
    // sensible defaults.
    let config_json = entry
        .config
        .as_ref()
        .and_then(|v| match serde_json::to_string(v) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!(
                    "veiland-core: plugin {:?}: failed to serialise [plugin.config] to JSON: {} \
                     — spawning without VEILAND_PLUGIN_CONFIG",
                    entry.name, e
                );
                None
            }
        });
    // Resolve a bare `binary` name (no `/`) to a concrete path before we
    // spawn — the child can't stat the filesystem. A path with a `/` passes
    // through unchanged. On BinaryNotFound this returns early; the caller
    // logs it and leaves the plugin's layer empty (non-fatal).
    let resolved_binary = resolve_binary(&entry.binary)?;
    if resolved_binary != entry.binary {
        eprintln!(
            "veiland-core: plugin {:?}: resolved binary {:?} -> {:?}",
            entry.name, entry.binary, resolved_binary
        );
    }
    let process = spawn_plugin(&resolved_binary, &entry.name, config_json.as_deref())?;
    let pid = process.child_pid;
    // From here on a child process exists; any failure below must not
    // leave it behind. A well-behaved plugin exits when the socket
    // closes but lingers as a zombie (teardown_one_plugin only ever
    // sees slots that connected); a hung one — exactly the RecvTimeout
    // case — would not exit at all.
    match connect_spawned(
        process,
        entry,
        output_name,
        scale_120,
        surface_size,
        host_capabilities,
        egl,
        display,
    ) {
        Ok(slot) => Ok(slot),
        Err(e) => {
            kill_unconnected_plugin(pid, &entry.name);
            Err(e)
        }
    }
}

/// The protocol half of `try_spawn_one`: handshake, `Hello`, state
/// machine, initial `Configure`. Split out so the caller can pair every
/// error with cleanup of the already-spawned child. Both blocking reads
/// here run under `HANDSHAKE_TIMEOUT` — see the constant's comment.
// One argument over clippy's limit: this is try_spawn_one's surface
// plus the spawned process, and inventing a struct to carry it would
// just move the eight names somewhere else.
#[allow(clippy::too_many_arguments)]
fn connect_spawned(
    process: PluginProcess,
    entry: &config::PluginEntry,
    output_name: &str,
    scale_120: u32,
    surface_size: Option<(u32, u32)>,
    host_capabilities: HostCapabilities,
    egl: &egl::Instance<egl::Static>,
    display: egl::Display,
) -> Result<PluginSlot, HostError> {
    let mut connection = HostConnection::from_fd(process.socket, host_capabilities);
    connection.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    connection.handshake()?;
    eprintln!("plugin {:?}: handshake ok", entry.name);

    // recv_message has already enforced "Hello carries no fds" at the
    // wire layer (any fd on a non-Buffer message is ProtocolViolation
    // there). We just need to reject a misbehaving plugin that sent
    // the wrong variant as its first message — never panic, plugin
    // input must not crash the locker.
    let (msg, _fds) = connection.recv_message()?;
    let (hello_name, hello_version) = match msg {
        ClientMessage::Hello(h) => (h.plugin_name, h.plugin_version),
        _ => {
            return Err(HostError::ProtocolViolation("first message was not Hello"));
        }
    };
    eprintln!(
        "plugin {:?}: says hello: {} v{}",
        entry.name, hello_name, hello_version
    );
    // The plugin has spoken. From here its socket is read on calloop
    // readiness only, so the deadline comes off — a timed-out runtime
    // read would misreport a merely-idle plugin as dead.
    connection.set_read_timeout(None)?;
    // Build PluginState and feed the Hello through handle_message so
    // the state machine records name/version through the canonical path.
    let mut state = PluginState::new(connection);
    state.handle_message(
        ClientMessage::Hello(veiland_protocol::Hello {
            plugin_name: hello_name.clone(),
            plugin_version: hello_version.clone(),
        }),
        ReceivedFds::None,
        egl,
        display,
    )?;

    // Initial Configure. Use the real surface size if the compositor
    // has reported it; otherwise fall back to 1080p. On a fresh lock
    // the size is not known at spawn time (the compositor sends it
    // asynchronously, after this), so the fallback covers the brief
    // window before the host resends Configure with the true size.
    // Picking 1080p for the fallback means a 4K plugin briefly renders
    // at 1080p-upscaled for ~one frame, then snaps to native on the
    // resend — visually the same as before this change until the
    // resend lands.
    let (region_w, region_h) = surface_size.unwrap_or((1920, 1080));
    let (time_unix_seconds, time_tz_offset_seconds) = current_time_for_configure();
    let initial_configure = Configure {
        region_x: 0,
        region_y: 0,
        region_w,
        region_h,
        scale_120,
        time_unix_seconds,
        time_tz_offset_seconds,
        output_name: output_name.to_string(),
    };
    state.connection.send_configure(initial_configure.clone())?;
    state.connection.send_frame_done()?;

    Ok(PluginSlot {
        state,
        pid: process.child_pid,
        name: entry.name.clone(),
        binary: entry.binary.clone(),
        z_index: entry.z_index,
        region: entry.region.clone(),
        output_name: output_name.to_string(),
        last_configure: Some(initial_configure),
    })
}

/// Best-effort cleanup of a child that never completed the handshake:
/// SIGKILL, then a bounded reap. No Shutdown message and no SIGTERM
/// grace — the protocol never came up, the child has no state worth
/// saving, and in the RecvTimeout case it is hung anyway. The reap
/// polls WNOHANG instead of blocking in `waitpid` because a child
/// stuck in uninterruptible sleep (a wedged GPU ioctl is the likely
/// cause of a handshake timeout) cannot die until the kernel releases
/// it, and blocking on it would recreate the exact main-thread hang
/// this path exists to escape. Giving up leaves a zombie: ugly in
/// `ps`, harmless to the locker.
fn kill_unconnected_plugin(pid: nix::unistd::Pid, name: &str) {
    use nix::sys::signal::{Signal, kill};
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};

    let _ = kill(pid, Signal::SIGKILL);
    let deadline = std::time::Instant::now() + Duration::from_millis(200);
    loop {
        match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {
                if std::time::Instant::now() >= deadline {
                    eprintln!(
                        "veiland-core: plugin {:?}: unconnected child (pid {}) not reaped \
                         after SIGKILL — leaving it",
                        name, pid
                    );
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(_) => {
                eprintln!(
                    "veiland-core: plugin {:?}: killed unconnected child (pid {})",
                    name, pid
                );
                return;
            }
            Err(e) => {
                eprintln!(
                    "veiland-core: plugin {:?}: waitpid on unconnected child (pid {}) \
                     failed: {} (continuing)",
                    name, pid, e
                );
                return;
            }
        }
    }
}

/// Snapshot the wall clock into `(unix_seconds, tz_offset_seconds)`,
/// the two fields a plugin needs to render a localised clock without
/// reading the system time itself. Computing `time_tz_offset_seconds`
/// from `chrono::Local` honours DST transitions automatically — the
/// plugin doesn't need to know the timezone, just the offset that
/// was in effect at this instant.
pub fn current_time_for_configure() -> (i64, i32) {
    let now = chrono::Local::now();
    (now.timestamp(), now.offset().local_minus_utc())
}

/// Wind down the plugin: send Shutdown, give it ~250ms to exit on its own,
/// then SIGTERM, then SIGKILL. Reaps the zombie. Best-effort — if any step
/// fails we log and continue, because at this point the host is exiting
/// anyway and refusing to exit would be worse than a leaked plugin.
pub fn teardown_one_plugin(slot: PluginSlot) {
    use nix::sys::signal::{Signal, kill};
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};

    // Shutdown was already sent in the loop above; here we just wait.

    // Grace period. The spec says "implementation-defined"; 250ms is
    //    enough for a well-behaved plugin to exit and short enough that
    //    a session-unlock doesn't feel laggy.
    let grace = Duration::from_millis(250);
    let deadline = std::time::Instant::now() + grace;
    loop {
        match waitpid(slot.pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {
                if std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(_) => {
                eprintln!("teardown: plugin {:?} exited cleanly", slot.name);
                return;
            }
            Err(e) => {
                eprintln!(
                    "teardown: plugin {:?} waitpid failed: {} (continuing)",
                    slot.name, e
                );
                return;
            }
        }
    }

    // 3. SIGTERM, brief wait.
    eprintln!(
        "teardown: plugin {:?} did not exit in {}ms, sending SIGTERM",
        slot.name,
        grace.as_millis()
    );
    let _ = kill(slot.pid, Signal::SIGTERM);
    std::thread::sleep(Duration::from_millis(100));
    if let Ok(status) = waitpid(slot.pid, Some(WaitPidFlag::WNOHANG))
        && !matches!(status, WaitStatus::StillAlive)
    {
        eprintln!("teardown: plugin {:?} reaped after SIGTERM", slot.name);
        return;
    }

    // 4. SIGKILL, reap, done.
    eprintln!(
        "teardown: plugin {:?} still alive, sending SIGKILL",
        slot.name
    );
    let _ = kill(slot.pid, Signal::SIGKILL);
    let _ = waitpid(slot.pid, None);
}
