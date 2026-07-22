#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# A battery status widget drawn from SVG icons instead of hand-traced cairo
# geometry (battery_cairo.py). This is the copy-me template for the status-icon
# pattern: an if/else picks a bucketed icon file, and the optional veiland_svg
# companion renders it -- via librsvg -- straight onto the same cairo context
# that writes into buf.map(). Clone this for wifi/bluetooth/etc: swap the data
# source and the icon set, keep the loop.
#
# The glyph sits in a small circular translucent pill inset from the top-right
# corner, matching the reference lockscreen's status cluster. (The keyboard
# badge that sits beside it there needs a core change to forward the layout, so
# it is not part of this example; the pill is inset to leave room for it.)
#
# Unlike battery_cairo.py this needs the SVG stack: pygobject3 + librsvg + the
# Rsvg typelib on GI_TYPELIB_PATH (the flake's dev shell wires it). The icons
# live in ./icons/ next to this file. A real plugin vendors veiland_plugin.py
# AND veiland_svg.py beside itself; this example adds the repo's python/ dir to
# sys.path so it runs straight from the tree.

from __future__ import annotations

import os
import sys
from typing import Any

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# These follow the sys.path shim, so the SDK imports resolve (E402). cairo is
# imported before veiland_svg on purpose: importing pycairo registers the
# pycairo<->GObject foreign bridge in-process, which is what lets librsvg render
# onto a cairo context inside veiland_svg.draw_svg.
import glob  # noqa: E402
import json  # noqa: E402

import cairo  # noqa: E402

import veiland_layout as vl  # noqa: E402
import veiland_plugin as vp  # noqa: E402
import veiland_svg as vs  # noqa: E402

ICON_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "icons")
ICON_FILES = [
    "battery-25.svg",
    "battery-50.svg",
    "battery-75.svg",
    "battery-100.svg",
    "battery-charging.svg",
]

# ------------------------------------------------------------- battery reading


def read_battery() -> int | None:
    # Unchanged from battery_cairo.py: first readable capacity, or None.
    for cap in glob.glob("/sys/class/power_supply/*/capacity"):
        try:
            with open(cap) as f:
                return int(f.read().strip())
        except (OSError, ValueError):
            continue
    return None


def read_battery_state() -> tuple[int | None, bool]:
    # Percentage plus whether any supply reports it is actively charging.
    # "Full"/"Discharging"/"Not charging"/"Unknown" all read as not charging.
    pct = read_battery()
    charging = False
    for st in glob.glob("/sys/class/power_supply/*/status"):
        try:
            with open(st) as f:
                if f.read().strip() == "Charging":
                    charging = True
                    break
        except OSError:
            continue
    return pct, charging


def pick_icon(pct: int | None, charging: bool) -> str:
    # The whole "logic" of a status widget: state -> filename. Charging wins;
    # None means no battery file (desktop / AC only) -> show the plugged glyph.
    # Thresholds are the midpoints between the 25/50/75/100 buckets.
    if charging or pct is None:
        return "battery-charging.svg"
    if pct >= 88:
        return "battery-100.svg"
    if pct >= 63:
        return "battery-75.svg"
    if pct >= 38:
        return "battery-50.svg"
    return "battery-25.svg"


def load_icons() -> dict[str, Any]:
    # Parse every icon once at startup (draw_svg is called many times per icon).
    # The values are Rsvg.Handle-or-None; gi ships no types, so the handle is
    # Any to mypy -- opaque here anyway, it only round-trips into veiland_svg.
    # A missing or corrupt file logs one line and stores None; draw_into then
    # draws just the empty pill for that state -- a bad asset must never crash
    # the locker or spew a traceback.
    icons: dict[str, Any] = {}
    for name in ICON_FILES:
        try:
            icons[name] = vs.load_svg(os.path.join(ICON_DIR, name))
        except vs.SvgError as e:
            print(f"battery-svg: {name}: {e}", file=sys.stderr)
            icons[name] = None
    return icons


# ------------------------------------------------------------------- drawing


# Default pill background: the translucent dark navy all the status chips share,
# matching battery_cairo.py's card colour. (r, g, b, a) floats in 0..1 for cairo;
# overridable per config via pill_color (see main).
PILL_BG = (15 / 255, 18 / 255, 28 / 255, 175 / 255)


def draw_into(
    buf: vp.LinearBuffer,
    handle: Any,
    pill_color: vs.RGBA,
    icon_color: vs.RGBA | None,
    halign: str,
    valign: str,
    border_on: bool,
    border_color: vs.RGBA,
) -> None:
    # Zero-copy: wrap buf.map()'s memoryview in a cairo surface and draw (pill +
    # SVG) straight into GPU-visible memory. cairo needs the MAP stride, not
    # buf.stride -- map() hands back the one it wants.
    with buf.map() as (mem, map_stride):
        surface = cairo.ImageSurface.create_for_data(
            mem, cairo.FORMAT_ARGB32, buf.width, buf.height, map_stride
        )
        cr = cairo.Context(surface)
        # Transparent canvas; the pill fills this buffer, which the host has
        # sized to our [plugin.region]. WHERE on screen the region sits is the
        # host's job (config anchor / pixels); we only fill our own box.
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.set_operator(cairo.OPERATOR_OVER)

        # Layer 2 -- content inside the region: the pill is a circle of diameter
        # 2*radius; the content-anchor convention (veiland_layout) parks that
        # bounding square at content_halign/content_valign within our own box. The
        # 4px inset keeps the chip off the region edge. Default center/center is a
        # true no-op: (w - 2r)/2 + r == w/2, exactly the old cx = w/2.
        w, h = float(buf.width), float(buf.height)
        radius = min(w, h) / 2 - 4
        block = 2 * radius
        x, y = vl.anchor_offset(halign, valign, w, h, block, block)
        cx, cy = x + radius, y + radius

        # Two calls do the whole widget: the translucent chip, then the glyph
        # centered on it at 80% of the pill so it breathes. draw_svg_centered is
        # skipped when the icon failed to load, leaving just the pill. A None
        # icon_color means "as authored" (the shipped icons are white).
        vs.draw_pill(cr, cx, cy, radius, pill_color)
        if handle is not None:
            vs.draw_svg_centered(cr, handle, cx, cy, radius * 1.6, tint=icon_color)

        # Debug border: trace the region box (= buffer edge) when debug_border is
        # set, so you can see where the host placed the region relative to the
        # pill floating in it. Off by default (untrusted-input rule).
        if border_on:
            cr.set_source_rgba(*border_color)
            cr.set_line_width(1.0)
            cr.rectangle(0.5, 0.5, w - 1.0, h - 1.0)
            cr.stroke()

        surface.flush()  # commit cairo's writes before we unmap
        surface.finish()


# ----------------------------------------------------------------- main


def main() -> None:
    conn = vp.Connection.connect("battery-svg", "0.1.0")
    cfg = conn.wait_for_configure()

    # Optional theming from [plugin.config], both RGBA 0..1 floats where the
    # fourth channel IS the opacity (no separate knob):
    #   pill_color = the chip ([0, 0, 0, 0] draws no chip at all)
    #   icon_color = tints the glyph (default: as authored -- white; pick a
    #                dark tint if you pick a light pill_color)
    plugin_cfg: dict[str, Any] = json.loads(
        os.environ.get("VEILAND_PLUGIN_CONFIG") or "{}"
    )
    pill_color = vs.parse_color(plugin_cfg, "pill_color", PILL_BG, tag="battery-svg")
    icon_color = vs.parse_color(plugin_cfg, "icon_color", None, tag="battery-svg")
    halign, valign = vl.anchor_from_config(plugin_cfg, tag="battery-svg")
    border_on, border_color = vl.debug_border_from_config(plugin_cfg, tag="battery-svg")

    icons = load_icons()
    dev = vp.GbmDevice()
    # BufferChain, not a single LinearBuffer: this widget REDRAWS (the icon
    # changes with the battery level), and a CPU plugin that redraws one buffer
    # in place races the host's live sampling -> a flicker. The chain hands out
    # the buffer the host is not showing, so the shown one is never mid-edit.
    # (Any status widget cloned from this one redraws too -- keep the chain.)
    chain = vp.BufferChain(dev, cfg.region_w, cfg.region_h)

    pacer = vp.FramePacer.on_demand()
    for ev in pacer.events(conn, timeout=30.0):
        if ev.kind is vp.Event.RENDER:
            pct, charging = read_battery_state()
            handle = icons.get(pick_icon(pct, charging))
            draw_into(
                chain.acquire(),
                handle,
                pill_color,
                icon_color,
                halign,
                valign,
                border_on,
                border_color,
            )
            chain.send(conn)
            pacer.submitted()
        elif ev.kind is vp.Event.RECONFIGURE and ev.configure is not None:
            # (`is not None` narrows for mypy; the SDK always sets .configure
            # on a RECONFIGURE event.)
            cfg = ev.configure
            chain = chain.resize_or_keep(dev, cfg)
            pacer.mark_dirty()
        elif ev.kind is vp.Event.TIMEOUT:
            pacer.mark_dirty()  # re-read the battery state
        elif ev.kind is vp.Event.SHUTDOWN:
            break

    chain.close()
    dev.close()
    conn.close()


if __name__ == "__main__":
    main()
