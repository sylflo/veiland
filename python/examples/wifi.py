#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# A wifi status widget: a monochrome signal-strength glyph in a small pill,
# inset from the top-right corner alongside the battery chip. Clone of the
# battery_svg.py template -- an if/else buckets a reading to an icon file and
# veiland_svg blits it -- with the reading coming from NetworkManager over D-Bus
# (the SYSTEM bus) instead of /sys. READ-ONLY: it displays signal, it never
# connects/disconnects (no click protocol exists, and the roadmap keeps v1
# display-only).
#
# Data: NetworkManager on the SYSTEM bus (org.freedesktop.NetworkManager). We
# find the wifi device, read its State (activated?) and its active access point's
# Strength (0..100), and bucket that to wifi-0/25/50/75/100.svg. No wifi device
# or the radio off -> wifi-off.svg; a device that exists but is not connected ->
# the empty-bars wifi-0.svg. The bus socket goes on the pacer's extra_fds, so
# NetworkManager's PropertiesChanged wakes us on connect/disconnect/strength
# change; a slow TIMEOUT tick is the fallback. No bus at all -> wifi-off.svg,
# never a crash (a locker plugin degrades, per the no-panic-on-input rule).
#
# Needs the SVG stack (pygobject3 + librsvg + the Rsvg typelib) AND jeepney for
# D-Bus; the flake's dev shell wires both. A real plugin vendors veiland_plugin.
# py, veiland_dbus.py and veiland_svg.py beside itself; this example adds the
# repo's python/ dir to sys.path so it runs straight from the tree. The script
# must be chmod +x or the host spawn fails with "Permission denied (os error 13)".

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
    "wifi-0.svg",
    "wifi-25.svg",
    "wifi-50.svg",
    "wifi-75.svg",
    "wifi-100.svg",
    "wifi-off.svg",
]

# ------------------------------------------------------------- NetworkManager
#
# The stable NetworkManager D-Bus shape (unchanged for years):
#   /org/freedesktop/NetworkManager  .Devices           -> [device object paths]
#   <device>  .Device        .DeviceType   u32   2 == WIFI
#             .Device        .State        u32   100 == ACTIVATED
#             .Device.Wireless .ActiveAccessPoint  object path ("/" if none)
#   <ap>      .AccessPoint   .Strength     u8    0..100
# All reads go through the veiland_dbus companion, which returns None/{} on any
# D-Bus error -- a vanished device or a bus hiccup buckets into "off", not a
# traceback.

NM = "org.freedesktop.NetworkManager"
NM_PATH = "/org/freedesktop/NetworkManager"
NM_IFACE = NM
DEV_IFACE = "org.freedesktop.NetworkManager.Device"
WIRELESS_IFACE = "org.freedesktop.NetworkManager.Device.Wireless"
AP_IFACE = "org.freedesktop.NetworkManager.AccessPoint"

NM_DEVICE_TYPE_WIFI = 2
NM_STATE_ACTIVATED = 100


class WifiSource:
    # Read-only NetworkManager wifi reader over the shared D-Bus companion. Wakes
    # the plugin on any NetworkManager PropertiesChanged (device state + AP
    # strength both ride it), so a connect/disconnect or a strength change
    # repaints immediately; the plugin's TIMEOUT tick is the slow fallback.
    def __init__(self, bus):
        self.bus = bus
        # Match the whole NetworkManager subtree: device and AP objects live at
        # many paths under /org/freedesktop/NetworkManager, and either can change
        # (State on the device, Strength on the AP). One namespace rule covers
        # all of them; we do not parse the signal, its arrival means "re-read".
        self.bus.subscribe(
            interface="org.freedesktop.DBus.Properties",
            member="PropertiesChanged",
            path_namespace=NM_PATH,
        )

    def _wifi_device(self):
        # The first WIFI device's object path, or None. NetworkManager lists all
        # devices (wired, wifi, loopback, ...) under .Devices; we filter by type.
        paths = self.bus.get_prop(NM_PATH, NM_IFACE, "Devices", bus_name=NM)
        if not paths:
            return None
        for path in paths:
            dtype = self.bus.get_prop(path, DEV_IFACE, "DeviceType", bus_name=NM)
            if dtype == NM_DEVICE_TYPE_WIFI:
                return str(path)
        return None

    def read(self):
        # Return (has_device, connected, strength). has_device False -> radio off
        # or no wifi hardware (-> wifi-off). connected False with a device ->
        # disconnected (-> empty bars). strength is 0..100, only meaningful when
        # connected. Any D-Bus failure collapses to (False, False, 0) == "off".
        dev = self._wifi_device()
        if dev is None:
            return (False, False, 0)
        state = self.bus.get_prop(dev, DEV_IFACE, "State", bus_name=NM)
        connected = state == NM_STATE_ACTIVATED
        if not connected:
            return (True, False, 0)
        ap = self.bus.get_prop(dev, WIRELESS_IFACE, "ActiveAccessPoint", bus_name=NM)
        # "/" is NetworkManager's null object path (activated but no AP object
        # yet, e.g. a mid-handshake window); treat as connected-but-unknown.
        if not ap or ap == "/":
            return (True, True, 0)
        strength = self.bus.get_prop(ap, AP_IFACE, "Strength", bus_name=NM)
        try:
            return (True, True, int(strength))
        except (TypeError, ValueError):
            # Strength absent/garbage -> connected but unknown strength (0).
            return (True, True, 0)

    def fileno(self):
        return self.bus.fileno()

    def drain_signals(self):
        self.bus.drain_signals()

    def close(self):
        self.bus.close()


def pick_icon(has_device, connected, strength):
    # The whole "logic" of the widget: state -> filename. No device / radio off
    # is the distinct "off" glyph; a present-but-not-connected device shows the
    # empty-bars wifi-0; a connection buckets Strength at the 25/50/75/100
    # midpoints (matching battery_svg.py's bucketing).
    if not has_device:
        return "wifi-off.svg"
    if not connected:
        return "wifi-0.svg"
    if strength >= 88:
        return "wifi-100.svg"
    if strength >= 63:
        return "wifi-75.svg"
    if strength >= 38:
        return "wifi-50.svg"
    if strength >= 13:
        return "wifi-25.svg"
    return "wifi-0.svg"


def load_icons():
    # Parse every icon once at startup. A missing/corrupt file logs one line and
    # stores None; draw_into then draws just the pill for that state -- a bad
    # asset must never crash the locker or spew a traceback. (Same as
    # battery_svg.py; the wifi-*.svg set ships in python/examples/icons/.)
    icons = {}
    for name in ICON_FILES:
        try:
            icons[name] = vs.load_svg(os.path.join(ICON_DIR, name))
        except vs.SvgError as e:
            print(f"wifi: {name}: {e}", file=sys.stderr)
            icons[name] = None
    return icons


# ------------------------------------------------------------------- drawing

# Translucent dark pill, matching battery_svg.py so the status chips share one
# visual language when they sit in a row.
PILL_BG = (15 / 255, 18 / 255, 28 / 255, 175 / 255)


def draw_into(buf, handle):
    # Zero-copy: wrap buf.map()'s memoryview in a cairo surface and draw (pill +
    # SVG) straight into GPU-visible memory. cairo needs the MAP stride, not
    # buf.stride. Identical structure to battery_svg.py's draw_into.
    with buf.map() as (mem, map_stride):
        surface = cairo.ImageSurface.create_for_data(
            mem, cairo.FORMAT_ARGB32, buf.width, buf.height, map_stride
        )
        cr = cairo.Context(surface)
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.set_operator(cairo.OPERATOR_OVER)

        # The buffer IS our region, so placement is a two-line centering; WHERE
        # the region sits on screen is the host's job (config anchor).
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
    conn = vp.Connection.connect("wifi", "0.1.0")
    cfg = conn.wait_for_configure()
    icons = load_icons()

    # The D-Bus connection is best-effort: if the SYSTEM bus is unreachable, run
    # in a permanent "off" state rather than exiting -- the pill still draws, it
    # just always shows wifi-off. (source is None -> no extra_fd, no reads.)
    source = None
    try:
        bus = vd.DBusConnection.connect("SYSTEM", tag="wifi")
        source = WifiSource(bus)
    except vd.DBusError as e:
        vd.log("wifi", f"no system bus, showing off state: {e}")

    dev = vp.GbmDevice()
    # BufferChain, not a single LinearBuffer: this widget REDRAWS (the icon
    # changes with signal), and a CPU plugin redrawing one buffer in place races
    # the host's live sampling -> a flicker. The chain hands out the buffer the
    # host is not showing. (Same rationale as battery_svg.py.)
    chain = vp.BufferChain(dev, cfg.region_w, cfg.region_h)

    def current_icon():
        if source is None:
            return icons.get("wifi-off.svg")
        return icons.get(pick_icon(*source.read()))

    pacer = vp.FramePacer.on_demand()
    # NetworkManager's socket (when present) is an extra fd: a PropertiesChanged
    # wakes us on any wifi change. A 30s tick is the slow fallback. We do not
    # diff the drawn state here (unlike now-playing): the read is one cheap
    # round-trip and the icon rarely changes, so a redraw-per-wake is fine.
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
