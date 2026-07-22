#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# An ethernet (wired) status widget: a monochrome plug/port glyph in a small
# pill, inset from the top-right corner alongside the battery and wifi chips.
# Sibling of wifi.py -- same NetworkManager SYSTEM-bus source and the same
# battery_svg.py pill structure -- but the WIRED device, whose state is just up
# vs down (a cable carries or it does not; there is no 0..100 to bucket). Two
# glyphs: ethernet-up.svg when the wired link is active, ethernet-down.svg when
# it is not (unplugged, no device, or no bus). READ-ONLY: it shows link, it
# never connects.
#
# Data: NetworkManager on the SYSTEM bus (org.freedesktop.NetworkManager). We
# find the wired device (DeviceType 1) and read its State; ACTIVATED (100) is
# "up", everything else is "down". A device state change wakes us via
# PropertiesChanged (its socket on the pacer's extra_fds); a slow TIMEOUT tick is
# the fallback. No wired device / no bus -> ethernet-down.svg, never a crash (the
# no-panic-on-input rule -- a locker plugin degrades). NM also exposes the
# negotiated Speed on Device.Wired, but this example stays icon-only (no text
# label): a bucketed speed tier would want a second icon axis and pulls Pango
# into an otherwise SVG-only pill, so it is deliberately left out.
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

ICON_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "icons")
ICON_FILES = [
    "ethernet-up.svg",
    "ethernet-down.svg",
]

# ------------------------------------------------------------- NetworkManager
#
# The stable NetworkManager D-Bus shape (same as wifi.py, wired branch):
#   /org/freedesktop/NetworkManager  .Devices           -> [device object paths]
#   <device>  .Device  .DeviceType  u32   1 == ETHERNET
#             .Device  .State       u32   100 == ACTIVATED (link up)
# The negotiated speed (Device.Wired.Speed, Mb/s) exists but is unused here --
# see the module header on why this stays icon-only. All reads go through the
# veiland_dbus companion, which returns None/{} on any error, so a vanished
# device or a bus hiccup buckets into "down", not a traceback.

NM = "org.freedesktop.NetworkManager"
NM_PATH = "/org/freedesktop/NetworkManager"
NM_IFACE = NM
DEV_IFACE = "org.freedesktop.NetworkManager.Device"

NM_DEVICE_TYPE_ETHERNET = 1
NM_STATE_ACTIVATED = 100


class EthernetSource:
    # Read-only NetworkManager wired reader over the shared D-Bus companion.
    # Wakes the plugin on any NetworkManager PropertiesChanged (a cable
    # plug/unplug flips the device State), so a link change repaints
    # immediately; the plugin's TIMEOUT tick is the slow fallback.
    def __init__(self, bus: vd.DBusConnection) -> None:
        self.bus = bus
        # Match the whole NetworkManager subtree, like wifi.py: the wired
        # device's State lives under /org/freedesktop/NetworkManager and we do
        # not parse the signal -- its arrival means "re-read".
        self.bus.subscribe(
            interface="org.freedesktop.DBus.Properties",
            member="PropertiesChanged",
            path_namespace=NM_PATH,
        )

    def _wired_device(self) -> str | None:
        # The first ETHERNET device's object path, or None. NetworkManager lists
        # all devices under .Devices; we filter by DeviceType.
        paths = self.bus.get_prop(NM_PATH, NM_IFACE, "Devices", bus_name=NM)
        if not paths:
            return None
        for path in paths:
            dtype = self.bus.get_prop(path, DEV_IFACE, "DeviceType", bus_name=NM)
            if dtype == NM_DEVICE_TYPE_ETHERNET:
                return str(path)
        return None

    def read(self) -> bool:
        # Return True if a wired link is up (device present AND activated), else
        # False. No device, not activated, or any D-Bus failure all collapse to
        # False == "down" -- the single honest bucket for "no working cable".
        dev = self._wired_device()
        if dev is None:
            return False
        state = self.bus.get_prop(dev, DEV_IFACE, "State", bus_name=NM)
        return state == NM_STATE_ACTIVATED

    def fileno(self) -> int:
        return self.bus.fileno()

    def drain_signals(self) -> None:
        self.bus.drain_signals()

    def close(self) -> None:
        self.bus.close()


def pick_icon(up: bool) -> str:
    # The whole "logic" of the widget: link up -> the connected glyph, else the
    # unplugged glyph. Two states, no bucketing (unlike wifi/battery): a wired
    # link either carries or it does not.
    return "ethernet-up.svg" if up else "ethernet-down.svg"


def load_icons() -> dict[str, Any]:
    # Parse every icon once at startup. A missing/corrupt file logs one line and
    # stores None; draw_into then draws just the pill for that state -- a bad
    # asset must never crash the locker or spew a traceback. (Same as
    # battery_svg.py / wifi.py; ethernet-*.svg ship in python/examples/icons/.)
    # Values are Rsvg.Handle-or-None; gi ships no types, so the handle is Any.
    icons: dict[str, Any] = {}
    for name in ICON_FILES:
        try:
            icons[name] = vs.load_svg(os.path.join(ICON_DIR, name))
        except vs.SvgError as e:
            print(f"ethernet: {name}: {e}", file=sys.stderr)
            icons[name] = None
    return icons


# ------------------------------------------------------------------- drawing

# Default pill background: the translucent dark navy all the status chips share
# (battery_svg.py / wifi.py), so they read as one row. Overridable per config via
# pill_color (see main).
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
    # buf.stride. Identical structure to battery_svg.py / wifi.py.
    with buf.map() as (mem, map_stride):
        surface = cairo.ImageSurface.create_for_data(
            mem, cairo.FORMAT_ARGB32, buf.width, buf.height, map_stride
        )
        cr = cairo.Context(surface)
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.set_operator(cairo.OPERATOR_OVER)

        # The buffer IS our region; the content-anchor convention (veiland_layout)
        # parks the pill's 2*radius bounding square at content_halign/content_valign.
        # Default center/center is a no-op: (w - 2r)/2 + r == w/2 (the old cx).
        w, h = float(buf.width), float(buf.height)
        radius = min(w, h) / 2 - 4
        block = 2 * radius
        x, y = vl.anchor_offset(halign, valign, w, h, block, block)
        cx, cy = x + radius, y + radius

        vs.draw_pill(cr, cx, cy, radius, pill_color)
        if handle is not None:
            vs.draw_svg_centered(cr, handle, cx, cy, radius * 1.6, tint=icon_color)

        # Debug border: trace the region box (= buffer edge) when debug_border is
        # set. Off by default (untrusted-input rule).
        if border_on:
            vl.draw_debug_border(cr, w, h, border_color)

        surface.flush()
        surface.finish()


# ----------------------------------------------------------------- main


def main() -> None:
    conn = vp.Connection.connect("ethernet", "0.1.0")
    cfg = conn.wait_for_configure()

    # Optional theming from [plugin.config], both RGBA 0..1 floats where the
    # fourth channel IS the opacity: pill_color = the chip ([0,0,0,0] = none),
    # icon_color = tints the glyph (default: as authored -- white). Same pair
    # on every status pill; see battery_svg.py, the template.
    plugin_cfg: dict[str, Any] = json.loads(
        os.environ.get("VEILAND_PLUGIN_CONFIG") or "{}"
    )
    pill_color = vs.parse_color(plugin_cfg, "pill_color", PILL_BG, tag="ethernet")
    icon_color = vs.parse_color(plugin_cfg, "icon_color", None, tag="ethernet")
    halign, valign = vl.anchor_from_config(plugin_cfg, tag="ethernet")
    border_on, border_color = vl.debug_border_from_config(plugin_cfg, tag="ethernet")

    icons = load_icons()

    # Best-effort D-Bus: if the SYSTEM bus is unreachable, run in a permanent
    # "down" state rather than exiting -- the pill still draws, it just always
    # shows ethernet-down. (source is None -> no extra_fd, no reads.)
    source: EthernetSource | None = None
    try:
        bus = vd.DBusConnection.connect("SYSTEM", tag="ethernet")
        source = EthernetSource(bus)
    except vd.DBusError as e:
        vd.log("ethernet", f"no system bus, showing down state: {e}")

    dev = vp.GbmDevice()
    # BufferChain, not a single LinearBuffer: this widget REDRAWS (the icon flips
    # on plug/unplug), and a CPU plugin redrawing one buffer in place races the
    # host's live sampling -> a flicker. The chain hands out the buffer the host
    # is not showing. (Same rationale as battery_svg.py / wifi.py.)
    chain = vp.BufferChain(dev, cfg.region_w, cfg.region_h)

    def current_icon() -> Any:
        up = source.read() if source is not None else False
        return icons.get(pick_icon(up))

    pacer = vp.FramePacer.on_demand()
    # NetworkManager's socket (when present) is an extra fd: a PropertiesChanged
    # wakes us on any wired change. A 30s tick is the slow fallback. The read is
    # one cheap round-trip and the icon rarely changes, so redraw-per-wake is
    # fine (no display-signature diff, unlike now-playing).
    extra = [source.fileno()] if source is not None else []
    for ev in pacer.events(conn, timeout=30.0, extra_fds=extra):
        if ev.kind is vp.Event.RENDER:
            draw_into(
                chain.acquire(),
                current_icon(),
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
            # NetworkManager emitted PropertiesChanged: drain the queued signals
            # (their arrival is the message) and redraw.
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
