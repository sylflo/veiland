#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# A bluetooth status widget: a monochrome bluetooth glyph in a small pill, inset
# from the top-right corner alongside the battery/wifi/ethernet chips, with an
# OPTIONAL connected-device-name label beside the glyph (show_label, off by
# default; label_pos = top|bottom|left|right, same knob as wifi.py). Sibling of
# wifi.py / ethernet.py -- same SYSTEM-bus + companion + pill structure -- but the
# source is bluez (org.bluez), not NetworkManager, and the state is three-way:
# adapter off, adapter on (nothing connected), or at least one device connected.
# Three glyphs: bluetooth-off.svg, bluetooth-on.svg, bluetooth-connected.svg. The
# device name, when shown, is read event-driven from bluez, not polled from a
# shell. READ-ONLY: it shows state, it never pairs or connects.
#
# Data: bluez on the SYSTEM bus. One GetManagedObjects call enumerates every
# adapter (org.bluez.Adapter1) and device (org.bluez.Device1) at once; we read
# the adapter's Powered and count devices with Connected == true. A power toggle
# or a device connect/disconnect wakes us via PropertiesChanged (its socket on
# the pacer's extra_fds); a slow TIMEOUT tick is the fallback. No adapter / no
# bluez / no bus -> bluetooth-off.svg, never a crash (the no-panic-on-input rule
# -- a locker plugin degrades).
#
# Needs the SVG stack (pygobject3 + librsvg + the Rsvg typelib) AND jeepney; the
# flake's dev shell wires both. A real plugin vendors veiland_plugin.py,
# veiland_dbus.py and veiland_svg.py beside itself; this example adds the repo's
# python/ dir to sys.path so it runs straight from the tree. The script must be
# chmod +x or the host spawn fails with "Permission denied (os error 13)".

from __future__ import annotations

import json
import os
import sys
from typing import Any

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# cairo before veiland_svg: importing pycairo registers the pycairo<->GObject
# foreign bridge in-process, which is what lets librsvg render onto a cairo
# context inside veiland_svg.draw_svg. (E402: after the sys.path shim.)
import cairo  # noqa: E402

import veiland_dbus as vd  # noqa: E402
import veiland_layout as vl  # noqa: E402
import veiland_plugin as vp  # noqa: E402
import veiland_svg as vs  # noqa: E402
import veiland_text as vt  # noqa: E402

ICON_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "icons")
ICON_FILES = [
    "bluetooth-off.svg",
    "bluetooth-on.svg",
    "bluetooth-connected.svg",
]

# --------------------------------------------------------------------- bluez
#
# The stable bluez D-Bus shape:
#   /  (ObjectManager)  GetManagedObjects -> {obj_path: {iface: {prop: value}}}
#     org.bluez.Adapter1  .Powered    bool   adapter radio on/off
#     org.bluez.Device1   .Connected  bool   a paired device is connected
# One GetManagedObjects gives the whole tree, so we read adapter power and count
# connected devices without walking per-object. The companion returns {} on any
# error, so no bluez / no adapter buckets into "off", not a traceback.

BLUEZ = "org.bluez"
ADAPTER_IFACE = "org.bluez.Adapter1"
DEVICE_IFACE = "org.bluez.Device1"


class BluetoothSource:
    # Read-only bluez reader over the shared D-Bus companion. Wakes the plugin on
    # any bluez PropertiesChanged (adapter Powered + device Connected both ride
    # it), so a power toggle or a connect/disconnect repaints immediately; the
    # plugin's TIMEOUT tick is the slow fallback.
    def __init__(self, bus: vd.DBusConnection) -> None:
        self.bus = bus
        # Match the whole bluez subtree: adapters live at /org/bluez/hciN and
        # devices below them, and either can change (Powered on the adapter,
        # Connected on a device). One namespace rule covers all of them; we do
        # not parse the signal -- its arrival means "re-read".
        self.bus.subscribe(
            interface="org.freedesktop.DBus.Properties",
            member="PropertiesChanged",
            path_namespace="/org/bluez",
        )

    def read(self) -> tuple[bool, int, str]:
        # Return (powered, connected_count, name). powered False -> adapter off or
        # absent (-> bluetooth-off). powered True with count 0 -> on but idle
        # (-> bluetooth-on); count > 0 -> at least one device (-> connected). name
        # is the FIRST connected device's Name for the optional label ("" when
        # nothing is connected); with several connected the first found wins --
        # one chip shows one name. Read from the SAME GetManagedObjects walk the
        # count comes from, so label and glyph never disagree. Any bluez/bus
        # failure collapses to (False, 0, "") == "off".
        objects = self.bus.get_managed_objects(bus_name=BLUEZ)
        powered = False
        connected = 0
        name = ""
        for ifaces in objects.values():
            adapter = ifaces.get(ADAPTER_IFACE)
            if adapter is not None and adapter.get("Powered"):
                powered = True
            device = ifaces.get(DEVICE_IFACE)
            if device is not None and device.get("Connected"):
                connected += 1
                if not name:
                    # Name is bluez's human label; fall back to Alias, then "" so
                    # a nameless device shows the glyph alone, never a crash (the
                    # value is external data -- coerce to str, don't trust it).
                    raw = device.get("Name") or device.get("Alias") or ""
                    name = str(raw) if isinstance(raw, str) else ""
        return (powered, connected, name)

    def fileno(self) -> int:
        return self.bus.fileno()

    def drain_signals(self) -> None:
        self.bus.drain_signals()

    def close(self) -> None:
        self.bus.close()


def pick_icon(powered: bool, connected: int) -> str:
    # The whole "logic" of the widget: state -> filename. Adapter off wins; then
    # any connected device shows the "connected" glyph; else the plain "on"
    # glyph. Three states, no numeric bucketing (unlike wifi/battery).
    if not powered:
        return "bluetooth-off.svg"
    if connected > 0:
        return "bluetooth-connected.svg"
    return "bluetooth-on.svg"


LABEL_POSITIONS = ("top", "bottom", "left", "right")


def _label_pos_from(cfg: dict[str, Any]) -> str:
    # Where the device-name label sits relative to the glyph: top | bottom | left
    # | right, all WITHIN this one region (the label is attached to the icon, not
    # placed elsewhere on screen -- that would be a second region). Default
    # "bottom" (the stacked chip look). An unknown value logs one line and falls
    # back, never crashes (the untrusted-input rule, as parse_color does). Kept
    # in step with wifi.py's identical knob.
    raw = cfg.get("label_pos", "bottom")
    if isinstance(raw, str) and raw in LABEL_POSITIONS:
        return raw
    print(
        f"bluetooth: label_pos: expected one of {LABEL_POSITIONS}, got {raw!r}; "
        "using 'bottom'",
        file=sys.stderr,
    )
    return "bottom"


def load_icons() -> dict[str, Any]:
    # Parse every icon once at startup. A missing/corrupt file logs one line and
    # stores None; draw_into then draws just the pill for that state -- a bad
    # asset must never crash the locker or spew a traceback. (Same as
    # battery_svg.py / wifi.py; bluetooth-*.svg ship in python/examples/icons/.)
    # Values are Rsvg.Handle-or-None; gi ships no types, so the handle is Any.
    icons: dict[str, Any] = {}
    for name in ICON_FILES:
        try:
            icons[name] = vs.load_svg(os.path.join(ICON_DIR, name))
        except vs.SvgError as e:
            print(f"bluetooth: {name}: {e}", file=sys.stderr)
            icons[name] = None
    return icons


# ------------------------------------------------------------------- drawing

# Default pill background: the translucent dark navy all the status chips share
# (battery_svg.py / wifi.py / ethernet.py), so they read as one row. Overridable
# per config via pill_color (see main).
PILL_BG = (15 / 255, 18 / 255, 28 / 255, 175 / 255)

# Default color of the optional device-name label beside the glyph -- near-white,
# legible on the dark pill. Overridable via label_color; draw_ellipsized_centered
# takes RGB, so only the first three channels reach the text (show_label = false,
# not an alpha of 0, is how you hide the label). Matches wifi.py's LABEL_FG.
LABEL_FG = (0.95, 0.95, 0.95, 1.0)


def draw_into(
    buf: vp.LinearBuffer,
    handle: Any,
    name: str,
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
    # stride, not buf.stride. Region-split logic mirrors wifi.py's draw_into.
    with buf.map() as (mem, map_stride):
        surface = cairo.ImageSurface.create_for_data(
            mem, cairo.FORMAT_ARGB32, buf.width, buf.height, map_stride
        )
        cr = cairo.Context(surface)
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.set_operator(cairo.OPERATOR_OVER)

        # The buffer IS our region. Glyph + label form ONE unit placed by
        # label_pos: name below (bottom), above (top), or beside (left/right) the
        # glyph, hugging it across `gap`. The label stays ATTACHED to the icon
        # (both inside this one region) -- a free-floating "icon here, name
        # elsewhere" would be two regions, which one plugin cannot do. An empty
        # name draws the glyph alone, centered, byte-identical to the icon-only
        # widget: main() passes "" only when show_label is off; a not-connected
        # state passes the label_disconnected placeholder ("N/A" by default), so
        # the chip keeps its shape and shows offline text, not a blank column.
        w, h = float(buf.width), float(buf.height)
        px = font.size * h

        # Draw the glyph and (optional) label as ONE tightly-stacked unit,
        # measured then placed by hand -- NOT via sub-boxes. Note vt's
        # "draw_ellipsized_centered" centers only VERTICALLY (on cy) and draws the
        # text's LEFT edge at x; it does NOT horizontally center. So we measure the
        # label width ourselves and set its x to sit centered on / beside the
        # glyph. gap is the glyph<->text breathing room. (Identical to wifi.py; a
        # shared veiland_layout helper is the next step now this lives in two files.)
        gap = px * 0.35

        label_w = label_h = 0.0
        if name:
            probe = vt.line_layout(cr, name, w, px, font.weight, font)
            _, logical = probe.get_pixel_extents()
            label_w, label_h = float(logical.width), float(logical.height)

        # pad insets the unit from the region edge so the centered glyph+label
        # keeps EQUAL breathing room all round (no label pressing the bottom rim);
        # glyph_max caps the disc so a tall card does not grow a huge glyph.
        # (Identical to wifi.py.)
        pad = min(w, h) * 0.14
        glyph_max = min(w, h) * 0.62
        if not name:
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

        if not name:
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

        vs.draw_pill(cr, cx, cy, radius, pill_color)
        if handle is not None:
            vs.draw_svg_centered(cr, handle, cx, cy, radius * 1.6, tint=icon_color)

        # The device-name label at its measured top-left. draw_ellipsized (top-left
        # form) takes RGB (text edges anti-alias via the glyph mask, not a color
        # alpha); max_w = label_w + 1 never truncates (the box is sized to the
        # text), and tly is the text's TOP because this variant draws downward.
        if name:
            vt.draw_ellipsized(
                cr,
                name,
                tlx,
                tly,
                label_w + 1.0,
                px,
                label_color[:3],
                weight=font.weight,
                spec=font,
            )

        # Debug border: trace the region box (= buffer edge) when debug_border is
        # set. Off by default (untrusted-input rule).
        if border_on:
            vl.draw_debug_border(cr, w, h, border_color)

        surface.flush()
        surface.finish()


# ----------------------------------------------------------------- main


def main() -> None:
    conn = vp.Connection.connect("bluetooth", "0.1.0")
    cfg = conn.wait_for_configure()

    # Optional theming from [plugin.config], both RGBA 0..1 floats where the
    # fourth channel IS the opacity: pill_color = the chip ([0,0,0,0] = none),
    # icon_color = tints the glyph (default: as authored -- white). Same pair
    # on every status pill; see battery_svg.py, the template.
    plugin_cfg: dict[str, Any] = json.loads(
        os.environ.get("VEILAND_PLUGIN_CONFIG") or "{}"
    )
    pill_color = vs.parse_color(plugin_cfg, "pill_color", PILL_BG, tag="bluetooth")
    icon_color = vs.parse_color(plugin_cfg, "icon_color", None, tag="bluetooth")
    halign, valign = vl.anchor_from_config(plugin_cfg, tag="bluetooth")
    border_on, border_color = vl.debug_border_from_config(plugin_cfg, tag="bluetooth")

    # Optional device-name label beside the glyph. OFF by default -> the widget is
    # byte-identical to the icon-only pill unless the user opts in. When on, the
    # font uses the uniform font_family/font_size keys (a fraction of the region
    # height, like every text widget), label_color themes the text, and label_pos
    # (top|bottom|left|right) places it relative to the glyph inside this region.
    show_label = bool(plugin_cfg.get("show_label", False))
    font = vt.font_from_config(plugin_cfg, tag="bluetooth")
    label_color = vs.parse_color(plugin_cfg, "label_color", LABEL_FG, tag="bluetooth")
    label_pos = _label_pos_from(plugin_cfg)
    # What the label shows when nothing is connected (adapter off / on-but-idle).
    # Defaults to "N/A" rather than "" because with label_pos left/right the chip
    # is a FIXED width -- a blank name column reads as a bug, not as "not
    # connected", so the empty state must show SOMETHING. Set label_disconnected =
    # "" to opt into the clean glyph-only vanish (fine for top/bottom). A
    # non-string logs one line and falls back to "N/A". (Same knob as wifi.py.)
    raw_disc = plugin_cfg.get("label_disconnected", "N/A")
    label_disconnected = raw_disc if isinstance(raw_disc, str) else "N/A"
    if not isinstance(raw_disc, str):
        print(
            f"bluetooth: label_disconnected: expected a string, got {raw_disc!r}; "
            "using 'N/A'",
            file=sys.stderr,
        )

    icons = load_icons()

    # Best-effort D-Bus: if the SYSTEM bus is unreachable, run in a permanent
    # "off" state rather than exiting -- the pill still draws, it just always
    # shows bluetooth-off. (source is None -> no extra_fd, no reads.)
    source: BluetoothSource | None = None
    try:
        bus = vd.DBusConnection.connect("SYSTEM", tag="bluetooth")
        source = BluetoothSource(bus)
    except vd.DBusError as e:
        vd.log("bluetooth", f"no system bus, showing off state: {e}")

    dev = vp.GbmDevice()
    # BufferChain, not a single LinearBuffer: this widget REDRAWS (the icon
    # changes on power/connect), and a CPU plugin redrawing one buffer in place
    # races the host's live sampling -> a flicker. The chain hands out the buffer
    # the host is not showing. (Same rationale as the other status pills.)
    chain = vp.BufferChain(dev, cfg.region_w, cfg.region_h)

    def current_state() -> tuple[Any, str]:
        # ONE D-Bus read feeds both the glyph and the label, so they can never
        # disagree. First resolve the icon + raw device name, then decide the
        # label:
        #   show_label off  -> "" (no label at all; icon-only geometry)
        #   connected       -> the device name
        #   not connected   -> the label_disconnected placeholder ("N/A" default)
        # so a fixed-width chip never shows a blank name column that reads as a bug.
        if source is None:
            icon, name = icons.get("bluetooth-off.svg"), ""
        else:
            powered, connected, name = source.read()
            icon = icons.get(pick_icon(powered, connected))
        if not show_label:
            return icon, ""
        return icon, (name if name else label_disconnected)

    pacer = vp.FramePacer.on_demand()
    # bluez's socket (when present) is an extra fd: a PropertiesChanged wakes us
    # on any power/connect change. A 30s tick is the slow fallback. The read is
    # one cheap round-trip and the icon rarely changes, so redraw-per-wake is
    # fine (no display-signature diff, unlike now-playing).
    extra = [source.fileno()] if source is not None else []
    for ev in pacer.events(conn, timeout=30.0, extra_fds=extra):
        if ev.kind is vp.Event.RENDER:
            icon, name = current_state()
            draw_into(
                chain.acquire(),
                icon,
                name,
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
        elif ev.kind is vp.Event.FD_READY:
            # bluez emitted PropertiesChanged: drain the queued signals (their
            # arrival is the message) and redraw.
            if source is not None:
                source.drain_signals()
            pacer.mark_dirty()
        elif ev.kind is vp.Event.TIMEOUT:
            pacer.mark_dirty()  # slow fallback re-read
        elif ev.kind is vp.Event.SHUTDOWN:
            break

    chain.close()
    dev.close()
    if source is not None:
        source.close()
    conn.close()


if __name__ == "__main__":
    main()
