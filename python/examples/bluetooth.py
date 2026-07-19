#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# A bluetooth status widget: a monochrome bluetooth glyph in a small pill, inset
# from the top-right corner alongside the battery/wifi/ethernet chips. Sibling of
# wifi.py / ethernet.py -- same SYSTEM-bus + companion + pill structure -- but the
# source is bluez (org.bluez), not NetworkManager, and the state is three-way:
# adapter off, adapter on (nothing connected), or at least one device connected.
# Three glyphs: bluetooth-off.svg, bluetooth-on.svg, bluetooth-connected.svg.
# READ-ONLY: it shows state, it never pairs or connects.
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

import os
import sys

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# cairo before veiland_svg: importing pycairo registers the pycairo<->GObject
# foreign bridge in-process, which is what lets librsvg render onto a cairo
# context inside veiland_svg.draw_svg. (E402: after the sys.path shim.)
import cairo  # noqa: E402

import veiland_dbus as vd  # noqa: E402
import veiland_plugin as vp  # noqa: E402
import veiland_svg as vs  # noqa: E402

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
    def __init__(self, bus):
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

    def read(self):
        # Return (powered, connected_count). powered False -> adapter off or
        # absent (-> bluetooth-off). powered True with count 0 -> on but idle
        # (-> bluetooth-on); count > 0 -> at least one device (-> connected).
        # Any bluez/bus failure collapses to (False, 0) == "off".
        objects = self.bus.get_managed_objects(bus_name=BLUEZ)
        powered = False
        connected = 0
        for ifaces in objects.values():
            adapter = ifaces.get(ADAPTER_IFACE)
            if adapter is not None and adapter.get("Powered"):
                powered = True
            device = ifaces.get(DEVICE_IFACE)
            if device is not None and device.get("Connected"):
                connected += 1
        return (powered, connected)

    def fileno(self):
        return self.bus.fileno()

    def drain_signals(self):
        self.bus.drain_signals()

    def close(self):
        self.bus.close()


def pick_icon(powered, connected):
    # The whole "logic" of the widget: state -> filename. Adapter off wins; then
    # any connected device shows the "connected" glyph; else the plain "on"
    # glyph. Three states, no numeric bucketing (unlike wifi/battery).
    if not powered:
        return "bluetooth-off.svg"
    if connected > 0:
        return "bluetooth-connected.svg"
    return "bluetooth-on.svg"


def load_icons():
    # Parse every icon once at startup. A missing/corrupt file logs one line and
    # stores None; draw_into then draws just the pill for that state -- a bad
    # asset must never crash the locker or spew a traceback. (Same as
    # battery_svg.py / wifi.py; bluetooth-*.svg ship in python/examples/icons/.)
    icons = {}
    for name in ICON_FILES:
        try:
            icons[name] = vs.load_svg(os.path.join(ICON_DIR, name))
        except vs.SvgError as e:
            print(f"bluetooth: {name}: {e}", file=sys.stderr)
            icons[name] = None
    return icons


# ------------------------------------------------------------------- drawing

# Translucent dark pill, matching battery_svg.py / wifi.py / ethernet.py so the
# status chips share one visual language when they sit in a row.
PILL_BG = (15 / 255, 18 / 255, 28 / 255, 175 / 255)


def draw_into(buf, handle):
    # Zero-copy: wrap buf.map()'s memoryview in a cairo surface and draw (pill +
    # SVG) straight into GPU-visible memory. cairo needs the MAP stride, not
    # buf.stride. Identical structure to battery_svg.py / wifi.py / ethernet.py.
    with buf.map() as (mem, map_stride):
        surface = cairo.ImageSurface.create_for_data(
            mem, cairo.FORMAT_ARGB32, buf.width, buf.height, map_stride
        )
        cr = cairo.Context(surface)
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.set_operator(cairo.OPERATOR_OVER)

        cx = buf.width / 2
        cy = buf.height / 2
        radius = min(buf.width, buf.height) / 2 - 4

        vs.draw_pill(cr, cx, cy, radius, PILL_BG)
        if handle is not None:
            vs.draw_svg_centered(cr, handle, cx, cy, radius * 1.6)

        surface.flush()
        surface.finish()


# ----------------------------------------------------------------- main


def main():
    conn = vp.Connection.connect("bluetooth", "0.1.0")
    cfg = conn.wait_for_configure()
    icons = load_icons()

    # Best-effort D-Bus: if the SYSTEM bus is unreachable, run in a permanent
    # "off" state rather than exiting -- the pill still draws, it just always
    # shows bluetooth-off. (source is None -> no extra_fd, no reads.)
    source = None
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

    def current_icon():
        if source is None:
            return icons.get("bluetooth-off.svg")
        return icons.get(pick_icon(*source.read()))

    pacer = vp.FramePacer.on_demand()
    # bluez's socket (when present) is an extra fd: a PropertiesChanged wakes us
    # on any power/connect change. A 30s tick is the slow fallback. The read is
    # one cheap round-trip and the icon rarely changes, so redraw-per-wake is
    # fine (no display-signature diff, unlike now-playing).
    extra = [source.fileno()] if source is not None else []
    for ev in pacer.events(conn, timeout=30.0, extra_fds=extra):
        if ev.kind is vp.Event.RENDER:
            draw_into(chain.acquire(), current_icon())
            chain.send(conn)
            pacer.submitted()
        elif ev.kind is vp.Event.RECONFIGURE:
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
