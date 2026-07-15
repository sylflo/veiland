#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# A veiland plugin in Python -- no SDK, no OpenGL. Companion code to
# plugin-python.md: a CPU-rendered battery widget that speaks the wire
# protocol (docs/protocol.md) directly and draws with Pillow into a
# linear GBM buffer allocated via ctypes.

import ctypes
import ctypes.util
import glob
import os
import select
import socket
import struct
import sys

def log(*a):
    print("battery:", *a, file=sys.stderr, flush=True)


# ------------------------------------------------------------------ codec

# client -> host
TAG_HELLO, TAG_BUFFER = 0x0001, 0x0002
# host -> client
TAG_CONFIGURE, TAG_FRAME_DONE, TAG_RELEASED, TAG_SHUTDOWN = 1, 2, 3, 4


def _s(text: str) -> bytes:
    b = text.encode("utf-8")
    return struct.pack("<H", len(b)) + b


def msg_hello(name, version):
    return struct.pack("<H", TAG_HELLO) + _s(name) + _s(version)


def msg_buffer(buf_id, w, h, fourcc, modifier, stride, offset=0):
    return struct.pack("<HIIIIQII", TAG_BUFFER,
                       buf_id, w, h, fourcc, modifier, stride, offset)


def parse_configure(payload):
    """payload = message bytes after the u16 tag."""
    head = "<iiIIIqi"
    x, y, w, h, scale_120, t, tz = struct.unpack_from(head, payload)
    off = struct.calcsize(head)
    (nlen,) = struct.unpack_from("<H", payload, off)
    name = payload[off + 2 : off + 2 + nlen].decode("utf-8")
    return {"x": x, "y": y, "w": w, "h": h,
            "scale": scale_120 / 120.0, "time": t, "tz": tz, "output": name}


# -------------------------------------------------------------- handshake


def connect():
    sock = socket.socket(fileno=int(os.environ["VEILAND_PLUGIN_SOCKET"]))
    sock.send(struct.pack("<I", 1))                     # our version
    reply = sock.recv(4)
    if len(reply) != 4 or struct.unpack("<I", reply)[0] != 1:
        sys.exit("veiland-battery: host rejected protocol version")
    caps_raw = sock.recv(4)
    if len(caps_raw) != 4:
        sys.exit("veiland-battery: host closed during handshake")
    caps = struct.unpack("<I", caps_raw)[0]
    if caps & ~0x1:
        # Reserved capability bits set: host speaks a future dialect.
        # The spec says fail closed rather than guess (protocol.md 5.1).
        sys.exit("veiland-battery: unknown host capabilities")
    sock.send(msg_hello("battery", "0.1.0"))
    return sock


# ---------------------------------------------------- GBM via ctypes

# find_library needs ldconfig/gcc and comes up empty on NixOS; the plain
# soname works there once LD_LIBRARY_PATH carries libgbm (the veiland dev
# shell sets it).
_gbm_path = ctypes.util.find_library("gbm") or "libgbm.so.1"
try:
    gbm = ctypes.CDLL(_gbm_path)
except OSError as e:
    sys.exit(f"veiland-battery: cannot load libgbm ({e})")

for fn, res, args in [
    ("gbm_create_device",   ctypes.c_void_p, [ctypes.c_int]),
    ("gbm_bo_create",       ctypes.c_void_p, [ctypes.c_void_p] + [ctypes.c_uint32] * 4),
    ("gbm_bo_map",          ctypes.c_void_p, [ctypes.c_void_p] + [ctypes.c_uint32] * 5
                                             + [ctypes.POINTER(ctypes.c_uint32),
                                                ctypes.POINTER(ctypes.c_void_p)]),
    ("gbm_bo_unmap",        None,            [ctypes.c_void_p, ctypes.c_void_p]),
    ("gbm_bo_get_fd",       ctypes.c_int,    [ctypes.c_void_p]),
    ("gbm_bo_get_stride",   ctypes.c_uint32, [ctypes.c_void_p]),
    ("gbm_bo_get_modifier", ctypes.c_uint64, [ctypes.c_void_p]),
    ("gbm_bo_destroy",      None,            [ctypes.c_void_p]),
]:
    getattr(gbm, fn).restype = res
    getattr(gbm, fn).argtypes = args

GBM_BO_USE_LINEAR     = 1 << 4
GBM_BO_TRANSFER_WRITE = 1 << 1
FOURCC_ARGB8888       = 0x34325241        # 'AR24'


def open_device():
    nodes = sorted(glob.glob("/dev/dri/renderD*"))
    if not nodes:
        sys.exit("veiland-battery: no DRM render node under /dev/dri")
    drm_fd = os.open(nodes[0], os.O_RDWR | os.O_CLOEXEC)
    dev = gbm.gbm_create_device(drm_fd)
    if not dev:
        sys.exit(f"veiland-battery: gbm_create_device failed on {nodes[0]}")
    log(f"gbm device on {nodes[0]}")
    return drm_fd, dev


def alloc(dev, w, h):
    bo = gbm.gbm_bo_create(dev, w, h, FOURCC_ARGB8888, GBM_BO_USE_LINEAR)
    if not bo:
        sys.exit("veiland-battery: gbm_bo_create failed")
    fd = gbm.gbm_bo_get_fd(bo)             # new fd, we own it
    if fd < 0:
        sys.exit("veiland-battery: gbm_bo_get_fd failed")
    return bo, fd


# ------------------------------------------------------------- drawing


def to_premultiplied_bgra(img) -> bytes:
    from PIL import Image, ImageChops
    r, g, b, a = img.split()
    return Image.merge("RGBA", (ImageChops.multiply(b, a),
                                ImageChops.multiply(g, a),
                                ImageChops.multiply(r, a), a)).tobytes()


def draw_widget(cfg, pct):
    # Imported here, after the handshake: keeps the spawn->Hello window
    # free of heavy module loads (protocol.md 5, "timing is host policy").
    from PIL import Image

    # Configure carries the full surface size; the widget is a small
    # card drawn at a fixed spot inside that transparent canvas — the
    # same model the reference label/clock plugins use. scale converts
    # the card's logical design size to physical pixels.
    s = cfg["scale"]
    canvas = Image.new("RGBA", (cfg["w"], cfg["h"]), (0, 0, 0, 0))
    card = draw_card(int(300 * s), int(100 * s), s, pct)
    canvas.paste(card, (int(40 * s), int(40 * s)))
    return canvas


def draw_card(w, h, s, pct):
    from PIL import Image, ImageDraw, ImageFont

    img = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    label = "AC" if pct is None else f"{pct}%"
    level = 100 if pct is None else max(0, min(100, pct))
    color = ((80, 220, 120, 255) if level > 40 else
             (250, 180, 60, 255) if level > 15 else
             (240, 80, 80, 255))

    # translucent pill background
    d.rounded_rectangle([0, 0, w - 1, h - 1], radius=int(14 * s),
                        fill=(15, 18, 28, 175),
                        outline=(255, 255, 255, 70), width=max(1, int(1.5 * s)))

    # battery glyph on the left half
    bx0, by0 = int(16 * s), h // 4
    bx1, by1 = w // 2 - int(8 * s), h - h // 4
    line = max(1, int(2 * s))
    d.rounded_rectangle([bx0, by0, bx1, by1], radius=int(4 * s),
                        outline=(255, 255, 255, 220), width=line)
    nub_h = (by1 - by0) // 3
    d.rectangle([bx1 + line, (by0 + by1) // 2 - nub_h // 2,
                 bx1 + line + max(2, int(4 * s)), (by0 + by1) // 2 + nub_h // 2],
                fill=(255, 255, 255, 220))
    inset = line + max(2, int(2 * s))
    fill_w = int((bx1 - bx0 - 2 * inset) * level / 100)
    if fill_w > 0:
        d.rectangle([bx0 + inset, by0 + inset,
                     bx0 + inset + fill_w, by1 - inset], fill=color)

    # percentage on the right half
    try:
        font = ImageFont.load_default(size=int(22 * s))
    except TypeError:                       # Pillow < 10.1: no size arg
        font = ImageFont.load_default()
    tx0, ty0, tx1, ty1 = d.textbbox((0, 0), label, font=font)
    d.text(((w * 3) // 4 - (tx1 - tx0) // 2, (h - (ty1 - ty0)) // 2 - ty0),
           label, font=font, fill=(255, 255, 255, 240))
    return img


def upload(bo, w, h, img):
    data = to_premultiplied_bgra(img)
    stride = ctypes.c_uint32()
    handle = ctypes.c_void_p()
    ptr = gbm.gbm_bo_map(bo, 0, 0, w, h, GBM_BO_TRANSFER_WRITE,
                         ctypes.byref(stride), ctypes.byref(handle))
    if not ptr:
        sys.exit("veiland-battery: gbm_bo_map failed")
    # Rows are written top-down, Pillow's natural order. The host's
    # compositor program flips plugin textures such that top-down
    # memory displays upright (verified against veiland-label, which
    # produces the same orientation).
    row = w * 4
    for y in range(h):
        ctypes.memmove(ptr + y * stride.value, data[y * row:(y + 1) * row], row)
    gbm.gbm_bo_unmap(bo, handle)


# ----------------------------------------------------------- event loop


def read_battery():
    for cap in glob.glob("/sys/class/power_supply/*/capacity"):
        try:
            with open(cap) as f:
                return int(f.read().strip())
        except (OSError, ValueError):
            continue
    return None


def main():
    sock = connect()                     # handshake first -- 2 s budget
    drm_fd, dev = open_device()

    cfg = None
    bo = fd = None
    released = True                      # may we draw into the buffer?
    have_cue = False                     # got a FrameDone yet?
    dirty = True                         # widget wants a repaint

    def render_and_send():
        nonlocal released, have_cue, dirty
        img = draw_widget(cfg, read_battery())
        upload(bo, cfg["w"], cfg["h"], img)
        socket.send_fds(sock, [msg_buffer(0, cfg["w"], cfg["h"],
                                          FOURCC_ARGB8888,
                                          gbm.gbm_bo_get_modifier(bo),
                                          gbm.gbm_bo_get_stride(bo))], [fd])
        log(f"sent buffer {cfg['w']}x{cfg['h']} "
            f"stride={gbm.gbm_bo_get_stride(bo)} "
            f"modifier={gbm.gbm_bo_get_modifier(bo):#x}")
        released = False
        have_cue = False                 # consumed; host re-cues on receipt
        dirty = False

    while True:
        if cfg and dirty and released and have_cue:
            render_and_send()

        readable, _, _ = select.select([sock], [], [], 30.0)
        if not readable:
            dirty = True                 # timer tick: refresh the reading
            continue

        data = sock.recv(65536)
        if not data:
            break                        # host gone; we are done
        tag = struct.unpack_from("<H", data)[0]

        if tag == TAG_CONFIGURE:
            new = parse_configure(data[2:])
            log(f"configure: {new['w']}x{new['h']} at ({new['x']},{new['y']}) "
                f"scale={new['scale']} output={new['output']}")
            if bo and (not cfg or (new["w"], new["h"]) != (cfg["w"], cfg["h"])):
                # Wait out an in-flight buffer before freeing it. Keep
                # honouring other tags -- discarding a FrameDone here
                # would eat our render cue and stall the widget.
                while not released:
                    rel = sock.recv(65536)
                    if not rel:
                        return
                    t = struct.unpack_from("<H", rel)[0]
                    if t == TAG_RELEASED:
                        released = True
                    elif t == TAG_FRAME_DONE:
                        have_cue = True
                    elif t == TAG_SHUTDOWN:
                        return
                os.close(fd)
                gbm.gbm_bo_destroy(bo)
                bo = fd = None
            cfg = new
            if bo is None:
                bo, fd = alloc(dev, cfg["w"], cfg["h"])
            dirty = True
        elif tag == TAG_FRAME_DONE:
            log("frame done (render cue)")
            have_cue = True
        elif tag == TAG_RELEASED:
            log("buffer released")
            released = True
        elif tag == TAG_SHUTDOWN:
            log("shutdown")
            break


if __name__ == "__main__":
    main()
