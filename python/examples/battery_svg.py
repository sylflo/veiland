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

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# These follow the sys.path shim, so the SDK imports resolve (E402). cairo is
# imported before veiland_svg on purpose: importing pycairo registers the
# pycairo<->GObject foreign bridge in-process, which is what lets librsvg render
# onto a cairo context inside veiland_svg.draw_svg.
import glob  # noqa: E402

import cairo  # noqa: E402

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


def read_battery():
    # Unchanged from battery_cairo.py: first readable capacity, or None.
    for cap in glob.glob("/sys/class/power_supply/*/capacity"):
        try:
            with open(cap) as f:
                return int(f.read().strip())
        except (OSError, ValueError):
            continue
    return None


def read_battery_state():
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


def pick_icon(pct, charging):
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


def load_icons():
    # Parse every icon once at startup (draw_svg is called many times per icon).
    # A missing or corrupt file logs one line and stores None; draw_into then
    # draws just the empty pill for that state -- a bad asset must never crash
    # the locker or spew a traceback.
    icons = {}
    for name in ICON_FILES:
        try:
            icons[name] = vs.load_svg(os.path.join(ICON_DIR, name))
        except vs.SvgError as e:
            print(f"battery-svg: {name}: {e}", file=sys.stderr)
            icons[name] = None
    return icons


# ------------------------------------------------------------------- drawing


# Translucent dark pill background, matching battery_cairo.py's card colour so
# the examples share a visual language. (r, g, b, a) floats in 0..1 for cairo.
PILL_BG = (15 / 255, 18 / 255, 28 / 255, 175 / 255)


def draw_into(buf, cfg, handle):
    # Zero-copy: wrap buf.map()'s memoryview in a cairo surface and draw (pill +
    # SVG) straight into GPU-visible memory. cairo needs the MAP stride, not
    # buf.stride -- map() hands back the one it wants.
    with buf.map() as (mem, map_stride):
        surface = cairo.ImageSurface.create_for_data(
            mem, cairo.FORMAT_ARGB32, buf.width, buf.height, map_stride
        )
        cr = cairo.Context(surface)
        # Transparent full-surface canvas; the pill sits at a fixed top-right
        # inset (same full-surface model as battery_cairo.py / label / clock).
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.set_operator(cairo.OPERATOR_OVER)
        s = cfg.scale

        # Pill center: inset from the top-right corner, leaving badge_gap room
        # on the right for the keyboard badge that sits beside it on the
        # reference lockscreen.
        radius = 22.0 * s
        badge_gap = 56.0 * s
        cx = buf.width - badge_gap - radius
        cy = 24.0 * s + radius

        # Two calls do the whole widget: the translucent chip, then the glyph
        # centered on it at 80% of the pill so it breathes. draw_svg_centered is
        # skipped when the icon failed to load, leaving just the pill.
        vs.draw_pill(cr, cx, cy, radius, PILL_BG)
        if handle is not None:
            vs.draw_svg_centered(cr, handle, cx, cy, radius * 1.6)

        surface.flush()  # commit cairo's writes before we unmap
        surface.finish()


# ----------------------------------------------------------------- main


def main():
    conn = vp.Connection.connect("battery-svg", "0.1.0")
    cfg = conn.wait_for_configure()
    icons = load_icons()
    dev = vp.GbmDevice()
    buf = vp.LinearBuffer(dev, cfg.region_w, cfg.region_h)

    pacer = vp.FramePacer.on_demand()
    for ev in pacer.events(conn, timeout=30.0):
        if ev.kind is vp.Event.RENDER:
            pct, charging = read_battery_state()
            draw_into(buf, cfg, icons.get(pick_icon(pct, charging)))
            conn.send_buffer(
                buf.fd,
                0,
                buf.width,
                buf.height,
                vp.FOURCC_ARGB8888,
                buf.modifier,
                buf.stride,
            )
            pacer.submitted()
        elif ev.kind is vp.Event.RECONFIGURE:
            cfg = ev.configure
            buf = buf.resize_or_keep(dev, cfg)
            pacer.mark_dirty()
        elif ev.kind is vp.Event.TIMEOUT:
            pacer.mark_dirty()  # re-read the battery state
        elif ev.kind is vp.Event.SHUTDOWN:
            break

    buf.close()
    dev.close()
    conn.close()


if __name__ == "__main__":
    main()
