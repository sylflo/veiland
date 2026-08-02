#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The shape widget: ONE rounded, colored, alpha-blended rectangle filling its
# region -- the backdrop/card primitive. It is READ-ONLY and STATIC: it draws
# once and never changes, no data source, no polling, no keystroke ever reaches
# it. The veiland answer to hyprlock's `shape` block.
#
# What it is FOR: the semi-transparent cards and tinted pills that other widgets
# sit on top of -- the dark login/clock/profile cards and the tan status pills in
# a layout like hyprlock's. There is no grouping primitive: you stack a shape
# UNDER a content widget by giving them the same region and the content plugin a
# higher z_index (paint order), exactly as hyprlock stacks label-over-shape by
# zindex. WHERE the card sits and HOW BIG it is are the host's job (the [[plugin]]
# region anchor); this plugin only paints its assigned region.
#
# What it is NOT: it does not BLUR. A frosted-glass card is the wallpaper's
# blur_region (OpenGL), not this. This is a flat translucent tint. It draws no
# text or icon -- put a markup/label/svg widget on top for that. One solid
# rounded fill, nothing else.
#
# radius matches the wallpaper's blur_region exactly so ONE value means ONE thing
# across both: a fraction of the region HEIGHT (0.0 = hard corners), clamped to
# half the shorter side (so an over-large value becomes a pill/circle). The
# wallpaper is OpenGL and aspect-corrects in UV space; cairo works in pixel space
# where radius*height is already a true circle, so no correction is needed here --
# the same config value yields the same corner curve in both plugins.
#
# This needs only cairo (no gi/Pango, no D-Bus). A real plugin vendors
# veiland_plugin.py and veiland_svg.py next to itself; this example adds the
# repo's python/ dir to sys.path so it runs from the tree. The script must be
# chmod +x or the host spawn fails with "Permission denied (os error 13)".

from __future__ import annotations

import json
import math
import os
import sys
from typing import Any

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

import cairo  # noqa: E402

import veiland_plugin as vp  # noqa: E402
import veiland_svg as vs  # noqa: E402

# Default fill: the tan the status pills use in the reference layout
# (204/164/124), RGBA 0..1 where the fourth channel IS the opacity. Overridable
# via `color` in [plugin.config]. There is no "off" default -- a shape with no
# color is pointless, so it always paints something.
COLOR = (204 / 255, 164 / 255, 124 / 255, 1.0)


def rounded_rect(
    cr: cairo.Context[cairo.ImageSurface],
    x: float,
    y: float,
    w: float,
    h: float,
    r: float,
) -> None:
    # cairo has no rounded-rectangle primitive; trace one from four arcs. r is
    # clamped to half the shorter side, so an over-large radius gives a full
    # capsule/circle -- the same clamp the wallpaper's blur-region SDF applies,
    # kept identical so `radius` reads the same in both plugins. (Same helper as
    # markup.py / avatar.py.)
    r = min(r, w / 2, h / 2)
    cr.new_sub_path()
    cr.arc(x + w - r, y + r, r, -math.pi / 2, 0)
    cr.arc(x + w - r, y + h - r, r, 0, math.pi / 2)
    cr.arc(x + r, y + h - r, r, math.pi / 2, math.pi)
    cr.arc(x + r, y + r, r, math.pi, 3 * math.pi / 2)
    cr.close_path()


def draw_into(buf: vp.LinearBuffer, color: vs.RGBA, radius_frac: float) -> None:
    # Zero-copy: wrap buf.map()'s memoryview in a cairo surface and fill straight
    # into GPU-visible memory. cairo needs the MAP stride, not buf.stride.
    # FORMAT_ARGB32 is premultiplied, matching the host's ONE/1-SRC_ALPHA blend,
    # so a translucent fill composites correctly over lower z-layers (same as the
    # markup/wifi chips).
    with buf.map() as (mem, map_stride):
        surface = cairo.ImageSurface.create_for_data(
            mem, cairo.FORMAT_ARGB32, buf.width, buf.height, map_stride
        )
        cr = cairo.Context(surface)
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.set_operator(cairo.OPERATOR_OVER)

        # Fill the WHOLE region (the buffer IS the region; the host owns WHERE it
        # sits). radius is a fraction of the region HEIGHT -> pixels; rounded_rect
        # clamps it to half the shorter side.
        w, h = float(buf.width), float(buf.height)
        rounded_rect(cr, 0.0, 0.0, w, h, radius_frac * h)
        cr.set_source_rgba(*color)
        cr.fill()

        surface.flush()  # commit cairo's writes before we unmap
        surface.finish()


def resolve_radius(cfg: dict[str, Any]) -> float:
    # radius as a non-negative fraction of region height (0.0 = hard corners),
    # matching the wallpaper's blur_region. Absent -> 0.0; a non-number or
    # negative value logs one line and falls back -- a bad config line mis-rounds
    # the corner at worst, it never crashes the widget (the untrusted-input rule).
    raw = cfg.get("radius", 0.0)
    try:
        val = float(raw)
    except (TypeError, ValueError):
        print(
            f"shape: radius: expected a number, got {raw!r}; using 0.0",
            file=sys.stderr,
        )
        return 0.0
    if val < 0.0:
        print(
            f"shape: radius: expected a value >= 0, got {val}; using 0.0",
            file=sys.stderr,
        )
        return 0.0
    return val


def main() -> None:
    conn = vp.Connection.connect("shape", "0.1.0")
    cfg = conn.wait_for_configure()

    plugin_cfg: dict[str, Any] = json.loads(
        os.environ.get("VEILAND_PLUGIN_CONFIG") or "{}"
    )
    color = vs.parse_color(plugin_cfg, "color", COLOR, tag="shape")
    radius = resolve_radius(plugin_cfg)

    dev = vp.GbmDevice()
    # BufferChain, not a single LinearBuffer: even though a shape never redraws
    # while running, resize (Reconfigure) hands out a fresh buffer, and using the
    # chain keeps the house style uniform. It costs nothing for a static widget.
    chain = vp.BufferChain(dev, cfg.region_w, cfg.region_h)

    # Static widget: no tick (timeout=None). It draws once on the first RENDER and
    # again only when the region resizes. A shape has no clock and no data source.
    pacer = vp.FramePacer.on_demand()
    for ev in pacer.events(conn, timeout=None):
        if ev.kind is vp.Event.RENDER:
            draw_into(chain.acquire(), color, radius)
            chain.send(conn)
            pacer.submitted()
        elif ev.kind is vp.Event.RECONFIGURE and ev.configure is not None:
            # (`is not None` narrows for mypy; the SDK always sets .configure
            # on a RECONFIGURE event.)
            cfg = ev.configure
            chain = chain.resize_or_keep(dev, cfg)
            pacer.mark_dirty()
        elif ev.kind is vp.Event.SHUTDOWN:
            break

    chain.close()
    dev.close()
    conn.close()


if __name__ == "__main__":
    main()
