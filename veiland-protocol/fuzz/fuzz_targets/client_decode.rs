// SPDX-License-Identifier: GPL-3.0-or-later

//! Fuzz `ClientMessage::decode` (plugin -> host). Every byte a plugin
//! sends is untrusted input; decode must never panic, only return
//! `Ok` or a `ProtocolError`.

#![no_main]

use libfuzzer_sys::fuzz_target;
use veiland_protocol::ClientMessage;

fuzz_target!(|data: &[u8]| {
    let _ = ClientMessage::decode(data);
});
