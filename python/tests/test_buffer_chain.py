# SPDX-License-Identifier: GPL-3.0-or-later
#
# Swap-sequencing tests for BufferChain. These do NOT allocate real GBM buffers
# (that needs a render node, which CI lacks) -- they stub LinearBuffer so the
# test exercises only the chain's logic: which buffer acquire() hands out, that
# send() ships the acquired one with its index as the buffer id, and that the
# two alternate 0,1,0,1. That alternation is the whole point of the chain: it is
# what keeps the host from ever sampling the buffer being drawn (the CPU-plugin
# flicker race). Resize and close fan-out are checked too.

from __future__ import annotations

from typing import Any, cast

import veiland_plugin as vp

# ------------------------------------------------------------------ fakes


class FakeBuffer:
    """Stands in for a LinearBuffer without touching GBM. Carries the fields
    BufferChain.send() reads off it, and records close()."""

    def __init__(self, tag: int, width: int = 100, height: int = 40) -> None:
        self.tag = tag  # so a test can tell the two buffers apart
        self.fd = 1000 + tag
        self.width = width
        self.height = height
        self.stride = width * 4
        self.modifier = 0
        self.closed = False

    def resize_or_keep(self, dev, configure):
        # Match LinearBuffer: keep self if size unchanged, else a new buffer.
        if (configure.region_w, configure.region_h) == (self.width, self.height):
            return self
        return FakeBuffer(self.tag, configure.region_w, configure.region_h)

    def close(self) -> None:
        self.closed = True


class RecordingConn:
    """Records every send_buffer call as a dict, so a test can assert the
    (fd, buf_id) sequence the chain produced."""

    def __init__(self) -> None:
        self.sends: list[dict[str, int]] = []

    def send_buffer(
        self, fd, buf_id, width, height, fourcc, modifier, stride, offset=0
    ):
        self.sends.append(
            {"fd": fd, "buf_id": buf_id, "width": width, "height": height}
        )


def make_chain(monkeypatch: Any) -> vp.BufferChain:
    """A BufferChain whose two buffers are FakeBuffers (tags 0 and 1), with
    LinearBuffer patched out so no GBM allocation happens."""
    tags = iter([0, 1])
    monkeypatch.setattr(
        vp, "LinearBuffer", lambda dev, w, h: FakeBuffer(next(tags), w, h)
    )
    # dev is unused once LinearBuffer is stubbed; cast past the GbmDevice hint.
    return vp.BufferChain(dev=cast(vp.GbmDevice, None), width=100, height=40)


def as_conn(conn: RecordingConn) -> vp.Connection:
    """Type the recording double as a Connection for the chain's send()."""
    return cast(vp.Connection, conn)


NO_DEV = cast(vp.GbmDevice, None)  # unused once LinearBuffer is stubbed


def make_configure(w: int, h: int) -> vp.Configure:
    return vp.Configure(
        region_x=0,
        region_y=0,
        region_w=w,
        region_h=h,
        scale_120=120,
        time_unix_seconds=0,
        time_tz_offset_seconds=0,
        output_name="TEST-1",
    )


# ------------------------------------------------------------------ tests


def test_acquire_send_alternates_buffer_ids(monkeypatch):
    # The core invariant: consecutive frames use different buffers, and each is
    # sent with its own index as the buffer id (0,1,0,1,...). This is what stops
    # the host from sampling the buffer currently being drawn.
    chain = make_chain(monkeypatch)
    conn = RecordingConn()

    for _ in range(4):
        chain.acquire()  # draw would happen here
        chain.send(as_conn(conn))

    ids = [s["buf_id"] for s in conn.sends]
    assert ids == [0, 1, 0, 1]


def test_acquire_returns_the_buffer_that_send_ships(monkeypatch):
    # acquire() and the following send() must target the SAME buffer -- the fd
    # sent is the fd of the buffer just handed to the caller to draw into.
    chain = make_chain(monkeypatch)
    conn = RecordingConn()

    for _ in range(3):
        buf = chain.acquire()
        chain.send(as_conn(conn))
        assert conn.sends[-1]["fd"] == buf.fd


def test_acquire_gives_the_other_buffer_after_send(monkeypatch):
    # After a send, the next acquire() is the OTHER buffer (the one the host is
    # not now sampling), not the one just shipped.
    chain = make_chain(monkeypatch)
    conn = RecordingConn()

    first = chain.acquire()
    chain.send(as_conn(conn))
    second = chain.acquire()
    assert second is not first
    chain.send(as_conn(conn))
    third = chain.acquire()
    assert third is first  # back to the first buffer on the third frame


def test_acquire_is_idempotent_without_send(monkeypatch):
    # Calling acquire() twice without a send in between returns the same buffer
    # (the front hasn't flipped -- only send() flips it).
    chain = make_chain(monkeypatch)
    assert chain.acquire() is chain.acquire()


def test_resize_replaces_buffers_and_resets_front(monkeypatch):
    # A resize to a new size swaps both buffers and resets so the next frame
    # starts from buffer id 0 again.
    chain = make_chain(monkeypatch)
    conn = RecordingConn()

    chain.acquire()
    chain.send(as_conn(conn))  # front is now 1
    chain = chain.resize_or_keep(NO_DEV, make_configure(200, 80))

    assert (chain.width, chain.height) == (200, 80)
    chain.acquire()
    chain.send(as_conn(conn))
    assert conn.sends[-1]["buf_id"] == 0  # reset to front 0
    assert conn.sends[-1]["width"] == 200


def test_resize_same_size_keeps_buffers(monkeypatch):
    # A reconfigure to the identical size keeps the existing buffers (no
    # reallocation), matching LinearBuffer.resize_or_keep.
    chain = make_chain(monkeypatch)
    b0 = chain.acquire()
    chain = chain.resize_or_keep(NO_DEV, make_configure(100, 40))
    # front was reset to 0, and the size matched, so buffer 0 is the same object
    assert chain.acquire() is b0


def test_close_closes_both_buffers(monkeypatch):
    # close() must fan out to both buffers, or a chain leaks an fd + a bo.
    chain = make_chain(monkeypatch)
    bufs = [cast(Any, chain._bufs[0]), cast(Any, chain._bufs[1])]
    chain.close()
    assert all(b.closed for b in bufs)
