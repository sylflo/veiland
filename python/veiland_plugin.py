# SPDX-License-Identifier: GPL-3.0-or-later
#
# veiland plugin SDK for Python -- the wire codec and the connection
# ceremony, so a plugin author writes the widget, not the protocol. This
# is a second, independent implementation of docs/protocol.md (the Rust
# crate veiland-protocol is the other); the doc is the source of truth and
# the two are kept byte-locked by shared golden-byte tests.
#
# Single vendorable file: copy it next to your plugin, no pip. Pure stdlib,
# Python >= 3.9 (socket.send_fds / recv_fds). PR A is codec + Connection
# only -- GBM buffers and the frame pacer land in PR B.

from __future__ import annotations

import os
import socket
import struct
from dataclasses import dataclass
from enum import Enum
from typing import Union

# The vendorable public surface. Everything else (the _Reader cursor,
# _write_str, the _Singleton enum) is a leading-underscore private helper.
__all__ = [
    # connection + message types
    "Connection",
    "Configure",
    "BufferReleased",
    "ServerMessage",
    "FRAME_DONE",
    "SHUTDOWN",
    # codec functions
    "encode_hello",
    "encode_buffer",
    "encode_buffer_destroy",
    "decode_server_message",
    # exceptions
    "ProtocolError",
    "Truncated",
    "TrailingBytes",
    "UnknownTag",
    "InvalidUtf8",
    "StringTooLong",
    "OutOfRange",
    "HandshakeError",
    "HostClosed",
    # constants
    "PROTOCOL_VERSION",
    "HOST_CAP_FENCE_FD",
    "FOURCC_ARGB8888",
    "MODIFIER_INVALID",
    "PLUGIN_NAME_MAX",
    "PLUGIN_VERSION_MAX",
    "OUTPUT_NAME_MAX",
]

# ------------------------------------------------------------------ constants

# Protocol version negotiated in the handshake (docs/protocol.md 5).
PROTOCOL_VERSION = 1

# Host capability bits (docs/protocol.md 5.1). Bit 0 is the only defined
# bit; bits 1..31 are reserved and MUST be zero -- a plugin that sees any
# reserved bit set fails the handshake closed.
HOST_CAP_FENCE_FD = 1 << 0
_HOST_CAP_KNOWN = HOST_CAP_FENCE_FD

# Client -> host message tags (docs/protocol.md 6).
TAG_HELLO = 0x0001
TAG_BUFFER = 0x0002
TAG_BUFFER_DESTROY = 0x0003

# Host -> client message tags (docs/protocol.md 7).
TAG_CONFIGURE = 0x0001
TAG_FRAME_DONE = 0x0002
TAG_BUFFER_RELEASED = 0x0003
TAG_SHUTDOWN = 0x0004

# Per-field string caps, in bytes (docs/protocol.md 6.1, 7.1).
PLUGIN_NAME_MAX = 64
PLUGIN_VERSION_MAX = 32
OUTPUT_NAME_MAX = 64

# Dimension and scale ranges (docs/protocol.md 6.2, 7.1). Inclusive.
DIM_MIN, DIM_MAX = 1, 8192
SCALE_MIN, SCALE_MAX = 1, 9999

# DRM FourCC for 32-bit ARGB, little-endian bytes 'A','R','2','4'. The only
# format the CPU-widget tier needs; the codec accepts any u32, so this is a
# convenience constant, not a restriction (docs/protocol.md 6.2, 11).
FOURCC_ARGB8888 = 0x34325241

# DRM "INVALID" modifier sentinel: what gbm_bo_create returns without
# explicit modifier negotiation. A legitimate value to send; whether the
# host can import it is EGL's decision (docs/protocol.md 6.2).
MODIFIER_INVALID = (1 << 64) - 1

# One recv() returns exactly one message under SOCK_SEQPACKET; 64 KiB is the
# protocol's max payload (docs/protocol.md 2). No legal message comes close.
_RECV_SIZE = 65536


# ----------------------------------------------------------------- exceptions

# The hierarchy mirrors veiland-protocol's ProtocolError enum (error.rs) so a
# caller can branch on a specific failure. All wire/handshake faults derive
# from ProtocolError; HostClosed does NOT -- host-gone is the normal clean
# exit, not an error, and must never surface as a traceback.


class ProtocolError(Exception):
    """Base for every wire-format and handshake violation."""


class Truncated(ProtocolError):
    """Buffer ended before all bytes the schema requires were available."""


class TrailingBytes(ProtocolError):
    """Buffer held more bytes than the variant's schema specified."""


class UnknownTag(ProtocolError):
    """Tag value not recognised in this direction."""

    def __init__(self, tag: int):
        self.tag = tag
        super().__init__(f"unknown message tag {tag:#06x}")


class InvalidUtf8(ProtocolError):
    """A str payload was not valid UTF-8."""


class StringTooLong(ProtocolError):
    """A declared string length exceeded the per-field cap."""

    def __init__(self, max_len: int, actual: int):
        self.max = max_len
        self.actual = actual
        super().__init__(f"string of {actual} bytes exceeds cap of {max_len}")


class OutOfRange(ProtocolError):
    """A field violated a spec-declared range."""


class HandshakeError(ProtocolError):
    """Version mismatch or a reserved host-capability bit set."""


class HostClosed(Exception):
    """The host closed the socket (EOF). The normal end of a session -- the
    caller should exit cleanly, not treat this as a fault."""


# --------------------------------------------------------------- codec: read

# Rust's read_*_le thread a &[u8] and return (value, rest); the Pythonic
# equivalent is a cursor that threads its offset and raises Truncated when a
# read would run past the end. Decoders below read fields top-down, exactly
# like Configure::decode in server.rs, then assert the buffer is exhausted.


class _Reader:
    __slots__ = ("_buf", "_off")

    def __init__(self, buf: bytes):
        self._buf = buf
        self._off = 0

    def _take(self, fmt: str) -> int:
        # Every fmt this class passes ("<H"/"<I"/"<Q"/"<i"/"<q") unpacks to a
        # single Python int; struct.unpack_from types the element as Any, so
        # int() both narrows the type and documents that invariant.
        size = struct.calcsize(fmt)
        if self._off + size > len(self._buf):
            raise Truncated(
                f"need {size} bytes at offset {self._off}, "
                f"have {len(self._buf) - self._off}"
            )
        (value,) = struct.unpack_from(fmt, self._buf, self._off)
        self._off += size
        return int(value)

    def u16(self) -> int:
        return self._take("<H")

    def u32(self) -> int:
        return self._take("<I")

    def u64(self) -> int:
        return self._take("<Q")

    def i32(self) -> int:
        return self._take("<i")

    def i64(self) -> int:
        return self._take("<q")

    def string(self, max_len: int) -> str:
        length = self.u16()
        if length > max_len:
            raise StringTooLong(max_len, length)
        if self._off + length > len(self._buf):
            raise Truncated(
                f"str claims {length} bytes, only {len(self._buf) - self._off} remain"
            )
        raw = self._buf[self._off : self._off + length]
        self._off += length
        try:
            return raw.decode("utf-8")
        except UnicodeDecodeError:
            raise InvalidUtf8("str payload is not valid UTF-8") from None

    def finish(self) -> None:
        """A frame carries exactly one message; leftover bytes are a fault."""
        if self._off != len(self._buf):
            raise TrailingBytes(f"{len(self._buf) - self._off} bytes after message end")


# -------------------------------------------------------------- codec: write


def _write_str(out: bytearray, s: str, max_len: int) -> None:
    raw = s.encode("utf-8")
    if len(raw) > max_len:
        raise StringTooLong(max_len, len(raw))
    out += struct.pack("<H", len(raw))
    out += raw


# ------------------------------------------------- client-message encoders

# Plugin -> host. Each returns the full frame bytes (tag + payload); the fd,
# where present, is out-of-band and handled by Connection.send_buffer.


def encode_hello(name: str, version: str) -> bytes:
    out = bytearray(struct.pack("<H", TAG_HELLO))
    _write_str(out, name, PLUGIN_NAME_MAX)
    _write_str(out, version, PLUGIN_VERSION_MAX)
    return bytes(out)


def encode_buffer(
    buf_id: int,
    width: int,
    height: int,
    fourcc: int,
    modifier: int,
    stride: int,
    offset: int = 0,
) -> bytes:
    # Validate on encode too (settled decision): a plugin bug surfaces here as
    # a clean OutOfRange at send time, not as a silent host-side socket close.
    if not (DIM_MIN <= width <= DIM_MAX):
        raise OutOfRange(f"width {width} outside [{DIM_MIN}, {DIM_MAX}]")
    if not (DIM_MIN <= height <= DIM_MAX):
        raise OutOfRange(f"height {height} outside [{DIM_MIN}, {DIM_MAX}]")
    if stride < width:
        raise OutOfRange(f"stride {stride} < width {width}")
    return struct.pack(
        "<HIIIIQII",
        TAG_BUFFER,
        buf_id,
        width,
        height,
        fourcc,
        modifier,
        stride,
        offset,
    )


def encode_buffer_destroy(buf_id: int) -> bytes:
    return struct.pack("<HI", TAG_BUFFER_DESTROY, buf_id)


# ------------------------------------------------------ server-message types

# Host -> client. Small typed union: FrameDone and Shutdown are singletons
# (empty payloads), Configure and BufferReleased carry data.


@dataclass(frozen=True)
class Configure:
    """Host configures region, scale, and the time tick (docs/protocol.md 7.1).

    region_w/region_h are already physical pixels -- do not multiply by scale.
    scale_120 is for converting plugin-internal logical sizes to physical
    pixels; use the `scale` property for the float multiplier."""

    region_x: int
    region_y: int
    region_w: int
    region_h: int
    scale_120: int
    time_unix_seconds: int
    time_tz_offset_seconds: int
    output_name: str

    @property
    def scale(self) -> float:
        return self.scale_120 / 120.0


@dataclass(frozen=True)
class BufferReleased:
    """Host is done sampling this buffer id; the plugin may reuse it."""

    id: int


class _Singleton(Enum):
    FRAME_DONE = TAG_FRAME_DONE
    SHUTDOWN = TAG_SHUTDOWN


# Module-level instances so callers compare with `is`.
FRAME_DONE = _Singleton.FRAME_DONE
SHUTDOWN = _Singleton.SHUTDOWN

ServerMessage = Union[Configure, BufferReleased, _Singleton]


def decode_server_message(frame: bytes) -> ServerMessage:
    """Decode one host->client frame. Raises UnknownTag / TrailingBytes /
    Truncated / OutOfRange / InvalidUtf8 / StringTooLong on a bad frame."""
    r = _Reader(frame)
    tag = r.u16()
    msg: ServerMessage
    if tag == TAG_CONFIGURE:
        msg = _decode_configure(r)
    elif tag == TAG_FRAME_DONE:
        msg = FRAME_DONE
    elif tag == TAG_BUFFER_RELEASED:
        msg = BufferReleased(id=r.u32())
    elif tag == TAG_SHUTDOWN:
        msg = SHUTDOWN
    else:
        raise UnknownTag(tag)
    r.finish()
    return msg


def _decode_configure(r: _Reader) -> Configure:
    region_x = r.i32()
    region_y = r.i32()
    region_w = r.u32()
    if not (DIM_MIN <= region_w <= DIM_MAX):
        raise OutOfRange(f"region_w {region_w} outside [{DIM_MIN}, {DIM_MAX}]")
    region_h = r.u32()
    if not (DIM_MIN <= region_h <= DIM_MAX):
        raise OutOfRange(f"region_h {region_h} outside [{DIM_MIN}, {DIM_MAX}]")
    scale_120 = r.u32()
    if not (SCALE_MIN <= scale_120 <= SCALE_MAX):
        raise OutOfRange(f"scale_120 {scale_120} outside [{SCALE_MIN}, {SCALE_MAX}]")
    time_unix_seconds = r.i64()
    time_tz_offset_seconds = r.i32()
    output_name = r.string(OUTPUT_NAME_MAX)
    return Configure(
        region_x=region_x,
        region_y=region_y,
        region_w=region_w,
        region_h=region_h,
        scale_120=scale_120,
        time_unix_seconds=time_unix_seconds,
        time_tz_offset_seconds=time_tz_offset_seconds,
        output_name=output_name,
    )


# ----------------------------------------------------------------- connection


class Connection:
    """The handshake and the framed transport. Owns the socket; hands the
    author typed events. Imperative primitives, not a framework -- the author
    drives the loop.

    CPU widgets are slow-path by construction: the buffer is complete before
    send, so every Buffer carries exactly one fd (the dmabuf) and never a
    fence. host_capabilities is still read and reserved bits fail closed."""

    def __init__(self, sock: socket.socket, host_capabilities: int):
        self._sock = sock
        self.host_capabilities = host_capabilities

    @classmethod
    def connect(cls, name: str, version: str) -> Connection:
        """Read the socket fd from VEILAND_PLUGIN_SOCKET, run the version +
        capability handshake, and send Hello. Do this before any heavy setup
        of your own -- the host applies a spawn deadline (docs/protocol.md 5)."""
        raw_fd = os.environ.get("VEILAND_PLUGIN_SOCKET")
        if raw_fd is None:
            raise HandshakeError("VEILAND_PLUGIN_SOCKET is not set")
        try:
            fd = int(raw_fd)
        except ValueError:
            raise HandshakeError(
                f"VEILAND_PLUGIN_SOCKET={raw_fd!r} is not an integer"
            ) from None

        sock = socket.socket(socket.AF_UNIX, socket.SOCK_SEQPACKET, fileno=fd)
        try:
            # 1. client -> server: our version.
            sock.send(struct.pack("<I", PROTOCOL_VERSION))
            # 2. server -> client: its version (or socket close on mismatch).
            server_version = _recv_u32(sock, "server version")
            if server_version != PROTOCOL_VERSION:
                raise HandshakeError(
                    f"host protocol version {server_version} != {PROTOCOL_VERSION}"
                )
            # 3. server -> client: host capabilities, reserved bits MUST be 0.
            caps = _recv_u32(sock, "host capabilities")
            if caps & ~_HOST_CAP_KNOWN:
                raise HandshakeError(
                    f"host set reserved capability bits ({caps:#010x}); "
                    "refusing to guess a future dialect"
                )
            # 4. client -> server: Hello.
            sock.send(encode_hello(name, version))
        except HandshakeError:
            sock.close()
            raise
        except OSError as e:
            # A host that died before or during the handshake shows up here as
            # a send-side broken pipe / reset (the recv side is handled by
            # _recv_u32's short-read check). Translate to a clean typed error
            # rather than leaking a raw OSError traceback.
            sock.close()
            raise HandshakeError(f"host connection failed during handshake: {e}") from e
        except BaseException:
            sock.close()
            raise
        return cls(sock, caps)

    def wait_for_configure(self) -> Configure:
        """Block until the first Configure. Per the lifecycle (protocol.md 8)
        Configure precedes every other host message; a Shutdown here means the
        host gave up before we started (raises HostClosed), and any other
        message before Configure is a protocol fault."""
        msg = self.recv_event()
        if isinstance(msg, Configure):
            return msg
        if msg is SHUTDOWN:
            raise HostClosed("host sent Shutdown before Configure")
        raise ProtocolError(
            f"expected Configure as the first host message, got {msg!r}"
        )

    def recv_event(self) -> ServerMessage:
        """Receive and decode one host message. Raises HostClosed on EOF (the
        normal end of a session) and a ProtocolError subclass on a bad frame."""
        frame = self._sock.recv(_RECV_SIZE)
        if not frame:
            raise HostClosed("host closed the socket")
        return decode_server_message(frame)

    def send_buffer(
        self,
        dmabuf_fd: int,
        buf_id: int,
        width: int,
        height: int,
        fourcc: int,
        modifier: int,
        stride: int,
        offset: int = 0,
    ) -> None:
        """Send a Buffer message with the dmabuf fd attached via SCM_RIGHTS.
        Exactly one fd, always (slow path); no fence. send_fds dups the fd, so
        the caller retains ownership and reuses it for the buffer's lifetime."""
        frame = encode_buffer(buf_id, width, height, fourcc, modifier, stride, offset)
        socket.send_fds(self._sock, [frame], [dmabuf_fd])

    def send_buffer_destroy(self, buf_id: int) -> None:
        self._sock.send(encode_buffer_destroy(buf_id))

    def fileno(self) -> int:
        """The underlying socket fd, for select()/poll() in the caller's loop."""
        return self._sock.fileno()

    def close(self) -> None:
        self._sock.close()

    def __enter__(self) -> Connection:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()


def _recv_u32(sock: socket.socket, what: str) -> int:
    """Read a bare 4-byte little-endian u32 handshake word. A short read means
    the host closed mid-handshake (HandshakeError, not HostClosed -- an
    incomplete handshake is a failure, not a clean session end)."""
    data = sock.recv(4)
    if len(data) != 4:
        raise HandshakeError(f"host closed while reading {what}")
    return int(struct.unpack("<I", data)[0])
