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

import ctypes
import ctypes.util
import glob
import os
import select
import socket
import struct
from collections.abc import Iterator, Sequence
from contextlib import contextmanager
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
    # GPU buffers + pacing (PR B)
    "GbmDevice",
    "LinearBuffer",
    "BufferChain",
    "FramePacer",
    "FrameEvent",
    "Event",
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
    "GbmError",
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


class GbmError(Exception):
    """A GBM/DRM resource operation failed (device open, allocation, map).

    Not a ProtocolError: nothing on the wire went wrong, this is a local GPU
    resource fault. Like every SDK failure it is a clean typed exception, not
    a sys.exit or a raw ctypes surprise -- the author decides how to react
    (retry, draw a fallback, or exit)."""


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


# ------------------------------------------------------------- GBM via ctypes

# The CPU-widget buffer path: allocate a linear (row-major, no tiling) GBM
# buffer object, CPU-map it, draw premultiplied BGRA into it, export a dmabuf
# fd, hand the fd to the host. No GL here -- the buffer is complete before we
# send, which is exactly why CPU plugins are the slow path (protocol.md 5.1,
# "CPU plugins are inherently slow-path"): never a fence, always one fd.
#
# libgbm is loaded lazily on first GbmDevice() so that importing this module
# (e.g. the codec-only test suite, or a box with no /dev/dri) never fails --
# only actually asking for a buffer needs the library present.

# GBM buffer-use / transfer flags we rely on. LINEAR forces row-major layout
# so the host can import it without a tiling modifier; TRANSFER_WRITE is the
# CPU-write access mode gbm_bo_map wants. Values from gbm.h, stable ABI.
GBM_BO_USE_LINEAR = 1 << 4
GBM_BO_TRANSFER_WRITE = 1 << 1

# A GBM buffer object / device pointer is an opaque C void*. We never
# dereference it in Python; we only pass it back to libgbm. Aliased for
# readability in the annotations below.
_GbmPtr = ctypes.c_void_p


class _Gbm:
    """The bound libgbm entry points, annotated for ctypes marshalling.

    restype/argtypes are load-bearing, not documentation: without them ctypes
    assumes every argument and return is a C int, which silently truncates the
    64-bit pointers and modifiers this API traffics in. One instance is built
    on first use and cached by _load_gbm()."""

    def __init__(self, lib: ctypes.CDLL):
        self._lib = lib
        # (name, restype, argtypes) for each entry point we call.
        table: list[tuple[str, object, list[object]]] = [
            # gbm_create_device(drm_fd) -> device*
            ("gbm_create_device", _GbmPtr, [ctypes.c_int]),
            # gbm_bo_create(device*, w, h, fourcc, use_flags) -> bo*
            (
                "gbm_bo_create",
                _GbmPtr,
                [
                    _GbmPtr,
                    ctypes.c_uint32,
                    ctypes.c_uint32,
                    ctypes.c_uint32,
                    ctypes.c_uint32,
                ],
            ),
            # gbm_bo_map(bo*, x, y, w, h, flags, *out_stride, **out_map_data)
            #   -> void* to the mapped pixels (or NULL on failure).
            # The last two args are out-pointers: out_stride is filled with the
            # map stride (may differ from the bo stride); out_map_data receives
            # an opaque handle that MUST be passed to gbm_bo_unmap.
            (
                "gbm_bo_map",
                _GbmPtr,
                [
                    _GbmPtr,
                    ctypes.c_uint32,
                    ctypes.c_uint32,
                    ctypes.c_uint32,
                    ctypes.c_uint32,
                    ctypes.c_uint32,
                    ctypes.POINTER(ctypes.c_uint32),
                    ctypes.POINTER(_GbmPtr),
                ],
            ),
            # gbm_bo_unmap(bo*, map_data) -> void
            ("gbm_bo_unmap", None, [_GbmPtr, _GbmPtr]),
            # gbm_bo_get_fd(bo*) -> new dmabuf fd we own (or < 0)
            ("gbm_bo_get_fd", ctypes.c_int, [_GbmPtr]),
            # gbm_bo_get_stride(bo*) -> row stride in bytes (the *bo* stride,
            # the one sent in the Buffer message -- distinct from map stride)
            ("gbm_bo_get_stride", ctypes.c_uint32, [_GbmPtr]),
            # gbm_bo_get_modifier(bo*) -> DRM format modifier (u64)
            ("gbm_bo_get_modifier", ctypes.c_uint64, [_GbmPtr]),
            # gbm_bo_destroy(bo*) -> void
            ("gbm_bo_destroy", None, [_GbmPtr]),
            # gbm_device_destroy(device*) -> void
            ("gbm_device_destroy", None, [_GbmPtr]),
        ]
        for name, restype, argtypes in table:
            fn = getattr(lib, name)
            fn.restype = restype
            fn.argtypes = argtypes

    def create_device(self, drm_fd: int) -> int:
        # ctypes returns c_void_p as a Python int-or-None; NULL comes back as
        # None. Normalise to 0 so callers test a single falsy value.
        ptr = self._lib.gbm_create_device(drm_fd)
        return int(ptr) if ptr else 0

    def bo_create(self, dev: int, w: int, h: int, fourcc: int, flags: int) -> int:
        ptr = self._lib.gbm_bo_create(dev, w, h, fourcc, flags)
        return int(ptr) if ptr else 0

    def bo_map(
        self,
        bo: int,
        w: int,
        h: int,
        out_stride: ctypes.c_uint32,
        out_data: ctypes.c_void_p,
    ) -> int:
        ptr = self._lib.gbm_bo_map(
            bo,
            0,
            0,
            w,
            h,
            GBM_BO_TRANSFER_WRITE,
            ctypes.byref(out_stride),
            ctypes.byref(out_data),
        )
        return int(ptr) if ptr else 0

    def bo_unmap(self, bo: int, map_data: ctypes.c_void_p) -> None:
        self._lib.gbm_bo_unmap(bo, map_data)

    def bo_get_fd(self, bo: int) -> int:
        return int(self._lib.gbm_bo_get_fd(bo))

    def bo_get_stride(self, bo: int) -> int:
        return int(self._lib.gbm_bo_get_stride(bo))

    def bo_get_modifier(self, bo: int) -> int:
        return int(self._lib.gbm_bo_get_modifier(bo))

    def bo_destroy(self, bo: int) -> None:
        self._lib.gbm_bo_destroy(bo)

    def device_destroy(self, dev: int) -> None:
        self._lib.gbm_device_destroy(dev)


_gbm_cache: _Gbm | None = None


def _load_gbm() -> _Gbm:
    """Load and bind libgbm once, cached. find_library("gbm") comes up empty on
    NixOS (it needs ldconfig/gcc); the bare "libgbm.so.1" soname resolves via
    LD_LIBRARY_PATH there (the veiland dev shell sets it). Raises GbmError, not
    an OSError traceback, if the library is absent."""
    global _gbm_cache
    if _gbm_cache is None:
        path = ctypes.util.find_library("gbm") or "libgbm.so.1"
        try:
            lib = ctypes.CDLL(path)
        except OSError as e:
            raise GbmError(f"cannot load libgbm ({e})") from e
        _gbm_cache = _Gbm(lib)
    return _gbm_cache


# ----------------------------------------------------------------- GbmDevice


class GbmDevice:
    """A GBM device on a DRM render node -- the allocator LinearBuffers draw
    from. Opened explicitly (like the Rust SDK's GbmEgl), not inferred from a
    Wayland connection: a CPU widget never touches EGL, it only needs a node to
    allocate dmabufs on. Use as a context manager or call close() when done.

    Raises GbmError on any failure (no render node, device creation) rather
    than exiting -- the author owns the reaction."""

    def __init__(self, render_node: str | None = None):
        gbm = _load_gbm()
        node = render_node or _first_render_node()
        # O_CLOEXEC so the drm fd is not inherited by anything the plugin might
        # exec; the dmabuf fds we export are separate and sent explicitly.
        drm_fd = os.open(node, os.O_RDWR | os.O_CLOEXEC)
        dev = gbm.create_device(drm_fd)
        if not dev:
            os.close(drm_fd)
            raise GbmError(f"gbm_create_device failed on {node}")
        self._gbm = gbm
        self._drm_fd: int = drm_fd
        self._dev: int = dev
        self.node = node

    @property
    def _ptr(self) -> int:
        """The device pointer, for LinearBuffer allocation. Raises if closed."""
        if not self._dev:
            raise GbmError("GbmDevice is closed")
        return self._dev

    def close(self) -> None:
        """Destroy the device and close the render-node fd. Idempotent."""
        if self._dev:
            self._gbm.device_destroy(self._dev)
            self._dev = 0
        if self._drm_fd >= 0:
            os.close(self._drm_fd)
            self._drm_fd = -1

    def __enter__(self) -> GbmDevice:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()


def _first_render_node() -> str:
    nodes = sorted(glob.glob("/dev/dri/renderD*"))
    if not nodes:
        raise GbmError("no DRM render node under /dev/dri")
    return nodes[0]


# ---------------------------------------------------------------- LinearBuffer


class LinearBuffer:
    """One linear ARGB8888 GBM buffer object: allocate once, draw many times,
    send the same dmabuf fd each frame (buffer id 0, reused for the buffer's
    lifetime -- the host replaces its texture on each receipt).

    Two ways to fill it:
      - buf.map() -> (memoryview, map_stride): the raw zero-copy path. Draw
        premultiplied BGRA straight into GPU-visible memory. cairo's
        FORMAT_ARGB32 and QImage's Format_ARGB32_Premultiplied match this
        layout byte-for-byte.
      - buf.upload(pil_image): the PIL convenience -- premultiplies RGBA and
        copies it in (PIL has no premultiplied concept, so a copy is
        unavoidable). Built on top of map().

    Stride discipline (the easy thing to get wrong): the *map* stride (from
    gbm_bo_map, what your CPU writes must honour) and the *bo* stride (from
    gbm_bo_get_stride, what goes in the Buffer message) are distinct values
    that often but not always agree. This class keeps them apart -- .stride is
    the bo stride for the wire; map() hands you the map stride for writing.

    The exported dmabuf fd lives as long as the buffer and is closed by
    close()/gbm_bo_destroy. Use as a context manager, or call close()."""

    def __init__(self, dev: GbmDevice, width: int, height: int):
        if not (DIM_MIN <= width <= DIM_MAX and DIM_MIN <= height <= DIM_MAX):
            raise GbmError(f"buffer {width}x{height} outside [{DIM_MIN}, {DIM_MAX}]")
        gbm = _load_gbm()
        bo = gbm.bo_create(dev._ptr, width, height, FOURCC_ARGB8888, GBM_BO_USE_LINEAR)
        if not bo:
            raise GbmError(f"gbm_bo_create failed for {width}x{height}")
        fd = gbm.bo_get_fd(bo)
        if fd < 0:
            gbm.bo_destroy(bo)
            raise GbmError("gbm_bo_get_fd failed")
        self._gbm = gbm
        self._bo: int = bo
        self.width = width
        self.height = height
        self.fd: int = fd
        # The bo stride and modifier are fixed at allocation; read them once.
        # .stride is the *bo* stride -- the value the Buffer message carries.
        self.stride: int = gbm.bo_get_stride(bo)
        self.modifier: int = gbm.bo_get_modifier(bo)

    @contextmanager
    def map(self) -> Iterator[tuple[memoryview, int]]:
        """Map the buffer for CPU writes. Yields (writable memoryview,
        map_stride); the memoryview aliases GPU-visible memory (no copy), and
        map_stride is the row pitch you must step by -- NOT self.stride. Rows
        are top-down. Unmaps on exit. Raises GbmError if the map fails."""
        map_stride = ctypes.c_uint32()
        map_data = ctypes.c_void_p()
        ptr = self._gbm.bo_map(self._bo, self.width, self.height, map_stride, map_data)
        if not ptr:
            raise GbmError("gbm_bo_map failed")
        try:
            # Wrap the raw void* as a writable memoryview over the whole
            # mapping (map_stride rows, height of them) without copying: view
            # the pointer as a byte array of that exact size, then memoryview
            # it. Writes land directly in the buffer libgbm handed us.
            #
            # .cast("B") is load-bearing: a memoryview over a ctypes array has
            # that array's element format, and slice-assignment (mem[a:b] = ...)
            # is only implemented for the plain unsigned-byte format "B" --
            # other formats raise NotImplementedError. c_ubyte already yields
            # "B", and the cast makes the contract explicit and copy-free.
            length = map_stride.value * self.height
            array_t = ctypes.c_ubyte * length
            backing = array_t.from_address(ptr)
            yield memoryview(backing).cast("B"), int(map_stride.value)
        finally:
            self._gbm.bo_unmap(self._bo, map_data)

    def upload(self, pil_image: object) -> None:
        """Copy a PIL RGBA image into the buffer, premultiplying alpha and
        reordering to the dmabuf's BGRA byte layout. The drawing-agnostic raw
        path is map(); this is the PIL sugar on top of it.

        The host blends premultiplied (ONE, ONE_MINUS_SRC_ALPHA), so each
        colour channel is scaled by alpha here. ARGB8888 little-endian is B,G,
        R,A in memory, hence the channel reorder."""
        data = _premultiplied_bgra(pil_image)
        row = self.width * 4  # tightly packed source: 4 bytes/pixel, no pad
        with self.map() as (mem, map_stride):
            # Copy row by row: the source is tightly packed (row bytes) but the
            # destination steps by map_stride, which may be wider. Writing
            # top-down matches PIL's natural row order and the host's texture
            # flip (verified against veiland-label).
            for y in range(self.height):
                dst = y * map_stride
                src = y * row
                mem[dst : dst + row] = data[src : src + row]

    def resize_or_keep(self, dev: GbmDevice, configure: Configure) -> LinearBuffer:
        """Return self if the configure's dimensions match, else free this
        buffer and return a freshly allocated one. The pacer has already
        drained the in-flight buffer's release before handing you a
        RECONFIGURE, so destroying here is safe."""
        if (configure.region_w, configure.region_h) == (self.width, self.height):
            return self
        self.close()
        return LinearBuffer(dev, configure.region_w, configure.region_h)

    def close(self) -> None:
        """Close the dmabuf fd and destroy the bo. Idempotent."""
        if self._bo:
            if self.fd >= 0:
                os.close(self.fd)
                self.fd = -1
            self._gbm.bo_destroy(self._bo)
            self._bo = 0

    def __enter__(self) -> LinearBuffer:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()


# --------------------------------------------------------------- BufferChain


class BufferChain:
    """A two-buffer swap chain for CPU plugins that REDRAW their content.

    Why this exists: the host samples a plugin's dmabuf live and zero-copy at
    the compositor's rate, and keeps sampling the last buffer sent until a new
    one replaces it. A CPU plugin sends no GPU fence, so redrawing a *single*
    LinearBuffer in place mutates the exact memory the host is displaying -- a
    host sample landing inside the draw window shows a half-cleared buffer, i.e.
    a flicker. The flash is probabilistic: its frequency is roughly
    draw_time / frame_time, so a cheap widget flickers rarely (easy to miss) and
    a heavy one flickers constantly. Either way it is a real race.

    The fix is to never redraw the buffer the host is showing. This chain holds
    two LinearBuffers and hands out the one that is NOT in flight: draw into
    acquire(), then send() -- which ships it and flips, so the next acquire()
    returns the other buffer while the host samples the one just sent. (GPU
    plugins avoid this with one buffer because their fence serializes the write
    against the host's sample; the CPU slow path has no fence, so it needs the
    spare buffer instead.)

    Contract: drive this from a FramePacer's RENDER events. The pacer only
    yields RENDER once the previous buffer's BufferReleased has arrived, which
    is what guarantees the buffer acquire() returns is free to overwrite. One
    buffer is in flight at a time; the two buffers carry ids 0 and 1 so the host
    swaps its texture on each send.

    Static plugins that draw ONCE and never change (a fixed logo, a
    non-animated wallpaper) do not need this -- a single LinearBuffer is fine
    because nothing is ever redrawn in place. Reach for BufferChain only when
    the content updates."""

    def __init__(self, dev: GbmDevice, width: int, height: int):
        self._dev = dev
        self._bufs = [
            LinearBuffer(dev, width, height),
            LinearBuffer(dev, width, height),
        ]
        self._front = 0  # index of the buffer acquire() hands out (not in flight)

    @property
    def width(self) -> int:
        return self._bufs[self._front].width

    @property
    def height(self) -> int:
        return self._bufs[self._front].height

    def acquire(self) -> LinearBuffer:
        """The buffer to draw into this frame -- the one the host is NOT
        currently sampling. Fill it (map()/upload()), then call send()."""
        return self._bufs[self._front]

    def send(self, conn: Connection) -> None:
        """Send the acquired buffer to the host, then flip so the next
        acquire() returns the other one. The buffer's index is its buffer id,
        so the host replaces its texture with the just-sent buffer and releases
        the previous one. Call this exactly once per acquire(), after drawing."""
        buf = self._bufs[self._front]
        conn.send_buffer(
            buf.fd,
            self._front,  # buffer id: 0/1, so the host swaps textures
            buf.width,
            buf.height,
            FOURCC_ARGB8888,
            buf.modifier,
            buf.stride,
        )
        self._front ^= 1

    def resize_or_keep(self, dev: GbmDevice, configure: Configure) -> BufferChain:
        """Resize both buffers to the configure's dimensions (or keep them if
        unchanged), and reset the chain so the next acquire() starts clean.
        Safe on RECONFIGURE: the pacer has drained the in-flight release first."""
        self._bufs = [b.resize_or_keep(dev, configure) for b in self._bufs]
        self._front = 0
        return self

    def close(self) -> None:
        """Close both buffers. Idempotent."""
        for b in self._bufs:
            b.close()

    def __enter__(self) -> BufferChain:
        return self

    def __exit__(self, *exc: object) -> None:
        self.close()


def _premultiplied_bgra(pil_image: object) -> bytes:
    """Premultiply a PIL RGBA image's colour channels by its alpha and return
    the bytes in dmabuf B,G,R,A order. Imported lazily: PIL is an example
    dependency, never an SDK one, so the SDK still imports without Pillow
    installed -- only upload() needs it."""
    from PIL import Image, ImageChops

    r, g, b, a = pil_image.split()  # type: ignore[attr-defined]
    return Image.merge(
        "RGBA",
        (
            ImageChops.multiply(b, a),
            ImageChops.multiply(g, a),
            ImageChops.multiply(r, a),
            a,
        ),
    ).tobytes()


# ------------------------------------------------------------------ FramePacer


class Event(Enum):
    """The kinds of event FramePacer.events yields. RENDER means all three
    gates (released/have_cue/dirty) are open -- draw and send now."""

    RENDER = "render"
    RECONFIGURE = "reconfigure"
    TIMEOUT = "timeout"
    FD_READY = "fd_ready"
    SHUTDOWN = "shutdown"


@dataclass(frozen=True)
class FrameEvent:
    """One event from FramePacer.events. `configure` is set only on
    RECONFIGURE; `fd` only on FD_READY."""

    kind: Event
    configure: Configure | None = None
    fd: int | None = None


class FramePacer:
    """Owns the frame-pacing state machine and the event multiplexing, so a
    plugin author never hand-writes the released/have_cue/dirty logic or the
    reconfigure drain (protocol.md 8, the two subtle bits battery.py got right
    inline). Drive it with the events() generator.

    A frame renders only when all three gates are open:
      - released:  the host released the buffer we last sent (or none is in
                   flight). Never draw into a buffer the host may sample.
      - have_cue:  the host sent FrameDone (may arrive before OR after the
                   BufferReleased for the same frame -- both orders handled).
      - dirty:     content changed and wants a repaint.

    self_paced() re-arms `dirty` after every send (render every granted
    frame -- animation). on_demand() renders only after mark_dirty() (a battery
    %, a D-Bus update, a timer tick -- static widgets)."""

    def __init__(self, self_paced: bool):
        self._self_paced = self_paced
        self._released = True  # nothing in flight yet
        self._have_cue = False  # no FrameDone yet
        self._dirty = True  # draw the first frame once configured
        # Bound at the top of events() so the drain helpers can read the socket
        # without threading the Connection through every call.
        self._conn: Connection | None = None

    @classmethod
    def self_paced(cls) -> FramePacer:
        """Render every frame the host grants -- for animation."""
        return cls(self_paced=True)

    @classmethod
    def on_demand(cls) -> FramePacer:
        """Render only after mark_dirty() -- for widgets that change rarely."""
        return cls(self_paced=False)

    def mark_dirty(self) -> None:
        """Request a repaint on the next granted frame."""
        self._dirty = True

    def submitted(self) -> None:
        """Call right after conn.send_buffer(): the buffer is now in flight
        (not released), the cue is consumed, and the frame is clean. A
        self-paced pacer immediately re-arms dirty for the next frame."""
        self._released = False
        self._have_cue = False
        self._dirty = self._self_paced

    def _may_render(self) -> bool:
        return self._released and self._have_cue and self._dirty

    def events(
        self,
        conn: Connection,
        timeout: float | None = None,
        extra_fds: Sequence[int] = (),
    ) -> Iterator[FrameEvent]:
        """Multiplex the host socket, caller fds, and an optional timeout,
        yielding typed events. RENDER is yielded whenever all three gates open;
        the caller draws, sends, and calls submitted(). RECONFIGURE arrives
        with the drain already done. TIMEOUT / FD_READY are refresh hooks
        (mark_dirty() in response). SHUTDOWN is yielded once, then the
        generator ends -- host EOF is the same clean end, never a traceback."""
        self._conn = conn
        extra = list(extra_fds)
        while True:
            if self._may_render():
                yield FrameEvent(Event.RENDER)
                # The caller has (or hasn't) sent + called submitted(); loop
                # back to re-check the gates before blocking in select.
                continue

            readable, _, _ = select.select([conn.fileno(), *extra], [], [], timeout)

            if not readable:
                yield FrameEvent(Event.TIMEOUT)
                continue

            # Service caller fds first; they don't touch pacing state, they
            # just signal the author to do work and probably mark_dirty().
            handled_socket = False
            for fd in readable:
                if fd in extra:
                    yield FrameEvent(Event.FD_READY, fd=fd)
                else:
                    handled_socket = True
            if not handled_socket:
                continue

            try:
                msg = conn.recv_event()
            except HostClosed:
                yield FrameEvent(Event.SHUTDOWN)
                return

            drained = self._apply(msg)
            if drained is _SHUTDOWN_SENTINEL:
                yield FrameEvent(Event.SHUTDOWN)
                return
            if isinstance(drained, Configure):
                yield FrameEvent(Event.RECONFIGURE, configure=drained)

    def _apply(self, msg: ServerMessage) -> object:
        """Fold one host message into the pacing state. Returns the Configure
        (post-drain) to surface as RECONFIGURE, _SHUTDOWN_SENTINEL to end, or
        None for the internal FrameDone/BufferReleased bookkeeping."""
        if msg is FRAME_DONE:
            self._have_cue = True
            return None
        if isinstance(msg, BufferReleased):
            self._released = True
            return None
        if msg is SHUTDOWN:
            return _SHUTDOWN_SENTINEL
        if isinstance(msg, Configure):
            return self._drain_for_reconfigure(msg)
        return None

    def _drain_for_reconfigure(self, cfg: Configure) -> object:
        """A Configure arrived. If a buffer is still in flight, wait out its
        BufferReleased before returning -- reallocating under the host would
        free a buffer it is sampling. While draining, keep honouring FrameDone
        (the cue survives the resize) and Shutdown (exit), and coalesce a newer
        Configure. Returns the latest Configure, or _SHUTDOWN_SENTINEL."""
        latest = cfg
        while not self._released:
            try:
                msg = self._recv_blocking()
            except HostClosed:
                return _SHUTDOWN_SENTINEL
            if isinstance(msg, BufferReleased):
                self._released = True
            elif msg is FRAME_DONE:
                self._have_cue = True  # keep the cue: dropping it would stall
            elif msg is SHUTDOWN:
                return _SHUTDOWN_SENTINEL
            elif isinstance(msg, Configure):
                latest = msg  # a newer size supersedes; keep draining
        return latest

    def _recv_blocking(self) -> ServerMessage:
        # Split out so _drain_for_reconfigure reads without re-selecting: the
        # drain is a tight blocking loop on the socket alone. _conn is always
        # bound here -- the drain only runs from inside events() -- so a None is
        # an SDK bug, not host input; surface it as one rather than an
        # AttributeError (and never as `assert`, which -O would strip).
        conn = self._conn
        if conn is None:
            raise RuntimeError("FramePacer drain ran without a bound Connection")
        else:
            return conn.recv_event()


_SHUTDOWN_SENTINEL = object()
