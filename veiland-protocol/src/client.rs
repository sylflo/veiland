// SPDX-License-Identifier: GPL-3.0-or-later

//! Plugin-to-host messages. See `docs/protocol.md` §6.

use crate::codec::{read_str, read_u16_le, read_u32_le, write_str, write_u16_le, write_u32_le};
use crate::error::ProtocolError;
use crate::types::{Fourcc, Modifier};

/// Plugin → host handshake message. See `docs/protocol.md` §6.1.
#[derive(Debug, PartialEq)]
pub struct Hello {
    pub plugin_name: String,
    pub plugin_version: String,
}

/// `Buffer` carries one dmabuf fd via `SCM_RIGHTS`. The fd is **not** part of
/// this struct or the encoded bytes — the socket layer pairs the fd received
/// in the cmsg with the `Buffer` message based on arrival order. This crate
/// has no I/O and never sees the fd.
///
/// See `docs/protocol.md` §6.2.
#[derive(Debug, PartialEq)]
pub struct Buffer {
    pub id: u32,
    pub width: u32,
    pub height: u32,
    pub format: Fourcc,
    pub modifier: Modifier,
    pub stride: u32,
    pub offset: u32,
}

/// Plugin tells the host it will no longer reuse this buffer id.
/// See `docs/protocol.md` §6.3.
#[derive(Debug, PartialEq)]
pub struct BufferDestroy {
    pub id: u32,
}

/// Any plugin-to-host message. See `docs/protocol.md` §6.
#[derive(Debug, PartialEq)]
pub enum ClientMessage {
    Hello(Hello),
    Buffer(Buffer),
    BufferDestroy(BufferDestroy),
}

impl ClientMessage {
    const TAG_HELLO: u16 = 0x0001;
    const TAG_BUFFER: u16 = 0x0002;
    const TAG_BUFFER_DESTROY: u16 = 0x0003;

    /// Encode one message to bytes. The encoded form is tag + variant payload.
    #[must_use = "encode returns a Result because a too-long string field can fail; handle the error"]
    pub fn encode(&self, out: &mut Vec<u8>) -> Result<(), ProtocolError> {
        match self {
            ClientMessage::Hello(hello) => {
                write_u16_le(out, Self::TAG_HELLO);
                hello.encode(out)?;
            }
            ClientMessage::Buffer(buffer) => {
                write_u16_le(out, Self::TAG_BUFFER);
                buffer.encode(out);
            }
            ClientMessage::BufferDestroy(buffer) => {
                write_u16_le(out, Self::TAG_BUFFER_DESTROY);
                buffer.encode(out);
            }
        }
        Ok(())
    }

    /// Decode one message from a byte buffer. Returns `TrailingBytes` if the
    /// buffer contains more than one message's worth of data.
    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        let (tag, buf) = read_u16_le(buf)?;
        let (msg, rest) = match tag {
            Self::TAG_HELLO => {
                let (hello, rest) = Hello::decode(buf)?;
                (ClientMessage::Hello(hello), rest)
            }
            Self::TAG_BUFFER => {
                let (buffer, rest) = Buffer::decode(buf)?;
                (ClientMessage::Buffer(buffer), rest)
            }
            Self::TAG_BUFFER_DESTROY => {
                let (buffer, rest) = BufferDestroy::decode(buf)?;
                (ClientMessage::BufferDestroy(buffer), rest)
            }
            other => return Err(ProtocolError::UnknownTag(other)),
        };
        if !rest.is_empty() {
            return Err(ProtocolError::TrailingBytes);
        }
        Ok(msg)
    }
}

impl Hello {
    #[must_use = "Hello::encode can fail if plugin_name or plugin_version exceeds its cap"]
    pub(crate) fn encode(&self, out: &mut Vec<u8>) -> Result<(), ProtocolError> {
        write_str(out, &self.plugin_name, 64)?;
        write_str(out, &self.plugin_version, 32)?;
        Ok(())
    }

    pub(crate) fn decode(buf: &[u8]) -> Result<(Self, &[u8]), ProtocolError> {
        let (plugin_name, buf) = read_str(buf, 64)?;
        let (plugin_version, buf) = read_str(buf, 32)?;
        Ok((
            Self {
                plugin_name,
                plugin_version,
            },
            buf,
        ))
    }
}

impl Buffer {
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        write_u32_le(out, self.id);
        write_u32_le(out, self.width);
        write_u32_le(out, self.height);
        self.format.encode(out);
        self.modifier.encode(out);
        write_u32_le(out, self.stride);
        write_u32_le(out, self.offset);
    }

    pub(crate) fn decode(buf: &[u8]) -> Result<(Self, &[u8]), ProtocolError> {
        let (id, buf) = read_u32_le(buf)?;

        let (width, buf) = read_u32_le(buf)?;
        if !(1..=8192).contains(&width) {
            return Err(ProtocolError::OutOfRange);
        }

        let (height, buf) = read_u32_le(buf)?;
        if !(1..=8192).contains(&height) {
            return Err(ProtocolError::OutOfRange);
        }

        let (format, buf) = Fourcc::decode(buf)?;
        if format != Fourcc::ARGB8888 {
            return Err(ProtocolError::OutOfRange);
        }

        let (modifier, buf) = Modifier::decode(buf)?;
        if modifier != Modifier(0) && modifier != Modifier(u64::MAX) {
            return Err(ProtocolError::OutOfRange);
        }

        let (stride, buf) = read_u32_le(buf)?;
        // bpp(ARGB8888) = 4
        if stride < width.saturating_mul(4) {
            return Err(ProtocolError::OutOfRange);
        }

        let (offset, buf) = read_u32_le(buf)?;
        // No upper bound on offset — codec can't validate it without knowing
        // the underlying buffer size. Core checks during fd import.

        Ok((
            Self {
                id,
                width,
                height,
                format,
                modifier,
                stride,
                offset,
            },
            buf,
        ))
    }
}

impl BufferDestroy {
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        write_u32_le(out, self.id)
    }

    pub(crate) fn decode(buf: &[u8]) -> Result<(Self, &[u8]), ProtocolError> {
        let (id, buf) = read_u32_le(buf)?;
        Ok((Self { id }, buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hello_roundtrip() {
        let msg = ClientMessage::Hello(Hello {
            plugin_name: "gradient".to_string(),
            plugin_version: "0.1".to_string(),
        });
        let mut out = Vec::new();
        msg.encode(&mut out).unwrap();
        assert_eq!(ClientMessage::decode(&out).unwrap(), msg);
    }

    #[test]
    fn hello_wire_format() {
        // From the spec, §6.1:
        //   tag = 0x0001                       → 01 00
        //   plugin_name length = 5             → 05 00
        //   plugin_name = "hello"              → 68 65 6c 6c 6f
        //   plugin_version length = 3          → 03 00
        //   plugin_version = "1.0"             → 31 2e 30
        let expected: Vec<u8> = vec![
            0x01, 0x00, 0x05, 0x00, b'h', b'e', b'l', b'l', b'o', 0x03, 0x00, b'1', b'.', b'0',
        ];
        let msg = ClientMessage::Hello(Hello {
            plugin_name: "hello".to_string(),
            plugin_version: "1.0".to_string(),
        });
        let mut out = Vec::new();
        msg.encode(&mut out).unwrap();
        assert_eq!(out, expected);
        // And the spec-mandated bytes decode back into the same message.
        assert_eq!(ClientMessage::decode(&expected).unwrap(), msg);
    }

    #[test]
    fn hello_unknown_tag() {
        let buf = [0xff, 0xff];
        assert_eq!(
            ClientMessage::decode(&buf),
            Err(ProtocolError::UnknownTag(0xffff))
        );
    }

    #[test]
    fn hello_trailing_bytes() {
        let mut buf: Vec<u8> = vec![
            0x01, 0x00, 0x05, 0x00, b'h', b'e', b'l', b'l', b'o', 0x03, 0x00, b'1', b'.', b'0',
        ];
        buf.push(0xaa); // one extra byte
        assert_eq!(
            ClientMessage::decode(&buf),
            Err(ProtocolError::TrailingBytes)
        );
    }

    #[test]
    fn hello_name_too_long_on_decode() {
        // tag = HELLO, then plugin_name length claims 100 bytes (over the 64 cap).
        let buf = [0x01, 0x00, 0x64, 0x00];
        assert_eq!(
            ClientMessage::decode(&buf),
            Err(ProtocolError::StringTooLong {
                max: 64,
                actual: 100
            })
        );
    }

    #[test]
    fn hello_version_too_long_on_decode() {
        // tag = HELLO, plugin_name = "" (len 0), plugin_version length = 50
        // (over the 32 cap). Exercises the cap on plugin_version specifically.
        let buf = [
            0x01, 0x00, // tag
            0x00, 0x00, // plugin_name length = 0
            0x32, 0x00, // plugin_version length = 50 → too long
        ];
        assert_eq!(
            ClientMessage::decode(&buf),
            Err(ProtocolError::StringTooLong {
                max: 32,
                actual: 50
            })
        );
    }

    // Helper: a Buffer with valid v1 values. Tests mutate one field then decode.
    fn valid_buffer() -> Buffer {
        Buffer {
            id: 0,
            width: 64,
            height: 64,
            format: Fourcc::ARGB8888,
            modifier: Modifier(0), // LINEAR
            stride: 256,           // 64 * 4
            offset: 0,
        }
    }

    #[test]
    fn buffer_roundtrip() {
        let msg = ClientMessage::Buffer(valid_buffer());
        let mut out = Vec::new();
        msg.encode(&mut out).unwrap();
        assert_eq!(ClientMessage::decode(&out).unwrap(), msg);
    }

    #[test]
    fn buffer_wire_format() {
        // From the spec, §6.2. Concrete values match `valid_buffer()`.
        //   tag        = 0x0002              → 02 00
        //   id         = 0
        //   width      = 64    (0x40)
        //   height     = 64    (0x40)
        //   format     = ARGB8888 (LE u32 → 'A','R','2','4')
        //   modifier   = 0     (LINEAR)
        //   stride     = 256   (0x100)
        //   offset     = 0
        let expected: Vec<u8> = vec![
            0x02, 0x00, // tag
            0x00, 0x00, 0x00, 0x00, // id
            0x40, 0x00, 0x00, 0x00, // width
            0x40, 0x00, 0x00, 0x00, // height
            b'A', b'R', b'2', b'4', // format
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, // modifier
            0x00, 0x01, 0x00, 0x00, // stride = 256
            0x00, 0x00, 0x00, 0x00, // offset
        ];
        let msg = ClientMessage::Buffer(valid_buffer());
        let mut out = Vec::new();
        msg.encode(&mut out).unwrap();
        assert_eq!(out, expected);
        assert_eq!(ClientMessage::decode(&expected).unwrap(), msg);
    }

    #[test]
    fn buffer_width_zero_rejected() {
        let mut b = valid_buffer();
        b.width = 0;
        let msg = ClientMessage::Buffer(b);
        let mut out = Vec::new();
        msg.encode(&mut out).unwrap();
        assert_eq!(ClientMessage::decode(&out), Err(ProtocolError::OutOfRange));
    }

    #[test]
    fn buffer_width_too_large_rejected() {
        let mut b = valid_buffer();
        b.width = 9000;
        b.stride = 36000; // stride consistent with width, so we isolate the width check
        let msg = ClientMessage::Buffer(b);
        let mut out = Vec::new();
        msg.encode(&mut out).unwrap();
        assert_eq!(ClientMessage::decode(&out), Err(ProtocolError::OutOfRange));
    }

    #[test]
    fn buffer_height_zero_rejected() {
        let mut b = valid_buffer();
        b.height = 0;
        let msg = ClientMessage::Buffer(b);
        let mut out = Vec::new();
        msg.encode(&mut out).unwrap();
        assert_eq!(ClientMessage::decode(&out), Err(ProtocolError::OutOfRange));
    }

    #[test]
    fn buffer_unknown_format_rejected() {
        let mut b = valid_buffer();
        b.format = Fourcc(0xdeadbeef);
        let msg = ClientMessage::Buffer(b);
        let mut out = Vec::new();
        msg.encode(&mut out).unwrap();
        assert_eq!(ClientMessage::decode(&out), Err(ProtocolError::OutOfRange));
    }

    #[test]
    fn buffer_unknown_modifier_rejected() {
        let mut b = valid_buffer();
        b.modifier = Modifier(42);
        let msg = ClientMessage::Buffer(b);
        let mut out = Vec::new();
        msg.encode(&mut out).unwrap();
        assert_eq!(ClientMessage::decode(&out), Err(ProtocolError::OutOfRange));
    }

    #[test]
    fn buffer_modifier_invalid_sentinel_accepted() {
        // u64::MAX (INVALID) is in the v1 allowlist alongside LINEAR.
        let mut b = valid_buffer();
        b.modifier = Modifier(u64::MAX);
        let msg = ClientMessage::Buffer(b);
        let mut out = Vec::new();
        msg.encode(&mut out).unwrap();
        assert_eq!(ClientMessage::decode(&out).unwrap(), msg);
    }

    #[test]
    fn buffer_stride_too_small_rejected() {
        let mut b = valid_buffer();
        b.stride = 100; // less than 64 * 4 = 256
        let msg = ClientMessage::Buffer(b);
        let mut out = Vec::new();
        msg.encode(&mut out).unwrap();
        assert_eq!(ClientMessage::decode(&out), Err(ProtocolError::OutOfRange));
    }

    #[test]
    fn buffer_dimensions_at_edge_accepted() {
        // width = height = 8192 sits at the top of the inclusive range and
        // must be accepted. Guards against a regression to `< 8192` upper bound.
        let mut b = valid_buffer();
        b.width = 8192;
        b.height = 8192;
        b.stride = 8192 * 4;
        let msg = ClientMessage::Buffer(b);
        let mut out = Vec::new();
        msg.encode(&mut out).unwrap();
        assert_eq!(ClientMessage::decode(&out).unwrap(), msg);
    }

    #[test]
    fn buffer_destroy_roundtrip() {
        let msg = ClientMessage::BufferDestroy(BufferDestroy { id: 42 });
        let mut out = Vec::new();
        msg.encode(&mut out).unwrap();
        assert_eq!(ClientMessage::decode(&out).unwrap(), msg);
    }

    #[test]
    fn buffer_destroy_wire_format() {
        // tag = 0x0003         → 03 00
        // id  = 42 (0x2a)      → 2a 00 00 00
        let expected: Vec<u8> = vec![0x03, 0x00, 0x2a, 0x00, 0x00, 0x00];
        let msg = ClientMessage::BufferDestroy(BufferDestroy { id: 42 });
        let mut out = Vec::new();
        msg.encode(&mut out).unwrap();
        assert_eq!(out, expected);
        assert_eq!(ClientMessage::decode(&expected).unwrap(), msg);
    }

    #[test]
    fn buffer_destroy_truncated_payload() {
        // Valid tag, but only 3 of 4 id bytes.
        let buf = [0x03, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(ClientMessage::decode(&buf), Err(ProtocolError::Truncated));
    }

    #[test]
    fn client_message_unknown_tag_near_assigned() {
        // 0x0099 is just past the assigned client tags (0x0001..=0x0003).
        // Catches off-by-one fall-through that hello_unknown_tag's 0xffff might miss.
        let buf = [0x99, 0x00];
        assert_eq!(
            ClientMessage::decode(&buf),
            Err(ProtocolError::UnknownTag(0x0099))
        );
    }
}
