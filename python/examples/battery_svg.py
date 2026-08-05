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
# It also carries the OPTIONAL label the sibling status widgets share (show_label,
# off by default; label_pos = top|bottom|left|right): the percent + charging
# state ("64% Discharging"), read from the same /sys read that picks the glyph, so
# text and icon never disagree. Off -> byte-identical to the icon-only pill.
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
import veiland_text as vt  # noqa: E402

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


def battery_label(pct: int | None, charging: bool) -> str:
    # The optional label text: percent + state on one line, e.g. "64% Discharging"
    # or "80% Charging". No battery file (desktop / AC only) -> "AC". Built from
    # the SAME read that picks the icon, so text and glyph never disagree. Kept a
    # single line (the widget draws one ellipsized line, like wifi/bluetooth).
    if pct is None:
        return "AC"
    return f"{pct}% {'Charging' if charging else 'Discharging'}"


LABEL_POSITIONS = ("top", "bottom", "left", "right")


def _label_pos_from(cfg: dict[str, Any]) -> str:
    # Where the label sits relative to the glyph: top | bottom | left | right,
    # all WITHIN this one region. Default "bottom" (the stacked chip look). An
    # unknown value logs one line and falls back, never crashes (the untrusted-
    # input rule). Same knob as wifi.py / bluetooth.py.
    raw = cfg.get("label_pos", "bottom")
    if isinstance(raw, str) and raw in LABEL_POSITIONS:
        return raw
    print(
        f"battery-svg: label_pos: expected one of {LABEL_POSITIONS}, got {raw!r}; "
        "using 'bottom'",
        file=sys.stderr,
    )
    return "bottom"


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

# Default color of the optional label -- near-white, legible on the dark pill.
# Overridable via label_color; draw_ellipsized takes RGB, so only the first three
# channels reach the text (show_label = false, not alpha 0, hides the label).
# Matches wifi.py / bluetooth.py's LABEL_FG.
LABEL_FG = (0.95, 0.95, 0.95, 1.0)


def draw_into(
    buf: vp.LinearBuffer,
    handle: Any,
    label: str,
    font: vt.FontSpec,
    label_color: vs.RGBA,
    label_pos: str,
    pill_color: vs.RGBA,
    icon_color: vs.RGBA | None,
    halign: str,
    valign: str,
    border_on: bool,
    border_color: vs.RGBA,
) -> None:
    # Zero-copy: wrap buf.map()'s memoryview in a cairo surface and draw (pill +
    # SVG + optional label) straight into GPU-visible memory. cairo needs the MAP
    # stride, not buf.stride. The glyph+label geometry mirrors wifi.py/bluetooth.py
    # (a shared veiland_layout helper is the natural next step: this is copy #3).
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

        # Glyph + optional label as ONE measured, padded, centered unit. An empty
        # label (show_label off) draws the glyph alone, centered -- byte-identical
        # to the old icon-only widget.
        w, h = float(buf.width), float(buf.height)
        px = font.size * h
        gap = px * 0.35

        label_w = label_h = 0.0
        if label:
            probe = vt.line_layout(cr, label, w, px, font.weight, font)
            _, logical = probe.get_pixel_extents()
            label_w, label_h = float(logical.width), float(logical.height)

        # pad insets the unit from the region edge so the centered glyph+label
        # keeps EQUAL breathing room; glyph_max caps the disc. (As wifi.py.)
        pad = min(w, h) * 0.14
        glyph_max = min(w, h) * 0.62
        if not label:
            diameter = min(w, h) - 2 * pad
            uw = uh = diameter
        elif label_pos in ("left", "right"):
            diameter = min(glyph_max, h - 2 * pad, w - label_w - gap - 2 * pad)
            uw = diameter + gap + label_w
            uh = max(diameter, label_h)
        else:  # top / bottom
            diameter = min(glyph_max, w - 2 * pad, h - label_h - gap - 2 * pad)
            uw = max(diameter, label_w)
            uh = diameter + gap + label_h
        diameter = max(0.0, diameter)
        radius = diameter / 2

        ox, oy = vl.anchor_offset(halign, valign, w, h, uw, uh)

        if not label:
            cx, cy = ox + uw / 2, oy + uh / 2
            tlx = tly = 0.0
        elif label_pos == "left":
            cx = ox + uw - radius
            cy = oy + uh / 2
            tlx, tly = ox, oy + (uh - label_h) / 2
        elif label_pos == "right":
            cx = ox + radius
            cy = oy + uh / 2
            tlx, tly = ox + diameter + gap, oy + (uh - label_h) / 2
        elif label_pos == "top":
            cx = ox + uw / 2
            cy = oy + uh - radius
            tlx, tly = ox + (uw - label_w) / 2, oy
        else:  # bottom (the default)
            cx = ox + uw / 2
            cy = oy + radius
            tlx, tly = ox + (uw - label_w) / 2, oy + diameter + gap

        # The translucent chip, then the glyph. draw_svg_centered is skipped when
        # the icon failed to load, leaving just the pill. A None icon_color means
        # "as authored" (the shipped icons are white).
        vs.draw_pill(cr, cx, cy, radius, pill_color)
        if handle is not None:
            vs.draw_svg_centered(cr, handle, cx, cy, radius * 1.6, tint=icon_color)

        # The label at its measured top-left (draw_ellipsized draws downward from
        # y; max_w = label_w + 1 never truncates since the box is sized to fit).
        if label:
            vt.draw_ellipsized(
                cr,
                label,
                tlx,
                tly,
                label_w + 1.0,
                px,
                label_color[:3],
                weight=font.weight,
                spec=font,
            )

        # Debug border: trace the region box (= buffer edge) when debug_border is
        # set, so you can see where the host placed the region relative to the
        # pill floating in it. Off by default (untrusted-input rule).
        if border_on:
            vl.draw_debug_border(cr, w, h, border_color)

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

    # Optional label beside the glyph: percent + charging state ("64% Charging").
    # OFF by default -> byte-identical to the icon-only pill unless opted in. Same
    # knobs as wifi.py / bluetooth.py: font_* theme the text, label_color colors
    # it, label_pos (top|bottom|left|right) places it inside this region.
    show_label = bool(plugin_cfg.get("show_label", False))
    font = vt.font_from_config(plugin_cfg, tag="battery-svg")
    label_color = vs.parse_color(plugin_cfg, "label_color", LABEL_FG, tag="battery-svg")
    label_pos = _label_pos_from(plugin_cfg)

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
            # "" when show_label is off -> icon-only geometry, as before.
            label = battery_label(pct, charging) if show_label else ""
            draw_into(
                chain.acquire(),
                handle,
                label,
                font,
                label_color,
                label_pos,
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
