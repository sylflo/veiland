# SPDX-License-Identifier: GPL-3.0-or-later
#
# Codec tests for the Python SDK. The golden-byte vectors are hand-derived
# from docs/protocol.md 3-7 and cross-checked byte-for-byte against
# veiland-protocol's Rust *_wire_format tests -- passing them proves the two
# implementations agree on the wire, which is the whole point of a second
# independent implementation of the spec.

import os
import socket
import struct

import pytest

import veiland_plugin as vp


# ------------------------------------------------------------------ fixtures


@pytest.fixture
def host(monkeypatch):
    """The host end of a SOCK_SEQPACKET pair; the plugin end is handed to
    Connection.connect via VEILAND_PLUGIN_SOCKET. Yielded (not returned) so
    teardown closes the host socket even if a test fails mid-way -- a bare
    return would leak the fd on any failed assertion."""
    host_sock, plugin = socket.socketpair(socket.AF_UNIX, socket.SOCK_SEQPACKET)
    # Connection.connect takes ownership of the plugin end via fileno; detach
    # so this wrapper's __del__ doesn't close the fd out from under it.
    monkeypatch.setenv("VEILAND_PLUGIN_SOCKET", str(plugin.fileno()))
    plugin.detach()
    yield host_sock
    host_sock.close()


@pytest.fixture
def handshook(host):
    """A Connection past the handshake (host advertises no capabilities),
    paired with the host end for the test to drive. Both are torn down
    automatically: conn here, host by its own fixture."""
    host.send(struct.pack("<I", vp.PROTOCOL_VERSION))
    host.send(struct.pack("<I", 0))
    conn = vp.Connection.connect("battery", "0.1.0")
    host.recv(4)              # client version
    host.recv(vp._RECV_SIZE)  # Hello
    yield conn, host
    conn.close()


# ------------------------------------------------------- golden-byte vectors
#
# Each expected buffer is copied from the corresponding Rust test:
#   Hello           -> client.rs::hello_wire_format
#   Buffer          -> client.rs::buffer_wire_format
#   BufferDestroy   -> client.rs::buffer_destroy_wire_format
#   Configure       -> server.rs::configure_wire_format
#   FrameDone       -> server.rs::frame_done_wire_format
#   BufferReleased  -> server.rs::buffer_released_wire_format
#   Shutdown        -> server.rs::shutdown_roundtrip (tag 0x0004)


def test_hello_wire_format():
    # tag 0x0001, name "hello" (len 5), version "1.0" (len 3).
    expected = bytes(
        [0x01, 0x00, 0x05, 0x00, ord("h"), ord("e"), ord("l"), ord("l"),
         ord("o"), 0x03, 0x00, ord("1"), ord("."), ord("0")]
    )
    assert vp.encode_hello("hello", "1.0") == expected


def test_buffer_wire_format():
    # id 0, 64x64, ARGB8888, LINEAR (modifier 0), stride 256, offset 0.
    expected = bytes(
        [
            0x02, 0x00,                                      # tag
            0x00, 0x00, 0x00, 0x00,                          # id = 0
            0x40, 0x00, 0x00, 0x00,                          # width = 64
            0x40, 0x00, 0x00, 0x00,                          # height = 64
            ord("A"), ord("R"), ord("2"), ord("4"),          # format ARGB8888
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,  # modifier = 0
            0x00, 0x01, 0x00, 0x00,                          # stride = 256
            0x00, 0x00, 0x00, 0x00,                          # offset = 0
        ]
    )
    got = vp.encode_buffer(
        buf_id=0, width=64, height=64, fourcc=vp.FOURCC_ARGB8888,
        modifier=0, stride=256, offset=0,
    )
    assert got == expected
    # ARGB8888 constant really is the 'A','R','2','4' little-endian u32.
    assert struct.pack("<I", vp.FOURCC_ARGB8888) == b"AR24"


def test_buffer_destroy_wire_format():
    # tag 0x0003, id 42.
    assert vp.encode_buffer_destroy(42) == bytes([0x03, 0x00, 0x2A, 0x00, 0x00, 0x00])


def _configure_frame(
    region_x=100, region_y=200, region_w=800, region_h=600, scale_120=120,
    time_unix=1_700_000_000, tz=3600, output_name="DP-1",
) -> bytes:
    """Build a Configure frame the way the host encodes it (server.rs order),
    so decode tests have well-formed input. Not part of the SDK -- the SDK
    only decodes server messages, never encodes them."""
    out = bytearray(struct.pack("<H", vp.TAG_CONFIGURE))
    out += struct.pack("<iiIIIqi", region_x, region_y, region_w, region_h,
                       scale_120, time_unix, tz)
    raw = output_name.encode("utf-8")
    out += struct.pack("<H", len(raw)) + raw
    return bytes(out)


def test_configure_wire_format():
    # The exact 40-byte vector from server.rs::configure_wire_format.
    expected = bytes(
        [
            0x01, 0x00,                                      # tag = Configure
            0x64, 0x00, 0x00, 0x00,                          # region_x = 100
            0xC8, 0x00, 0x00, 0x00,                          # region_y = 200
            0x20, 0x03, 0x00, 0x00,                          # region_w = 800
            0x58, 0x02, 0x00, 0x00,                          # region_h = 600
            0x78, 0x00, 0x00, 0x00,                          # scale_120 = 120
            0x00, 0xF1, 0x53, 0x65, 0x00, 0x00, 0x00, 0x00,  # time = 1_700_000_000
            0x10, 0x0E, 0x00, 0x00,                          # tz = 3600
            0x04, 0x00,                                      # output_name len = 4
            ord("D"), ord("P"), ord("-"), ord("1"),          # "DP-1"
        ]
    )
    # Our helper must reproduce the spec's bytes...
    assert _configure_frame() == expected
    # ...and the SDK must decode them into the expected message.
    cfg = vp.decode_server_message(expected)
    assert cfg == vp.Configure(
        region_x=100, region_y=200, region_w=800, region_h=600,
        scale_120=120, time_unix_seconds=1_700_000_000,
        time_tz_offset_seconds=3600, output_name="DP-1",
    )
    assert cfg.scale == pytest.approx(1.0)


@pytest.mark.parametrize(
    "frame, singleton",
    [
        (bytes([0x02, 0x00]), vp.FRAME_DONE),  # tag 0x0002, empty payload
        (bytes([0x04, 0x00]), vp.SHUTDOWN),    # tag 0x0004, empty payload
    ],
)
def test_singleton_wire_format(frame, singleton):
    # Empty-payload messages decode to the module singletons (compare with `is`).
    assert vp.decode_server_message(frame) is singleton


def test_buffer_released_wire_format():
    frame = bytes([0x03, 0x00, 0x07, 0x00, 0x00, 0x00])  # tag 0x0003, id 7
    assert vp.decode_server_message(frame) == vp.BufferReleased(id=7)


# --------------------------------------------------------------- round-trips


def _decode_client(frame: bytes):
    """Decode a client-direction frame back into its fields. The SDK doesn't
    ship a client decoder (the host does that in Rust) so tests roll a tiny
    one to prove the encoders are self-consistent and reversible."""
    r = vp._Reader(frame)
    tag = r.u16()
    if tag == vp.TAG_HELLO:
        result = ("hello", r.string(vp.PLUGIN_NAME_MAX), r.string(vp.PLUGIN_VERSION_MAX))
    elif tag == vp.TAG_BUFFER:
        result = ("buffer", r.u32(), r.u32(), r.u32(), r.u32(), r.u64(), r.u32(), r.u32())
    elif tag == vp.TAG_BUFFER_DESTROY:
        result = ("destroy", r.u32())
    else:
        raise vp.UnknownTag(tag)
    r.finish()
    return result


def test_hello_roundtrip():
    assert _decode_client(vp.encode_hello("gradient", "0.1")) == ("hello", "gradient", "0.1")


def test_buffer_roundtrip():
    frame = vp.encode_buffer(7, 128, 96, vp.FOURCC_ARGB8888, vp.MODIFIER_INVALID, 512, 0)
    assert _decode_client(frame) == (
        "buffer", 7, 128, 96, vp.FOURCC_ARGB8888, vp.MODIFIER_INVALID, 512, 0,
    )


def test_buffer_destroy_roundtrip():
    assert _decode_client(vp.encode_buffer_destroy(99)) == ("destroy", 99)


def test_configure_roundtrip():
    frame = _configure_frame(region_x=-10, region_y=5, scale_120=240, tz=-3600)
    cfg = vp.decode_server_message(frame)
    assert cfg.region_x == -10 and cfg.region_y == 5
    assert cfg.scale_120 == 240 and cfg.scale == pytest.approx(2.0)
    assert cfg.time_tz_offset_seconds == -3600


def test_configure_multibyte_utf8_output_name():
    # "hello" with an accented e is 6 UTF-8 bytes, not 5 (matches Rust's
    # str_roundtrip_multibyte_utf8). Proves length is bytes, not chars.
    name = "héllo"
    assert len(name.encode("utf-8")) == 6
    cfg = vp.decode_server_message(_configure_frame(output_name=name))
    assert cfg.output_name == name


def test_configure_empty_output_name():
    # Transient hotplug case: an output with no name yet (protocol.md 7.1).
    cfg = vp.decode_server_message(_configure_frame(output_name=""))
    assert cfg.output_name == ""


def test_configure_max_length_output_name():
    name = "a" * vp.OUTPUT_NAME_MAX  # 64 bytes, at the inclusive cap
    cfg = vp.decode_server_message(_configure_frame(output_name=name))
    assert cfg.output_name == name


def test_configure_fractional_scale_accepted():
    # 150 = 1.25x, a common laptop fractional scale.
    cfg = vp.decode_server_message(_configure_frame(scale_120=150))
    assert cfg.scale == pytest.approx(1.25)


# --------------------------------------------------------------- fail-closed


def test_decode_unknown_tag_server():
    with pytest.raises(vp.UnknownTag) as exc:
        vp.decode_server_message(bytes([0x99, 0x00]))
    assert exc.value.tag == 0x0099


def test_decode_unknown_tag_client():
    with pytest.raises(vp.UnknownTag):
        _decode_client(bytes([0x99, 0x00]))


def test_decode_trailing_bytes():
    # FrameDone is empty-payload; an extra byte must be rejected, not ignored.
    with pytest.raises(vp.TrailingBytes):
        vp.decode_server_message(bytes([0x02, 0x00, 0xAA]))


def test_decode_configure_trailing_bytes():
    with pytest.raises(vp.TrailingBytes):
        vp.decode_server_message(_configure_frame() + b"\xaa")


def test_decode_truncated_tag():
    with pytest.raises(vp.Truncated):
        vp.decode_server_message(b"\x01")  # half a u16 tag


def test_decode_truncated_fixed_field():
    # Valid BufferReleased tag, but only 3 of the 4 id bytes.
    with pytest.raises(vp.Truncated):
        vp.decode_server_message(bytes([0x03, 0x00, 0x00, 0x00, 0x00]))


def test_decode_str_length_prefix_truncated():
    # Configure up to output_name, then a single byte of the 2-byte length.
    frame = bytearray(struct.pack("<H", vp.TAG_CONFIGURE))
    frame += struct.pack("<iiIIIqi", 0, 0, 1, 1, 120, 0, 0)
    frame += b"\x00"  # only half the output_name length prefix
    with pytest.raises(vp.Truncated):
        vp.decode_server_message(bytes(frame))


def test_decode_str_claims_more_than_present():
    # output_name length says 10, but no payload bytes follow.
    frame = bytearray(struct.pack("<H", vp.TAG_CONFIGURE))
    frame += struct.pack("<iiIIIqi", 0, 0, 1, 1, 120, 0, 0)
    frame += struct.pack("<H", 10)
    with pytest.raises(vp.Truncated):
        vp.decode_server_message(bytes(frame))


def test_decode_invalid_utf8_output_name():
    # length 1, one byte that is not valid UTF-8 on its own (matches Rust's
    # configure_invalid_utf8_output_name_rejected).
    frame = bytearray(struct.pack("<H", vp.TAG_CONFIGURE))
    frame += struct.pack("<iiIIIqi", 100, 200, 800, 600, 1, 1_700_000_000, 3600)
    frame += struct.pack("<H", 1) + b"\xff"
    with pytest.raises(vp.InvalidUtf8):
        vp.decode_server_message(bytes(frame))


def test_decode_output_name_too_long():
    # length prefix claims 65, over the 64 cap; reject before allocating.
    frame = bytearray(struct.pack("<H", vp.TAG_CONFIGURE))
    frame += struct.pack("<iiIIIqi", 100, 200, 800, 600, 120, 1_700_000_000, 3600)
    frame += struct.pack("<H", 65) + b"a" * 65
    with pytest.raises(vp.StringTooLong) as exc:
        vp.decode_server_message(bytes(frame))
    assert exc.value.max == 64 and exc.value.actual == 65


@pytest.mark.parametrize(
    "region_w, region_h, scale_120",
    [
        (0, 600, 120),      # region_w below min
        (9000, 600, 120),   # region_w above max
        (800, 0, 120),      # region_h below min
        (800, 9000, 120),   # region_h above max
        (800, 600, 0),      # scale below min
        (800, 600, 10000),  # scale above max
    ],
)
def test_decode_configure_out_of_range(region_w, region_h, scale_120):
    frame = _configure_frame(region_w=region_w, region_h=region_h, scale_120=scale_120)
    with pytest.raises(vp.OutOfRange):
        vp.decode_server_message(frame)


@pytest.mark.parametrize("region_w, region_h, scale_120", [(8192, 8192, 9999)])
def test_decode_configure_edge_values_accepted(region_w, region_h, scale_120):
    frame = _configure_frame(region_w=region_w, region_h=region_h, scale_120=scale_120)
    cfg = vp.decode_server_message(frame)
    assert (cfg.region_w, cfg.region_h, cfg.scale_120) == (8192, 8192, 9999)


# Out-of-range on ENCODE -- the direction the SDK adds over the Rust codec,
# which only validates on decode. A plugin bug surfaces here at send time.


@pytest.mark.parametrize(
    "width, height, stride",
    [
        (0, 64, 256),      # width below min
        (9000, 64, 36000), # width above max (stride kept >= width)
        (64, 0, 256),      # height below min
        (64, 9000, 256),   # height above max
        (64, 64, 32),      # stride < width
    ],
)
def test_encode_buffer_out_of_range(width, height, stride):
    with pytest.raises(vp.OutOfRange):
        vp.encode_buffer(0, width, height, vp.FOURCC_ARGB8888, 0, stride)


def test_encode_buffer_edge_dimensions_accepted():
    frame = vp.encode_buffer(0, 8192, 8192, vp.FOURCC_ARGB8888, 0, 8192 * 4)
    # _decode_client returns ("buffer", id, width, height, ...); width/height
    # are at indices 2 and 3.
    assert _decode_client(frame)[2:4] == (8192, 8192)


def test_encode_hello_name_too_long():
    with pytest.raises(vp.StringTooLong) as exc:
        vp.encode_hello("a" * 65, "0.1")
    assert exc.value.max == 64 and exc.value.actual == 65


def test_encode_hello_version_too_long():
    with pytest.raises(vp.StringTooLong) as exc:
        vp.encode_hello("battery", "v" * 33)
    assert exc.value.max == 32 and exc.value.actual == 33


# ------------------------------------------------ socketpair fake-host tests
#
# A cheap in-process "host": one end of a SOCK_SEQPACKET pair plays the host
# (the `host` fixture), the SDK drives the other via VEILAND_PLUGIN_SOCKET.
# No subprocess, no GPU.


def test_handshake_success(host):
    # Host: accept the version, advertise the fence capability.
    host.send(struct.pack("<I", vp.PROTOCOL_VERSION))
    host.send(struct.pack("<I", vp.HOST_CAP_FENCE_FD))

    conn = vp.Connection.connect("battery", "0.1.0")

    # Host reads back: client version, then Hello.
    assert host.recv(4) == struct.pack("<I", vp.PROTOCOL_VERSION)
    assert host.recv(vp._RECV_SIZE) == vp.encode_hello("battery", "0.1.0")
    assert conn.host_capabilities == vp.HOST_CAP_FENCE_FD
    conn.close()


def test_handshake_no_capabilities(host):
    host.send(struct.pack("<I", vp.PROTOCOL_VERSION))
    host.send(struct.pack("<I", 0))  # host supports nothing optional

    conn = vp.Connection.connect("battery", "0.1.0")
    assert conn.host_capabilities == 0
    conn.close()


@pytest.mark.parametrize("caps", [0x2, 0x80000000, 0xDEADBEEF])
def test_handshake_reserved_caps_fail_closed(host, caps):
    host.send(struct.pack("<I", vp.PROTOCOL_VERSION))
    host.send(struct.pack("<I", caps))  # a reserved bit is set
    with pytest.raises(vp.HandshakeError):
        vp.Connection.connect("battery", "0.1.0")


def test_handshake_version_mismatch(host):
    host.send(struct.pack("<I", 2))  # a version the plugin doesn't speak
    with pytest.raises(vp.HandshakeError):
        vp.Connection.connect("battery", "0.1.0")


def test_handshake_host_closed_early(host):
    host.close()  # host gone before sending its version
    with pytest.raises(vp.HandshakeError):
        vp.Connection.connect("battery", "0.1.0")


# The env-var faults share no host socket, so they don't use the `host`
# fixture -- but each arranges a different broken env, so they stay separate
# (parametrize is for varying data, not varying setup).


def test_missing_env_var(monkeypatch):
    monkeypatch.delenv("VEILAND_PLUGIN_SOCKET", raising=False)
    with pytest.raises(vp.HandshakeError):
        vp.Connection.connect("battery", "0.1.0")


def test_non_integer_env_var(monkeypatch):
    monkeypatch.setenv("VEILAND_PLUGIN_SOCKET", "not-a-number")
    with pytest.raises(vp.HandshakeError):
        vp.Connection.connect("battery", "0.1.0")


# ---------------------------------------------- wait_for_configure / events


def test_wait_for_configure(handshook):
    conn, host = handshook
    host.send(_configure_frame(output_name="eDP-1"))
    assert conn.wait_for_configure().output_name == "eDP-1"


def test_recv_event_frame_done_then_released(handshook):
    conn, host = handshook
    host.send(bytes([0x02, 0x00]))                          # FrameDone
    host.send(bytes([0x03, 0x00, 0x00, 0x00, 0x00, 0x00]))  # BufferReleased id 0
    assert conn.recv_event() is vp.FRAME_DONE
    assert conn.recv_event() == vp.BufferReleased(id=0)


def test_recv_event_host_closed(handshook):
    conn, host = handshook
    host.close()
    with pytest.raises(vp.HostClosed):
        conn.recv_event()


def test_wait_for_configure_shutdown_is_clean_exit(handshook):
    conn, host = handshook
    host.send(bytes([0x04, 0x00]))  # Shutdown before any Configure
    with pytest.raises(vp.HostClosed):
        conn.wait_for_configure()


def test_send_buffer_carries_one_fd(handshook):
    conn, host = handshook
    # A real fd to pass; any fd works -- we only assert count and framing.
    r, w = socket.socketpair()
    try:
        conn.send_buffer(
            dmabuf_fd=r.fileno(), buf_id=0, width=64, height=64,
            fourcc=vp.FOURCC_ARGB8888, modifier=0, stride=256,
        )
        msg, fds, _flags, _addr = socket.recv_fds(host, vp._RECV_SIZE, 1)
        assert msg == vp.encode_buffer(0, 64, 64, vp.FOURCC_ARGB8888, 0, 256)
        assert len(fds) == 1  # exactly one fd: the dmabuf, never a fence
        for fd in fds:
            os.close(fd)
    finally:
        r.close()
        w.close()
