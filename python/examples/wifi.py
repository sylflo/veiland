#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# A wifi status widget: a monochrome signal-strength glyph in a small pill,
# inset from the top-right corner alongside the battery chip, with an OPTIONAL
# SSID label under the glyph (show_label, off by default). Clone of the
# battery_svg.py template -- an if/else buckets a reading to an icon file and
# veiland_svg blits it -- with the reading coming from NetworkManager over D-Bus
# (the SYSTEM bus) instead of /sys. Unlike hyprlock's fixed nerd-font wifi glyph,
# the icon is state-driven (bars rise/fall with signal, off when the radio is),
# and the SSID -- when shown -- is read from NetworkManager event-driven, not
# polled from a shell. READ-ONLY: it displays signal, it never connects/
# disconnects (no click protocol exists, and the roadmap keeps v1 display-only).
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
    def __init__(self, bus: vd.DBusConnection) -> None:
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

    def _wifi_device(self) -> str | None:
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

    def read(self) -> tuple[bool, bool, int, str]:
        # Return (has_device, connected, strength, ssid). has_device False ->
        # radio off or no wifi hardware (-> wifi-off). connected False with a
        # device -> disconnected (-> empty bars). strength is 0..100, only
        # meaningful when connected. ssid is the network name (""; only non-empty
        # when connected to a named AP) -- read from the SAME AccessPoint object
        # Strength lives on, so the optional label and the glyph never disagree.
        # Any D-Bus failure collapses to (False, False, 0, "") == "off".
        dev = self._wifi_device()
        if dev is None:
            return (False, False, 0, "")
        state = self.bus.get_prop(dev, DEV_IFACE, "State", bus_name=NM)
        connected = state == NM_STATE_ACTIVATED
        if not connected:
            return (True, False, 0, "")
        ap = self.bus.get_prop(dev, WIRELESS_IFACE, "ActiveAccessPoint", bus_name=NM)
        # "/" is NetworkManager's null object path (activated but no AP object
        # yet, e.g. a mid-handshake window); treat as connected-but-unknown.
        if not ap or ap == "/":
            return (True, True, 0, "")
        ssid = _decode_ssid(self.bus.get_prop(ap, AP_IFACE, "Ssid", bus_name=NM))
        strength = self.bus.get_prop(ap, AP_IFACE, "Strength", bus_name=NM)
        if strength is None:
            return (True, True, 0, ssid)
        try:
            return (True, True, int(strength), ssid)
        except (TypeError, ValueError):
            # Strength absent/garbage -> connected but unknown strength (0).
            return (True, True, 0, ssid)

    def fileno(self) -> int:
        return self.bus.fileno()

    def drain_signals(self) -> None:
        self.bus.drain_signals()

    def close(self) -> None:
        self.bus.close()


LABEL_POSITIONS = ("top", "bottom", "left", "right")


def _label_pos_from(cfg: dict[str, Any]) -> str:
    # Where the SSID label sits relative to the glyph: top | bottom | left |
    # right, all WITHIN this one region (the label is attached to the icon, not
    # placed elsewhere on screen -- that would be a second region). Default
    # "bottom" (the stacked chip look). An unknown value logs one line and falls
    # back, never crashes (the untrusted-input rule, as parse_color does).
    raw = cfg.get("label_pos", "bottom")
    if isinstance(raw, str) and raw in LABEL_POSITIONS:
        return raw
    print(
        f"wifi: label_pos: expected one of {LABEL_POSITIONS}, got {raw!r}; "
        "using 'bottom'",
        file=sys.stderr,
    )
    return "bottom"


def _decode_ssid(raw: Any) -> str:
    # NetworkManager types AccessPoint.Ssid as `ay` (a byte array), NOT a string
    # -- an SSID is arbitrary bytes and need not be valid UTF-8. jeepney hands it
    # over as bytes/bytearray (or a list of ints on some paths); decode
    # best-effort, replacing undecodable bytes and trimming a stray trailing NUL,
    # so a weird SSID mis-renders at worst and never raises (the untrusted-input
    # rule -- an access point's advertised name is external data like any other
    # D-Bus field). Anything unexpected collapses to "" == no label.
    if raw is None:
        return ""
    try:
        if isinstance(raw, (bytes, bytearray, list, tuple)):
            return bytes(raw).decode("utf-8", "replace").rstrip("\x00")
    except (TypeError, ValueError):
        return ""
    return ""


def pick_icon(has_device: bool, connected: bool, strength: int) -> str:
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


def load_icons() -> dict[str, Any]:
    # Parse every icon once at startup. A missing/corrupt file logs one line and
    # stores None; draw_into then draws just the pill for that state -- a bad
    # asset must never crash the locker or spew a traceback. (Same as
    # battery_svg.py; the wifi-*.svg set ships in python/examples/icons/.)
    # Values are Rsvg.Handle-or-None; gi ships no types, so the handle is Any.
    icons: dict[str, Any] = {}
    for name in ICON_FILES:
        try:
            icons[name] = vs.load_svg(os.path.join(ICON_DIR, name))
        except vs.SvgError as e:
            print(f"wifi: {name}: {e}", file=sys.stderr)
            icons[name] = None
    return icons


# ------------------------------------------------------------------- drawing

# Default pill background: the translucent dark navy all the status chips share
# (battery_svg.py et al), so they read as one row. Overridable per config via
# pill_color (see main).
PILL_BG = (15 / 255, 18 / 255, 28 / 255, 175 / 255)

# Default color of the optional SSID label under the glyph -- near-white, legible
# on the dark pill, matching the icon. Overridable via label_color. RGBA (alpha
# is honoured by clamping, but draw_ellipsized_centered takes RGB, so only the
# first three channels reach the text; alpha 0 is NOT how you hide the label --
# show_label = false is).
LABEL_FG = (0.95, 0.95, 0.95, 1.0)


def draw_into(
    buf: vp.LinearBuffer,
    handle: Any,
    ssid: str,
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
    # stride, not buf.stride. Structure follows battery_svg.py's draw_into.
    with buf.map() as (mem, map_stride):
        surface = cairo.ImageSurface.create_for_data(
            mem, cairo.FORMAT_ARGB32, buf.width, buf.height, map_stride
        )
        cr = cairo.Context(surface)
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.set_operator(cairo.OPERATOR_OVER)

        # The buffer IS our region. Glyph + label form ONE unit placed by
        # label_pos: text below (bottom), above (top), or beside (left/right) the
        # glyph, hugging it across `gap`. The label stays ATTACHED to the icon
        # (both inside this one region) -- a free-floating "icon here, text
        # elsewhere" would be two regions, which one plugin cannot do. An empty
        # ssid draws the glyph alone, centered, byte-identical to the icon-only
        # widget: main() passes "" only when show_label is off; a disconnected
        # state passes the label_disconnected placeholder ("N/A" by default), so
        # the chip keeps its shape and shows offline text, not a blank column.
        w, h = float(buf.width), float(buf.height)
        px = font.size * h

        # Draw the glyph and (optional) label as ONE tightly-stacked unit,
        # measured then placed by hand -- NOT via sub-boxes. Note vt's
        # "draw_ellipsized_centered" centers only VERTICALLY (on cy) and draws the
        # text's LEFT edge at x; it does NOT horizontally center. So we measure the
        # label width ourselves and set its x to sit centered on / beside the
        # glyph. gap is the glyph<->text breathing room.
        gap = px * 0.35

        # Measure the label once (0 width when there is none) so both the glyph
        # sizing and the label placement can use its real extent.
        label_w = label_h = 0.0
        if ssid:
            probe = vt.line_layout(cr, ssid, w, px, font.weight, font)
            _, logical = probe.get_pixel_extents()
            label_w, label_h = float(logical.width), float(logical.height)

        # Size the glyph and lay out the unit's bounding box (uw x uh), then anchor
        # that box in the region via content_halign/valign (default center = a
        # centered chip). Everything below is expressed relative to the box's
        # top-left (ox, oy), so the anchor is applied in exactly one place.
        #
        # pad is inset from the region edge, so the centered unit keeps EQUAL
        # breathing room all round instead of the label pressing the bottom edge
        # (fraction of the shorter side, so it scales with the card). The glyph is
        # also capped at glyph_max of the shorter side so a tall card does not grow
        # a huge disc that shoves the label to the rim.
        pad = min(w, h) * 0.14
        glyph_max = min(w, h) * 0.62
        if not ssid:
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

        # Glyph center (cx, cy) and label top-left (tlx, tly), placed inside the
        # unit box so glyph and label always share an axis and hug across `gap`.
        if not ssid:
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

        # The SSID label at its measured top-left. draw_ellipsized (top-left form)
        # takes RGB (text edges anti-alias via the glyph mask, not a color alpha);
        # max_w = label_w + 1 never truncates (we sized the box to the text), and
        # tly is the text's TOP because this variant draws downward from y.
        if ssid:
            vt.draw_ellipsized(
                cr,
                ssid,
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
    conn = vp.Connection.connect("wifi", "0.1.0")
    cfg = conn.wait_for_configure()

    # Optional theming from [plugin.config], both RGBA 0..1 floats where the
    # fourth channel IS the opacity: pill_color = the chip ([0,0,0,0] = none),
    # icon_color = tints the glyph (default: as authored -- white). Same pair
    # on every status pill; see battery_svg.py, the template.
    plugin_cfg: dict[str, Any] = json.loads(
        os.environ.get("VEILAND_PLUGIN_CONFIG") or "{}"
    )
    pill_color = vs.parse_color(plugin_cfg, "pill_color", PILL_BG, tag="wifi")
    icon_color = vs.parse_color(plugin_cfg, "icon_color", None, tag="wifi")
    halign, valign = vl.anchor_from_config(plugin_cfg, tag="wifi")
    border_on, border_color = vl.debug_border_from_config(plugin_cfg, tag="wifi")

    # Optional SSID label beside the glyph. OFF by default -> the widget is
    # byte-identical to the icon-only pill unless the user opts in. When on, the
    # font uses the uniform font_family/font_size keys (a fraction of the region
    # height, like every text widget), label_color themes the text, and label_pos
    # (top|bottom|left|right) places it relative to the glyph inside this region.
    show_label = bool(plugin_cfg.get("show_label", False))
    font = vt.font_from_config(plugin_cfg, tag="wifi")
    label_color = vs.parse_color(plugin_cfg, "label_color", LABEL_FG, tag="wifi")
    label_pos = _label_pos_from(plugin_cfg)
    # What the label shows when there is no SSID (disconnected / radio off / no
    # device). Defaults to "N/A" rather than "" because with label_pos left/right
    # the chip is a FIXED width -- a blank name column reads as a bug, not as
    # "offline", so the empty state must show SOMETHING. Set label_disconnected =
    # "" to opt into the clean glyph-only vanish (fine for top/bottom, where the
    # strip just disappears). A non-string logs one line and falls back to "N/A".
    raw_disc = plugin_cfg.get("label_disconnected", "N/A")
    label_disconnected = raw_disc if isinstance(raw_disc, str) else "N/A"
    if not isinstance(raw_disc, str):
        print(
            f"wifi: label_disconnected: expected a string, got {raw_disc!r}; "
            "using 'N/A'",
            file=sys.stderr,
        )

    icons = load_icons()

    # The D-Bus connection is best-effort: if the SYSTEM bus is unreachable, run
    # in a permanent "off" state rather than exiting -- the pill still draws, it
    # just always shows wifi-off. (source is None -> no extra_fd, no reads.)
    source: WifiSource | None = None
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

    def current_state() -> tuple[Any, str]:
        # ONE D-Bus read feeds both the glyph and the label, so they can never
        # disagree. First resolve the icon + raw SSID, then decide the label:
        #   show_label off  -> "" (no label at all; icon-only geometry)
        #   connected       -> the SSID
        #   disconnected    -> the label_disconnected placeholder ("N/A" default)
        # so a fixed-width chip never shows a blank name column that reads as a bug.
        if source is None:
            icon, ssid = icons.get("wifi-off.svg"), ""
        else:
            has_device, connected, strength, ssid = source.read()
            icon = icons.get(pick_icon(has_device, connected, strength))
        if not show_label:
            return icon, ""
        return icon, (ssid if ssid else label_disconnected)

    pacer = vp.FramePacer.on_demand()
    # NetworkManager's socket (when present) is an extra fd: a PropertiesChanged
    # wakes us on any wifi change. A 30s tick is the slow fallback. We do not
    # diff the drawn state here (unlike now-playing): the read is one cheap
    # round-trip and the icon rarely changes, so a redraw-per-wake is fine.
    extra = [source.fileno()] if source is not None else []
    for ev in pacer.events(conn, timeout=30.0, extra_fds=extra):
        if ev.kind is vp.Event.RENDER:
            icon, ssid = current_state()
            draw_into(
                chain.acquire(),
                icon,
                ssid,
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
