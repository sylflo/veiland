<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# veiland-protocol

Wire-format types and codec for the veiland host/plugin protocol.

This crate defines the Rust types that mirror the messages in
[`docs/protocol.md`](../docs/protocol.md), encodes them to bytes, decodes
bytes into them, and rejects malformed input with typed errors. The spec
is the source of truth: if this crate and the spec disagree, the spec
wins and the code is a bug.

## What this crate is not

`veiland-protocol` has no I/O. It does not open sockets, call `recvmsg`,
or handle file descriptors. The socket layer — including `SCM_RIGHTS` fd
passing for the `Buffer` message — lives in `veiland-core` (host) and
`veiland-plugin` (plugin helper), both of which depend on this crate.
Separating the codec from the socket keeps the codec testable without
any kernel involvement and concentrates all wire-format validation in
one auditable place.

The crate uses only `std` — no `serde`, no `bincode`. The wire format is
hand-rolled against the spec so non-Rust plugin implementations can
target the spec directly without depending on Rust-specific encodings.

## Layout

- `src/error.rs` — `ProtocolError` (the single error type returned by
  every decode path).
- `src/codec.rs` — byte-level primitives (`read_u16_le`, `write_str`,
  …) and the version handshake.
- `src/types.rs` — shared opaque-identifier types (`Fourcc`, `Modifier`).
- `src/client.rs` — plugin → host messages (`Hello`, `Buffer`,
  `BufferDestroy`, `ClientMessage`).
- `src/server.rs` — host → plugin messages (`Configure`, `FrameDone`,
  `BufferReleased`, `Shutdown`, `ServerMessage`).
- `src/lib.rs` — crate root; declares modules and re-exports the public
  API.

Tests live in `#[cfg(test)] mod tests` at the bottom of each module
alongside the code they test.
