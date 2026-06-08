// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-side plugin lifecycle helpers: spawn one plugin instance
//! (socketpair + handshake + Hello + initial Configure) and tear one
//! down (Shutdown → grace → SIGTERM → SIGKILL).
//!
//! These sit one layer above `spawn.rs` (the raw socketpair/fork/exec):
//! `spawn.rs` produces a `PluginProcess`, and `try_spawn_one` drives the
//! protocol handshake on top of it to produce a fully-connected
//! `PluginSlot`. They live in the `plugin` module because they are the
//! host's side of the plugin protocol; `app/` calls them through the
//! `plugin::` re-exports. Moved verbatim from main.rs; no logic change.

use std::time::Duration;

use khronos_egl as egl;

use veiland_protocol::{ClientMessage, Configure, HostCapabilities};

use crate::config;

use super::{HostConnection, HostError, PluginSlot, PluginState, ReceivedFds, spawn_plugin};

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
    scale: u32,
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
    let process = spawn_plugin(&entry.binary, &entry.name, config_json.as_deref())?;
    let mut connection = HostConnection::from_fd(process.socket, host_capabilities);
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
        scale,
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
