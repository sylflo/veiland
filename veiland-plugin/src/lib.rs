// SPDX-License-Identifier: GPL-3.0-or-later

//! Helper library for writing veiland plugins. Hides the socket dance,
//! the EGL/GBM setup, and protocol dispatch. See `docs/protocol.md`
//! for the wire protocol this crate implements the plugin side of.

mod buffer;
mod config;
mod error;
pub mod gl;
mod lifecycle;
pub mod math;
mod render;
mod socket;
mod sync;

pub use buffer::DmaBuffer;
pub use config::load_config;
pub use error::PluginError;
pub use lifecycle::{Frame, FramePacer};
pub use math::{Rng, px_to_clip};
pub use render::GbmEgl;
pub use socket::Connection;
pub use sync::SyncFence;
