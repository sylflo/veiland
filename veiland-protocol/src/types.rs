// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared opaque-identifier types used inside variant payloads.
//! See `docs/protocol.md` §6.2.

use crate::codec::{read_u32_le, read_u64_le, write_u32_le, write_u64_le};
use crate::error::ProtocolError;

/// DRM FourCC pixel format identifier. Opaque to the protocol; the
/// v1-allowlist check (`ARGB8888` only) lives in `Buffer::decode`.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Fourcc(pub u32);

/// DRM buffer modifier (tiling / compression scheme). Opaque to the protocol;
/// the v1-allowlist check (`LINEAR` or `INVALID`) lives in `Buffer::decode`.
#[derive(Debug, PartialEq, Clone, Copy)]
pub struct Modifier(pub u64);

impl Fourcc {
    /// DRM FourCC for 32-bit ARGB with 8 bits per channel — packs the
    /// ASCII bytes `'A','R','2','4'` into a little-endian u32.
    pub const ARGB8888: Fourcc = Fourcc(0x34325241);

    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        write_u32_le(out, self.0);
    }

    pub(crate) fn decode(buf: &[u8]) -> Result<(Self, &[u8]), ProtocolError> {
        let (v, buf) = read_u32_le(buf)?;
        Ok((Self(v), buf))
    }
}

impl Modifier {
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        write_u64_le(out, self.0);
    }

    pub(crate) fn decode(buf: &[u8]) -> Result<(Self, &[u8]), ProtocolError> {
        let (v, buf) = read_u64_le(buf)?;
        Ok((Self(v), buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fourcc_argb8888_roundtrip() {
        let mut out = Vec::new();
        Fourcc::ARGB8888.encode(&mut out);
        let (decoded, rest) = Fourcc::decode(&out).unwrap();
        assert_eq!(decoded, Fourcc::ARGB8888);
        assert!(rest.is_empty());
        // On the wire (little-endian u32), the bytes spell 'A','R','2','4' in ASCII.
        assert_eq!(out, vec![b'A', b'R', b'2', b'4']);
    }

    #[test]
    fn fourcc_truncated() {
        let buf = [0u8; 3];
        assert_eq!(Fourcc::decode(&buf), Err(ProtocolError::Truncated));
    }

    #[test]
    fn modifier_roundtrip_invalid_sentinel() {
        // u64::MAX is the DRM "INVALID" modifier — the value v1 plugins use
        // when they don't have a specific tiling/compression scheme to declare.
        let v = Modifier(u64::MAX);
        let mut out = Vec::new();
        v.encode(&mut out);
        let (decoded, rest) = Modifier::decode(&out).unwrap();
        assert_eq!(decoded, v);
        assert!(rest.is_empty());
    }

    #[test]
    fn modifier_truncated() {
        let buf = [0u8; 7];
        assert_eq!(Modifier::decode(&buf), Err(ProtocolError::Truncated));
    }
}
