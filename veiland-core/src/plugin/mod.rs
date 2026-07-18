// SPDX-License-Identifier: GPL-3.0-or-later

mod connection;
mod dmabuf;
mod error;
mod host_spawn;
mod slot;
mod spawn;
mod state;
mod sync;

pub use connection::{HostConnection, ReceivedFds};
pub use dmabuf::GL_TEXTURE_EXTERNAL_OES;
pub use error::HostError;
pub use host_spawn::{
    current_time_for_configure, entry_matches_output, kill_slot, teardown_one_plugin, try_spawn_one,
};
pub use slot::PluginSlot;
pub use spawn::spawn_plugin;
pub use state::PluginState;
pub use sync::{create_host_fence, release_fence, wait_fence};
