// SPDX-License-Identifier: GPL-3.0-or-later

//! Wire-format types and codec for the veiland host/plugin protocol.
//! The spec is `docs/protocol.md`; if this crate and the spec disagree,
//! the spec wins and the code is a bug.

#![deny(unsafe_code)]

mod client;
mod codec;
mod error;
mod server;
mod types;

pub use client::{Buffer, BufferDestroy, ClientMessage, Hello};
pub use codec::{
    HOST_CAP_FENCE_FD, HostCapabilities, PROTOCOL_VERSION, read_host_capabilities, read_version,
    write_host_capabilities, write_version,
};
pub use error::ProtocolError;
pub use server::{BufferReleased, Configure, ServerMessage};
pub use types::{Fourcc, Modifier};
