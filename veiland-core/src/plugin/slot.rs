// SPDX-License-Identifier: GPL-3.0-or-later

//! `PluginSlot` — one entry in the host's plugin vec. Wraps the
//! runtime `PluginState` (texture, connection, last-seen Hello)
//! with the static config metadata from the user's `config.toml`
//! (name, binary path, z_index, optional region).
//!
//! Why two layers: `PluginState` evolves per frame (texture swaps
//! on every Buffer, current_buffer_id moves with BufferReleased);
//! the config metadata is set once at spawn and never touched
//! again. Keeping them in separate types makes the static/dynamic
//! split explicit and stops the per-frame state from accidentally
//! depending on config fields it shouldn't.
//!
//! `name` exists on both layers and refers to two different
//! things: `slot.name` is what the user wrote in config.toml;
//! `slot.state.name` is what the plugin announced in Hello. Logs
//! prefer the config name (it's how the user identifies the
//! plugin); a mismatch is a future debugging clue.

use std::path::PathBuf;

use nix::unistd::Pid;

use crate::config::Region;

use super::state::PluginState;

pub struct PluginSlot {
    /// Runtime state — the parts that change per frame.
    pub state: PluginState,
    /// Child PID for teardown (Shutdown / SIGTERM / SIGKILL).
    pub pid: Pid,

    // --- Static config metadata, set at spawn, never mutated. ---
    /// Name from `[[plugin]] name = ...`. Used in logs.
    pub name: String,
    /// Binary path from `[[plugin]] binary = ...`. Used in logs.
    /// Not stat'd at config-load time (see config.rs).
    pub binary: PathBuf,
    /// z-index from `[[plugin]] z_index = ...`. The host sorts by
    /// this once at startup; ties keep config-file order (stable
    /// sort). After sort, `plugins[0]` is the bottom layer.
    pub z_index: i32,
    /// Optional region from `[[plugin]] region = ...`. `None` means
    /// "fill the whole lock surface." Resolved into clip-space
    /// vertices at composite time (M6 step 3).
    pub region: Option<Region>,
    /// Which output this instance is for. Matches one of the
    /// `LockSurface.name` strings (xdg_output.name). Used in logs
    /// here in step 2; carried to the plugin via `Configure.output_name`
    /// in step 3.
    pub output_name: String,
}
