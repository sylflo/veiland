// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-to-plugin messages. See `docs/protocol.md` §7.

use crate::codec::{
    read_i32_le, read_i64_le, read_str, read_u16_le, read_u32_le, write_i32_le, write_i64_le,
    write_str, write_u16_le, write_u32_le,
};
use crate::error::ProtocolError;

/// Cap on `Configure.output_name`'s length on the wire. Matches
/// `Hello.plugin_name`'s cap; Wayland output names are short
/// (`"DP-1"`, `"HDMI-A-1"`, `"eDP-1"`) and 64 bytes is wildly
/// generous. See `docs/protocol.md` §7.1.
pub const CONFIGURE_OUTPUT_NAME_MAX: u16 = 64;

/// Host configures the plugin's render region, scale, and time tick.
/// See `docs/protocol.md` §7.1.
#[derive(Debug, Clone, PartialEq)]
pub struct Configure {
    pub region_x: i32,
    pub region_y: i32,
    pub region_w: u32,
    pub region_h: u32,
    /// Output scale as 120ths, matching `wp_fractional_scale_v1`'s encoding.
    /// 120 = 1×, 180 = 1.5×, 240 = 2×. Use `scale_120 as f32 / 120.0` to get
    /// the float multiplier. The region dimensions are already in physical
    /// pixels — plugins do **not** multiply `region_w`/`region_h` by scale.
    /// Range: 1..=9999.
    pub scale_120: u32,
    pub time_unix_seconds: i64,
    pub time_tz_offset_seconds: i32,
    /// `xdg_output.name` of the output this plugin instance serves
    /// (e.g. `"DP-1"`, `"HDMI-A-1"`). Plugins that don't care about
    /// per-output behaviour ignore it; plugins that do (a wallpaper
    /// rendering different images per monitor, a clock showing a
    /// different timezone per monitor) key internal state off it.
    pub output_name: String,
}

/// Host is done sampling the buffer with this id; plugin may reuse it.
/// See `docs/protocol.md` §7.3.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BufferReleased {
    pub id: u32,
}

/// Any host-to-plugin message. See `docs/protocol.md` §7.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerMessage {
    Configure(Configure),
    FrameDone,
    BufferReleased(BufferReleased),
    Shutdown,
}

impl ServerMessage {
    const TAG_CONFIGURE: u16 = 0x0001;
    const TAG_FRAME_DONE: u16 = 0x0002;
    const TAG_BUFFER_RELEASED: u16 = 0x0003;
    const TAG_SHUTDOWN: u16 = 0x0004;

    /// Encode one message to bytes. The encoded form is tag + variant payload.
    /// No server-side variant can fail to encode in v1 (no fallible fields).
    pub fn encode(&self, out: &mut Vec<u8>) {
        match self {
            ServerMessage::Configure(c) => {
                write_u16_le(out, Self::TAG_CONFIGURE);
                c.encode(out);
            }
            ServerMessage::FrameDone => {
                write_u16_le(out, Self::TAG_FRAME_DONE);
            }
            ServerMessage::BufferReleased(r) => {
                write_u16_le(out, Self::TAG_BUFFER_RELEASED);
                r.encode(out);
            }
            ServerMessage::Shutdown => {
                write_u16_le(out, Self::TAG_SHUTDOWN);
            }
        }
    }

    /// Decode one message from a byte buffer. Returns `TrailingBytes` if the
    /// buffer contains more than one message's worth of data.
    pub fn decode(buf: &[u8]) -> Result<Self, ProtocolError> {
        let (tag, buf) = read_u16_le(buf)?;
        let (msg, rest) = match tag {
            Self::TAG_CONFIGURE => {
                let (c, rest) = Configure::decode(buf)?;
                (ServerMessage::Configure(c), rest)
            }
            Self::TAG_FRAME_DONE => (ServerMessage::FrameDone, buf),
            Self::TAG_BUFFER_RELEASED => {
                let (r, rest) = BufferReleased::decode(buf)?;
                (ServerMessage::BufferReleased(r), rest)
            }
            Self::TAG_SHUTDOWN => (ServerMessage::Shutdown, buf),
            other => return Err(ProtocolError::UnknownTag(other)),
        };
        if !rest.is_empty() {
            return Err(ProtocolError::TrailingBytes);
        }
        Ok(msg)
    }
}

impl Configure {
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        write_i32_le(out, self.region_x);
        write_i32_le(out, self.region_y);
        write_u32_le(out, self.region_w);
        write_u32_le(out, self.region_h);
        write_u32_le(out, self.scale_120);
        write_i64_le(out, self.time_unix_seconds);
        write_i32_le(out, self.time_tz_offset_seconds);
        // Host-controlled string (xdg_output.name), always short in
        // practice. An over-cap value would be a host bug, not runtime
        // input — keep encode infallible (matches the §7 invariant
        // "no server-side variant can fail to encode in v1") and assert
        // on the bug case.
        debug_assert!(
            self.output_name.len() <= CONFIGURE_OUTPUT_NAME_MAX as usize,
            "output_name {} bytes exceeds CONFIGURE_OUTPUT_NAME_MAX ({})",
            self.output_name.len(),
            CONFIGURE_OUTPUT_NAME_MAX,
        );
        // Panic-free in release: write_str's check catches it and
        // returns Err; we just unwrap since encode's signature is
        // infallible and the debug_assert above already covered the
        // dev-time case.
        write_str(out, &self.output_name, CONFIGURE_OUTPUT_NAME_MAX)
            .expect("Configure.output_name length already bounded");
    }

    pub(crate) fn decode(buf: &[u8]) -> Result<(Self, &[u8]), ProtocolError> {
        let (region_x, buf) = read_i32_le(buf)?;
        let (region_y, buf) = read_i32_le(buf)?;

        let (region_w, buf) = read_u32_le(buf)?;
        if !(1..=8192).contains(&region_w) {
            return Err(ProtocolError::OutOfRange);
        }

        let (region_h, buf) = read_u32_le(buf)?;
        if !(1..=8192).contains(&region_h) {
            return Err(ProtocolError::OutOfRange);
        }

        let (scale_120, buf) = read_u32_le(buf)?;
        if !(1..=9999).contains(&scale_120) {
            return Err(ProtocolError::OutOfRange);
        }

        let (time_unix_seconds, buf) = read_i64_le(buf)?;
        let (time_tz_offset_seconds, buf) = read_i32_le(buf)?;

        let (output_name, buf) = read_str(buf, CONFIGURE_OUTPUT_NAME_MAX)?;

        Ok((
            Self {
                region_x,
                region_y,
                region_w,
                region_h,
                scale_120,
                time_unix_seconds,
                time_tz_offset_seconds,
                output_name,
            },
            buf,
        ))
    }
}

impl BufferReleased {
    pub(crate) fn encode(&self, out: &mut Vec<u8>) {
        write_u32_le(out, self.id);
    }

    pub(crate) fn decode(buf: &[u8]) -> Result<(Self, &[u8]), ProtocolError> {
        let (id, buf) = read_u32_le(buf)?;
        Ok((Self { id }, buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_configure() -> Configure {
        Configure {
            region_x: 100,
            region_y: 200,
            region_w: 800,
            region_h: 600,
            scale_120: 120,
            time_unix_seconds: 1_700_000_000,
            time_tz_offset_seconds: 3600,
            output_name: "DP-1".to_string(),
        }
    }

    #[test]
    fn configure_roundtrip() {
        let msg = ServerMessage::Configure(valid_configure());
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(ServerMessage::decode(&out).unwrap(), msg);
    }

    #[test]
    fn configure_wire_format() {
        // From the spec, §7.1. Field order: region_x, region_y, region_w,
        // region_h, scale, time_unix_seconds, time_tz_offset_seconds,
        // output_name.
        let expected: Vec<u8> = vec![
            0x01, 0x00, // tag = Configure
            0x64, 0x00, 0x00, 0x00, // region_x = 100
            0xc8, 0x00, 0x00, 0x00, // region_y = 200
            0x20, 0x03, 0x00, 0x00, // region_w = 800
            0x58, 0x02, 0x00, 0x00, // region_h = 600
            0x78, 0x00, 0x00, 0x00, // scale_120 = 120 (1×)
            0x00, 0xf1, 0x53, 0x65, 0x00, 0x00, 0x00, 0x00, // time_unix = 1_700_000_000
            0x10, 0x0e, 0x00, 0x00, // tz_offset = 3600
            0x04, 0x00, // output_name length = 4
            b'D', b'P', b'-', b'1', // output_name = "DP-1"
        ];
        let msg = ServerMessage::Configure(valid_configure());
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(out, expected);
        assert_eq!(ServerMessage::decode(&expected).unwrap(), msg);
    }

    #[test]
    fn configure_region_w_zero_rejected() {
        let mut c = valid_configure();
        c.region_w = 0;
        let msg = ServerMessage::Configure(c);
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(ServerMessage::decode(&out), Err(ProtocolError::OutOfRange));
    }

    #[test]
    fn configure_region_h_too_large_rejected() {
        let mut c = valid_configure();
        c.region_h = 9000;
        let msg = ServerMessage::Configure(c);
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(ServerMessage::decode(&out), Err(ProtocolError::OutOfRange));
    }

    #[test]
    fn configure_scale_zero_rejected() {
        let mut c = valid_configure();
        c.scale_120 = 0;
        let msg = ServerMessage::Configure(c);
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(ServerMessage::decode(&out), Err(ProtocolError::OutOfRange));
    }

    #[test]
    fn configure_scale_too_large_rejected() {
        let mut c = valid_configure();
        c.scale_120 = 10000;
        let msg = ServerMessage::Configure(c);
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(ServerMessage::decode(&out), Err(ProtocolError::OutOfRange));
    }

    #[test]
    fn configure_values_at_max_edge_accepted() {
        // region_w = region_h = 8192 and scale_120 = 9999 are the top of their
        // inclusive ranges and must be accepted.
        let mut c = valid_configure();
        c.region_w = 8192;
        c.region_h = 8192;
        c.scale_120 = 9999;
        let msg = ServerMessage::Configure(c);
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(ServerMessage::decode(&out).unwrap(), msg);
    }

    #[test]
    fn configure_scale_fractional_accepted() {
        // 150 = 1.25×, a common laptop fractional scale.
        let mut c = valid_configure();
        c.scale_120 = 150;
        let msg = ServerMessage::Configure(c);
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(ServerMessage::decode(&out).unwrap(), msg);
    }

    #[test]
    fn frame_done_roundtrip() {
        let msg = ServerMessage::FrameDone;
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(ServerMessage::decode(&out).unwrap(), msg);
    }

    #[test]
    fn frame_done_wire_format() {
        // Just the 2-byte tag, no payload.
        let expected: Vec<u8> = vec![0x02, 0x00];
        let msg = ServerMessage::FrameDone;
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(out, expected);
        assert_eq!(ServerMessage::decode(&expected).unwrap(), msg);
    }

    #[test]
    fn frame_done_trailing_bytes_rejected() {
        // Valid tag, but extra byte — must be rejected, not silently ignored.
        // Exercises the unit-variant boundary: no payload means the buffer
        // should already be empty before the TrailingBytes check.
        let buf = [0x02, 0x00, 0xaa];
        assert_eq!(
            ServerMessage::decode(&buf),
            Err(ProtocolError::TrailingBytes)
        );
    }

    #[test]
    fn buffer_released_roundtrip() {
        let msg = ServerMessage::BufferReleased(BufferReleased { id: 7 });
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(ServerMessage::decode(&out).unwrap(), msg);
    }

    #[test]
    fn buffer_released_wire_format() {
        // tag = 0x0003         → 03 00
        // id  = 7              → 07 00 00 00
        let expected: Vec<u8> = vec![0x03, 0x00, 0x07, 0x00, 0x00, 0x00];
        let msg = ServerMessage::BufferReleased(BufferReleased { id: 7 });
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(out, expected);
        assert_eq!(ServerMessage::decode(&expected).unwrap(), msg);
    }

    #[test]
    fn shutdown_roundtrip() {
        let msg = ServerMessage::Shutdown;
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(ServerMessage::decode(&out).unwrap(), msg);
    }

    #[test]
    fn server_message_unknown_tag_near_assigned() {
        // 0x0099 is past the assigned server tags (0x0001..=0x0004).
        let buf = [0x99, 0x00];
        assert_eq!(
            ServerMessage::decode(&buf),
            Err(ProtocolError::UnknownTag(0x0099))
        );
    }

    #[test]
    fn configure_empty_output_name_roundtrip() {
        // A hotplug edge case could briefly produce an unnamed output;
        // the codec must accept the empty string cleanly (length = 0,
        // no payload bytes).
        let mut c = valid_configure();
        c.output_name = String::new();
        let msg = ServerMessage::Configure(c);
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(ServerMessage::decode(&out).unwrap(), msg);
    }

    #[test]
    fn configure_max_length_output_name_roundtrip() {
        // 64 bytes is at the inclusive cap; must round-trip.
        let mut c = valid_configure();
        c.output_name = "a".repeat(CONFIGURE_OUTPUT_NAME_MAX as usize);
        let msg = ServerMessage::Configure(c);
        let mut out = Vec::new();
        msg.encode(&mut out);
        assert_eq!(ServerMessage::decode(&out).unwrap(), msg);
    }

    #[test]
    fn configure_over_cap_output_name_rejected_on_decode() {
        // Craft bytes claiming a 65-byte output_name. decode must reject
        // before allocating. (We don't go through encode here — the
        // host-side debug_assert would trip first; this test exercises
        // the wire-level defence against a malicious peer.)
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0x01, 0x00]); // tag = Configure
        bytes.extend_from_slice(&100i32.to_le_bytes()); // region_x
        bytes.extend_from_slice(&200i32.to_le_bytes()); // region_y
        bytes.extend_from_slice(&800u32.to_le_bytes()); // region_w
        bytes.extend_from_slice(&600u32.to_le_bytes()); // region_h
        bytes.extend_from_slice(&120u32.to_le_bytes()); // scale_120 = 120 (1×)
        bytes.extend_from_slice(&1_700_000_000i64.to_le_bytes()); // time_unix
        bytes.extend_from_slice(&3600i32.to_le_bytes()); // tz
        bytes.extend_from_slice(&65u16.to_le_bytes()); // output_name length = 65
        bytes.extend(std::iter::repeat_n(b'a', 65));
        assert_eq!(
            ServerMessage::decode(&bytes),
            Err(ProtocolError::StringTooLong {
                max: CONFIGURE_OUTPUT_NAME_MAX,
                actual: 65,
            })
        );
    }

    #[test]
    fn configure_invalid_utf8_output_name_rejected() {
        // Length = 1, one byte that's not valid UTF-8 on its own.
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&[0x01, 0x00]); // tag = Configure
        bytes.extend_from_slice(&100i32.to_le_bytes());
        bytes.extend_from_slice(&200i32.to_le_bytes());
        bytes.extend_from_slice(&800u32.to_le_bytes());
        bytes.extend_from_slice(&600u32.to_le_bytes());
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&1_700_000_000i64.to_le_bytes());
        bytes.extend_from_slice(&3600i32.to_le_bytes());
        bytes.extend_from_slice(&1u16.to_le_bytes()); // output_name length = 1
        bytes.push(0xff); // not valid UTF-8
        assert_eq!(
            ServerMessage::decode(&bytes),
            Err(ProtocolError::InvalidUtf8)
        );
    }
}
