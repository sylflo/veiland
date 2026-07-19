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
use veiland_protocol::Configure;

use crate::config::{Region, RegionSpec};

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
    /// Optional region spec from `[[plugin]] region = ...`, kept
    /// unresolved (pixel or anchored form). `None` means "fill the whole
    /// lock surface." The anchored form is resolved against the surface
    /// size at each Configure; `resolved_region` caches the result.
    pub region_spec: Option<RegionSpec>,
    /// The `region_spec` resolved to absolute pixels against the current
    /// surface size, recomputed at every Configure (spawn + resize).
    /// `None` mirrors `region_spec = None` (full surface). This is what
    /// the composite path and `configure_dims` read — they never see the
    /// unresolved spec, so anchored and pixel regions share one code
    /// path downstream.
    pub resolved_region: Option<Region>,
    /// Which output this instance is for. Matches one of the
    /// `LockSurface.name` strings (xdg_output.name). Used in logs
    /// here in step 2; carried to the plugin via `Configure.output_name`
    /// in step 3.
    pub output_name: String,
    /// The most recent `Configure` sent to this plugin. Stored so the
    /// host's periodic time tick (M11 step 2) can re-send a Configure
    /// with the same region/scale/output_name and only the time fields
    /// bumped. `None` if no Configure has been sent yet (shouldn't
    /// happen in practice — every slot gets one at spawn).
    pub last_configure: Option<Configure>,
    /// Tenancy identity of the calloop source driving this slot's
    /// socket, minted by `register_plugin_source`. The source's
    /// closure captures the serial it was registered with; if hotplug
    /// later reuses the same `(output, plugin)` indices for a new
    /// plugin while the old source is still registered, the serials
    /// no longer match and `drive_plugin` removes the stale source
    /// instead of polling the new tenant's socket. `None` until the
    /// source is registered.
    pub source_serial: Option<u64>,
}
