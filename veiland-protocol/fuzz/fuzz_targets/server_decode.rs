// SPDX-License-Identifier: GPL-3.0-or-later

//! Fuzz `ServerMessage::decode` (host -> plugin). The host is trusted,
//! but the codec is symmetric and a plugin decoding host messages must
//! also never panic on garbage. Decode returns `Ok` or a `ProtocolError`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use veiland_protocol::ServerMessage;

fuzz_target!(|data: &[u8]| {
    let _ = ServerMessage::decode(data);
});
