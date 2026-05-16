// SPDX-License-Identifier: GPL-3.0-or-later

//! Error type returned by every decode path. See `docs/protocol.md` §9.

#[derive(Debug, PartialEq)]
#[non_exhaustive]
pub enum ProtocolError {
    /// Buffer ended before all bytes the schema requires were available.
    Truncated,
    /// Buffer contained more bytes than the variant's schema specified.
    TrailingBytes,
    /// Tag value not recognised in this direction.
    UnknownTag(u16),
    /// `str` payload was not valid UTF-8.
    InvalidUtf8,
    /// Declared string length exceeded the per-field cap.
    StringTooLong { max: u16, actual: u16 },
    /// A value violated a spec-declared range.
    OutOfRange,
}

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:?}", self)
    }
}

impl std::error::Error for ProtocolError {}
