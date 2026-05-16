// SPDX-License-Identifier: GPL-3.0-or-later

//! Byte-level codec primitives and the protocol-version handshake.
//! See `docs/protocol.md` §3 (encoding primitives) and §5 (version).

use crate::error::ProtocolError;

/// Current protocol version. See `docs/protocol.md` §5.
pub const PROTOCOL_VERSION: u32 = 1;

/// Append the 4-byte version handshake. See `docs/protocol.md` §5.
pub fn write_version(out: &mut Vec<u8>) {
    write_u32_le(out, PROTOCOL_VERSION);
}

/// Read the 4-byte version handshake. The buffer must contain exactly four
/// bytes; any trailing data is rejected with `TrailingBytes`.
pub fn read_version(buf: &[u8]) -> Result<u32, ProtocolError> {
    let (v, rest) = read_u32_le(buf)?;
    if !rest.is_empty() {
        return Err(ProtocolError::TrailingBytes);
    }
    Ok(v)
}

pub(crate) fn read_u16_le(buf: &[u8]) -> Result<(u16, &[u8]), ProtocolError> {
    if buf.len() < 2 {
        return Err(ProtocolError::Truncated);
    }
    let arr: [u8; 2] = buf[0..2].try_into().unwrap();
    Ok((u16::from_le_bytes(arr), &buf[2..]))
}

pub(crate) fn read_u32_le(buf: &[u8]) -> Result<(u32, &[u8]), ProtocolError> {
    if buf.len() < 4 {
        return Err(ProtocolError::Truncated);
    }
    let arr: [u8; 4] = buf[0..4].try_into().unwrap();
    Ok((u32::from_le_bytes(arr), &buf[4..]))
}

pub(crate) fn read_u64_le(buf: &[u8]) -> Result<(u64, &[u8]), ProtocolError> {
    if buf.len() < 8 {
        return Err(ProtocolError::Truncated);
    }
    let arr: [u8; 8] = buf[0..8].try_into().unwrap();
    Ok((u64::from_le_bytes(arr), &buf[8..]))
}

pub(crate) fn read_i32_le(buf: &[u8]) -> Result<(i32, &[u8]), ProtocolError> {
    if buf.len() < 4 {
        return Err(ProtocolError::Truncated);
    }
    let arr: [u8; 4] = buf[0..4].try_into().unwrap();
    Ok((i32::from_le_bytes(arr), &buf[4..]))
}

pub(crate) fn read_i64_le(buf: &[u8]) -> Result<(i64, &[u8]), ProtocolError> {
    if buf.len() < 8 {
        return Err(ProtocolError::Truncated);
    }
    let arr: [u8; 8] = buf[0..8].try_into().unwrap();
    Ok((i64::from_le_bytes(arr), &buf[8..]))
}

pub(crate) fn read_str(buf: &[u8], max_len: u16) -> Result<(String, &[u8]), ProtocolError> {
    let (len, rest) = read_u16_le(buf)?; // length in bytes
    if len > max_len {
        return Err(ProtocolError::StringTooLong {
            max: max_len,
            actual: len,
        });
    }
    let len = len as usize;
    if rest.len() < len {
        return Err(ProtocolError::Truncated);
    }
    let (bytes, rest) = rest.split_at(len);
    let s = std::str::from_utf8(bytes).map_err(|_| ProtocolError::InvalidUtf8)?;
    Ok((s.to_string(), rest))
}

pub(crate) fn write_u16_le(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn write_u32_le(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn write_u64_le(out: &mut Vec<u8>, v: u64) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn write_i32_le(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn write_i64_le(out: &mut Vec<u8>, v: i64) {
    out.extend_from_slice(&v.to_le_bytes());
}

pub(crate) fn write_str(out: &mut Vec<u8>, s: &str, max_len: u16) -> Result<(), ProtocolError> {
    // byte length (UTF-8)
    let len = s.len();
    if len > max_len as usize {
        return Err(ProtocolError::StringTooLong {
            max: max_len,
            // careful: if len > u16::MAX this lies
            actual: len as u16,
        });
    }
    write_u16_le(out, len as u16);
    out.extend_from_slice(s.as_bytes());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn u16_roundtrip() {
        let mut out = Vec::new();
        write_u16_le(&mut out, 0x1234);
        let (v, rest) = read_u16_le(&out).unwrap();
        assert_eq!(v, 0x1234);
        assert!(rest.is_empty());
        // Sanity: little-endian byte order on the wire.
        assert_eq!(out, vec![0x34, 0x12]);
    }

    #[test]
    fn u16_truncated() {
        let buf = [0u8; 1];
        assert_eq!(read_u16_le(&buf), Err(ProtocolError::Truncated));
    }

    #[test]
    fn u32_roundtrip() {
        let mut out = Vec::new();
        write_u32_le(&mut out, 0xdeadbeef);
        let (v, rest) = read_u32_le(&out).unwrap();
        assert_eq!(v, 0xdeadbeef);
        assert!(rest.is_empty());
        assert_eq!(out, vec![0xef, 0xbe, 0xad, 0xde]);
    }

    #[test]
    fn u32_truncated() {
        let buf = [0u8; 3];
        assert_eq!(read_u32_le(&buf), Err(ProtocolError::Truncated));
    }

    #[test]
    fn u64_roundtrip() {
        let mut out = Vec::new();
        write_u64_le(&mut out, 0x0123_4567_89ab_cdef);
        let (v, rest) = read_u64_le(&out).unwrap();
        assert_eq!(v, 0x0123_4567_89ab_cdef);
        assert!(rest.is_empty());
    }

    #[test]
    fn u64_truncated() {
        let buf = [0u8; 7];
        assert_eq!(read_u64_le(&buf), Err(ProtocolError::Truncated));
    }

    #[test]
    fn i32_roundtrip_negative() {
        let mut out = Vec::new();
        write_i32_le(&mut out, -42);
        let (v, rest) = read_i32_le(&out).unwrap();
        assert_eq!(v, -42);
        assert!(rest.is_empty());
    }

    #[test]
    fn i32_truncated() {
        let buf = [0u8; 3];
        assert_eq!(read_i32_le(&buf), Err(ProtocolError::Truncated));
    }

    #[test]
    fn i64_roundtrip_negative() {
        let mut out = Vec::new();
        write_i64_le(&mut out, i64::MIN);
        let (v, rest) = read_i64_le(&out).unwrap();
        assert_eq!(v, i64::MIN);
        assert!(rest.is_empty());
    }

    #[test]
    fn i64_truncated() {
        let buf = [0u8; 7];
        assert_eq!(read_i64_le(&buf), Err(ProtocolError::Truncated));
    }

    #[test]
    fn remaining_buffer_threaded_correctly() {
        // Write two values back-to-back, then thread the slice through two reads.
        let mut out = Vec::new();
        write_u16_le(&mut out, 0x0102);
        write_u32_le(&mut out, 0x0a0b0c0d);
        let (a, rest) = read_u16_le(&out).unwrap();
        let (b, rest) = read_u32_le(rest).unwrap();
        assert_eq!(a, 0x0102);
        assert_eq!(b, 0x0a0b0c0d);
        assert!(rest.is_empty());
    }

    #[test]
    fn str_roundtrip() {
        let mut out = Vec::new();
        write_str(&mut out, "hello", 64).unwrap();
        let (s, rest) = read_str(&out, 64).unwrap();
        assert_eq!(s, "hello");
        assert!(rest.is_empty());
        // Byte-level sanity: u16 LE length 5, then ASCII.
        assert_eq!(out, vec![0x05, 0x00, b'h', b'e', b'l', b'l', b'o']);
    }

    #[test]
    fn str_roundtrip_empty() {
        let mut out = Vec::new();
        write_str(&mut out, "", 64).unwrap();
        let (s, rest) = read_str(&out, 64).unwrap();
        assert_eq!(s, "");
        assert!(rest.is_empty());
        assert_eq!(out, vec![0x00, 0x00]);
    }

    #[test]
    fn str_roundtrip_multibyte_utf8() {
        // "héllo" is 6 bytes in UTF-8 (é is 0xc3 0xa9), not 5.
        let mut out = Vec::new();
        write_str(&mut out, "héllo", 64).unwrap();
        let (s, rest) = read_str(&out, 64).unwrap();
        assert_eq!(s, "héllo");
        assert!(rest.is_empty());
        // First two bytes are the u16 LE length: 6.
        assert_eq!(out[0..2], [0x06, 0x00]);
    }

    #[test]
    fn str_length_prefix_truncated() {
        let buf = [0x00u8]; // only 1 byte of the 2-byte length prefix
        assert_eq!(read_str(&buf, 64), Err(ProtocolError::Truncated));
    }

    #[test]
    fn str_claims_more_than_buffer() {
        // length = 5, but only 3 payload bytes follow.
        let buf = [0x05, 0x00, b'a', b'b', b'c'];
        assert_eq!(read_str(&buf, 64), Err(ProtocolError::Truncated));
    }

    #[test]
    fn str_too_long_on_read() {
        // length = 100, max_len = 64. Should reject before allocating.
        let buf = [0x64, 0x00];
        assert_eq!(
            read_str(&buf, 64),
            Err(ProtocolError::StringTooLong {
                max: 64,
                actual: 100
            })
        );
    }

    #[test]
    fn str_invalid_utf8() {
        // length = 1, then a byte that's not valid UTF-8 on its own.
        let buf = [0x01, 0x00, 0xff];
        assert_eq!(read_str(&buf, 64), Err(ProtocolError::InvalidUtf8));
    }

    #[test]
    fn str_too_long_on_write() {
        let mut out = Vec::new();
        let too_long = "a".repeat(65);
        assert_eq!(
            write_str(&mut out, &too_long, 64),
            Err(ProtocolError::StringTooLong {
                max: 64,
                actual: 65
            })
        );
        // Failed write must not leave partial bytes in `out`.
        assert!(out.is_empty());
    }

    #[test]
    fn str_exactly_at_max() {
        // A string of exactly max_len bytes is accepted; max_len + 1 fails.
        let mut out = Vec::new();
        let s = "a".repeat(64);
        write_str(&mut out, &s, 64).unwrap();
        let (back, rest) = read_str(&out, 64).unwrap();
        assert_eq!(back, s);
        assert!(rest.is_empty());
    }

    #[test]
    fn version_roundtrip() {
        let mut out = Vec::new();
        write_version(&mut out);
        assert_eq!(read_version(&out), Ok(PROTOCOL_VERSION));
        // Byte-level sanity: four little-endian bytes of PROTOCOL_VERSION.
        assert_eq!(out, vec![0x01, 0x00, 0x00, 0x00]);
    }

    #[test]
    fn version_truncated() {
        let buf = [0u8; 3];
        assert_eq!(read_version(&buf), Err(ProtocolError::Truncated));
    }

    #[test]
    fn version_trailing_bytes() {
        // Four valid bytes for the version, plus one extra.
        let buf = [0x01, 0x00, 0x00, 0x00, 0xaa];
        assert_eq!(read_version(&buf), Err(ProtocolError::TrailingBytes));
    }
}
