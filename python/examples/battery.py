#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The battery widget from the repo-root battery.py, rewritten on the Python
# SDK. Same pixels, but the protocol handshake, the ctypes GBM allocation, the
# premultiply/upload, and the frame-pacing state machine (including the resize
# drain) all move behind veiland_plugin -- the author writes the widget, not
# the protocol. Compare with ../../battery.py, the "no SDK, just the wire"
# reference, to see what the SDK absorbs.
#
# A real plugin vendors veiland_plugin.py next to itself (copy one file, no
# pip). This example instead adds the repo's python/ dir to sys.path so it runs
# straight from the tree.

from __future__ import annotations

import os
import sys
from typing import TYPE_CHECKING

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import glob  # noqa: E402  (after the sys.path shim, so the SDK import resolves)

import veiland_plugin as vp  # noqa: E402

if TYPE_CHECKING:
    # For annotations only: at runtime PIL stays lazily imported inside the
    # draw functions (the SDK's own upload() treats Pillow the same way).
    from PIL import Image

# ------------------------------------------------------------- battery reading


def read_battery() -> int | None:
    for cap in glob.glob("/sys/class/power_supply/*/capacity"):
        try:
            with open(cap) as f:
                return int(f.read().strip())
        except (OSError, ValueError):
            continue
    return None


# ------------------------------------------------------------------- drawing

# Unchanged from the no-SDK reference: the SDK ends at "here is a premultiplied
# buffer with a stride", so the Pillow drawing is identical. The one seam is
# that Configure is now a dataclass (cfg.region_w / cfg.region_h / cfg.scale)
# instead of the hand-parsed dict.


def draw_widget(cfg: vp.Configure, pct: int | None) -> Image.Image:
    from PIL import Image

    s = cfg.scale
    canvas = Image.new("RGBA", (cfg.region_w, cfg.region_h), (0, 0, 0, 0))
    card = draw_card(int(300 * s), int(100 * s), s, pct)
    canvas.paste(card, (int(40 * s), int(40 * s)))
    return canvas


def draw_card(w: int, h: int, s: float, pct: int | None) -> Image.Image:
    from PIL import Image, ImageDraw, ImageFont

    img = Image.new("RGBA", (w, h), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    label = "AC" if pct is None else f"{pct}%"
    level = 100 if pct is None else max(0, min(100, pct))
    color = (
        (80, 220, 120, 255)
        if level > 40
        else (250, 180, 60, 255)
        if level > 15
        else (240, 80, 80, 255)
    )

    # translucent pill background
    d.rounded_rectangle(
        [0, 0, w - 1, h - 1],
        radius=int(14 * s),
        fill=(15, 18, 28, 175),
        outline=(255, 255, 255, 70),
        width=max(1, int(1.5 * s)),
    )

    # battery glyph on the left half
    bx0, by0 = int(16 * s), h // 4
    bx1, by1 = w // 2 - int(8 * s), h - h // 4
    line = max(1, int(2 * s))
    d.rounded_rectangle(
        [bx0, by0, bx1, by1],
        radius=int(4 * s),
        outline=(255, 255, 255, 220),
        width=line,
    )
    nub_h = (by1 - by0) // 3
    d.rectangle(
        [
            bx1 + line,
            (by0 + by1) // 2 - nub_h // 2,
            bx1 + line + max(2, int(4 * s)),
            (by0 + by1) // 2 + nub_h // 2,
        ],
        fill=(255, 255, 255, 220),
    )
    inset = line + max(2, int(2 * s))
    fill_w = int((bx1 - bx0 - 2 * inset) * level / 100)
    if fill_w > 0:
        d.rectangle(
            [bx0 + inset, by0 + inset, bx0 + inset + fill_w, by1 - inset], fill=color
        )

    # percentage on the right half
    try:
        font = ImageFont.load_default(size=int(22 * s))
    except TypeError:  # Pillow < 10.1: no size arg
        font = ImageFont.load_default()
    tx0, ty0, tx1, ty1 = d.textbbox((0, 0), label, font=font)
    d.text(
        ((w * 3) // 4 - (tx1 - tx0) // 2, (h - (ty1 - ty0)) // 2 - ty0),
        label,
        font=font,
        fill=(255, 255, 255, 240),
    )
    return img


# ----------------------------------------------------------------- main


def main() -> None:
    conn = vp.Connection.connect("battery", "0.1.0")  # env fd, handshake, Hello
    cfg = conn.wait_for_configure()
    dev = vp.GbmDevice()
    # BufferChain, not a single LinearBuffer: this widget redraws (the % and bar
    # change), and a CPU plugin redrawing one buffer in place races the host's
    # live sampling -> a flicker. The chain hands out the buffer the host is not
    # showing (via acquire()), so the shown one is never mid-edit. upload() works
    # on the acquired LinearBuffer exactly as before. See veiland_plugin.
    chain = vp.BufferChain(dev, cfg.region_w, cfg.region_h)

    # on_demand: the widget only repaints when the battery reading might have
    # changed. A 30 s TIMEOUT tick drives that refresh with no host message.
    pacer = vp.FramePacer.on_demand()
    for ev in pacer.events(conn, timeout=30.0):
        if ev.kind is vp.Event.RENDER:
            chain.acquire().upload(draw_widget(cfg, read_battery()))
            chain.send(conn)
            pacer.submitted()
        elif ev.kind is vp.Event.RECONFIGURE and ev.configure is not None:
            # (`is not None` narrows for mypy; the SDK always sets .configure
            # on a RECONFIGURE event.)
            cfg = ev.configure
            chain = chain.resize_or_keep(dev, cfg)
            pacer.mark_dirty()
        elif ev.kind is vp.Event.TIMEOUT:
            pacer.mark_dirty()  # re-read the battery percentage
        elif ev.kind is vp.Event.SHUTDOWN:
            break

    chain.close()
    dev.close()
    conn.close()


if __name__ == "__main__":
    main()
