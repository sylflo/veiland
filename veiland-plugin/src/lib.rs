// SPDX-License-Identifier: GPL-3.0-or-later

//! Helper library for writing veiland plugins. Hides the socket dance,
//! the EGL/GBM setup, and protocol dispatch. See `docs/protocol.md`
//! for the wire protocol this crate implements the plugin side of.

mod buffer;
mod error;
mod render;
mod socket;
mod sync;

pub use buffer::DmaBuffer;
pub use error::PluginError;
pub use render::GbmEgl;
pub use socket::Connection;
pub use sync::SyncFence;
