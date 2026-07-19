#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The same battery widget as battery.py, drawn with pycairo instead of Pillow
# to exercise the SDK's *other* buffer path. battery.py uses buf.upload(pil):
# PIL has no premultiplied concept, so the SDK premultiplies and copies. cairo
# is different: FORMAT_ARGB32 is premultiplied BGRA byte-for-byte -- the exact
# dmabuf layout -- so cairo draws straight into buf.map()'s writable memoryview
# with zero conversion and zero copy. This example is the proof that the SDK is
# drawing-library-agnostic: map() is the raw contract, upload() is only PIL
# sugar on top of it.
#
# Text uses cairo's built-in "toy" font API (show_text): "85%" is trivial
# ASCII that needs no shaping. Real shaping + ellipsization (PangoCairo) shows
# up in the now-playing example, where a long CJK title is the test case.
#
# A real plugin vendors veiland_plugin.py next to itself. This example adds the
# repo's python/ dir to sys.path so it runs straight from the tree.

import math
import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import glob  # noqa: E402  (after the sys.path shim, so the SDK import resolves)

import cairo  # noqa: E402

import veiland_plugin as vp  # noqa: E402

# ------------------------------------------------------------- battery reading


def read_battery():
    for cap in glob.glob("/sys/class/power_supply/*/capacity"):
        try:
            with open(cap) as f:
                return int(f.read().strip())
        except (OSError, ValueError):
            continue
    return None


# ------------------------------------------------------------------- drawing

# Same card as battery.py, expressed in cairo. cairo colours are 0..1 floats
# and premultiplied-aware, so there is no manual premultiply or channel
# reorder -- set_source_rgba + fill and cairo lays down the right bytes.


def rounded_rect(cr, x, y, w, h, r):
    # cairo has no rounded-rectangle primitive; trace one from four arcs.
    r = min(r, w / 2, h / 2)
    cr.new_sub_path()
    cr.arc(x + w - r, y + r, r, -math.pi / 2, 0)
    cr.arc(x + w - r, y + h - r, r, 0, math.pi / 2)
    cr.arc(x + r, y + h - r, r, math.pi / 2, math.pi)
    cr.arc(x + r, y + r, r, math.pi, 3 * math.pi / 2)
    cr.close_path()


def draw_card(cr, w, h, s, pct):
    label = "AC" if pct is None else f"{pct}%"
    level = 100 if pct is None else max(0, min(100, pct))
    if level > 40:
        color = (80 / 255, 220 / 255, 120 / 255)
    elif level > 15:
        color = (250 / 255, 180 / 255, 60 / 255)
    else:
        color = (240 / 255, 80 / 255, 80 / 255)

    # translucent pill background + hairline outline
    rounded_rect(cr, 0, 0, w, h, 14 * s)
    cr.set_source_rgba(15 / 255, 18 / 255, 28 / 255, 175 / 255)
    cr.fill_preserve()
    cr.set_source_rgba(1, 1, 1, 70 / 255)
    cr.set_line_width(max(1.0, 1.5 * s))
    cr.stroke()

    # battery glyph on the left half
    bx0, by0 = 16 * s, h / 4
    bx1, by1 = w / 2 - 8 * s, h - h / 4
    line = max(1.0, 2 * s)
    rounded_rect(cr, bx0, by0, bx1 - bx0, by1 - by0, 4 * s)
    cr.set_source_rgba(1, 1, 1, 220 / 255)
    cr.set_line_width(line)
    cr.stroke()
    # nub
    nub_h = (by1 - by0) / 3
    cr.rectangle(bx1 + line, (by0 + by1) / 2 - nub_h / 2, max(2.0, 4 * s), nub_h)
    cr.fill()
    # fill bar
    inset = line + max(2.0, 2 * s)
    fill_w = (bx1 - bx0 - 2 * inset) * level / 100
    if fill_w > 0:
        cr.set_source_rgb(*color)
        cr.rectangle(bx0 + inset, by0 + inset, fill_w, (by1 - by0) - 2 * inset)
        cr.fill()

    # percentage on the right half (toy text API: no shaping needed for ASCII)
    cr.set_source_rgba(1, 1, 1, 240 / 255)
    cr.select_font_face("sans-serif", cairo.FONT_SLANT_NORMAL, cairo.FONT_WEIGHT_NORMAL)
    cr.set_font_size(22 * s)
    xb, yb, tw, th, _, _ = cr.text_extents(label)
    cr.move_to((w * 3) / 4 - tw / 2 - xb, h / 2 - th / 2 - yb)
    cr.show_text(label)


def draw_into(buf, cfg, pct):
    # The zero-copy path: wrap buf.map()'s memoryview in a cairo surface and
    # draw straight into GPU-visible memory. cairo needs the MAP stride (the
    # pitch of the CPU mapping it writes), not buf.stride (the bo stride sent
    # on the wire) -- map() hands back exactly the one cairo wants.
    with buf.map() as (mem, map_stride):
        surface = cairo.ImageSurface.create_for_data(
            mem, cairo.FORMAT_ARGB32, buf.width, buf.height, map_stride
        )
        cr = cairo.Context(surface)
        # Start from a transparent canvas; the card sits at a fixed inset, the
        # same full-surface transparent-canvas model as battery.py.
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.set_operator(cairo.OPERATOR_OVER)
        s = cfg.scale
        cr.translate(40 * s, 40 * s)
        draw_card(cr, 300 * s, 100 * s, s, pct)
        surface.flush()  # commit cairo's writes before we unmap
        surface.finish()


# ----------------------------------------------------------------- main


def main():
    conn = vp.Connection.connect("battery-cairo", "0.1.0")
    cfg = conn.wait_for_configure()
    dev = vp.GbmDevice()
    # BufferChain, not a single LinearBuffer: this widget redraws (the % and
    # bar change), and a CPU plugin redrawing one buffer in place races the
    # host's live sampling -> a flicker. The chain hands out the buffer the host
    # is not showing, so the shown one is never mid-edit. See veiland_plugin.
    chain = vp.BufferChain(dev, cfg.region_w, cfg.region_h)

    pacer = vp.FramePacer.on_demand()
    for ev in pacer.events(conn, timeout=30.0):
        if ev.kind is vp.Event.RENDER:
            draw_into(chain.acquire(), cfg, read_battery())
            chain.send(conn)
            pacer.submitted()
        elif ev.kind is vp.Event.RECONFIGURE:
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
