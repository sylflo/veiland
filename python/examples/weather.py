#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# A current-weather widget: a condition glyph + temperature, from Open-Meteo
# (keyless HTTP), drawn either as a bottom-corner glass card (default) or a
# compact top-right status pill. It is READ-ONLY (a display, like the clock):
# the core cannot forward clicks into a plugin region, so there is no unit
# toggle -- you set the unit in config.
#
# Data: Open-Meteo's keyless forecast API over plain HTTPS (stdlib urllib, no
# key, no dependency). A location is either explicit latitude/longitude, or a
# city string ("Paris") geocoded once at startup via Open-Meteo's geocoding
# endpoint -- the doxxing-safe override, so a screenshot need not reveal your
# coordinates. With no location the widget draws a "set location" placeholder
# and never makes a request.
#
# The network is kept OFF the render path: the fetch is a synchronous urlopen
# with a short timeout, run only on the frame pacer's slow refresh tick (every
# refresh_minutes, floor 5). The locker is a SEPARATE process, so even a stalled
# fetch never freezes the lock/password UI -- only this widget's own repaints
# pause for at most one timeout. The last-good reading is cached to disk, so a
# re-lock or a monitor-hotplug respawn draws real data on frame one. Every
# external step (HTTP, JSON parse, cache read, bad config) degrades to a
# placeholder + one stderr line; nothing a network or a config file can send
# ever raises into the loop.
#
# (If you dislike the <=3s pause on the refresh tick, move the fetch to a
# background thread and wake the loop with a self-pipe fd on the pacer's
# extra_fds -- the same FD_READY plumbing wifi.py uses for its D-Bus socket.
# It is left out here to keep the example single-threaded.)
#
# The icons live in ./icons/ next to this file. A real plugin vendors
# veiland_plugin.py + veiland_svg.py + veiland_text.py + veiland_layout.py
# beside itself; this example adds the repo's python/ dir to sys.path so it runs
# straight from the tree.

from __future__ import annotations

import json
import math
import os
import sys
import time
import urllib.parse
import urllib.request
from dataclasses import dataclass
from typing import Any

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# The SDK + drawing companions (veiland_plugin, and cairo before veiland_svg /
# veiland_text / veiland_layout -- pycairo registers the pycairo<->GObject
# bridge librsvg/PangoCairo draw through) are imported with the draw +
# event-loop code below. The pure logic in this section is stdlib-only.

# ------------------------------------------------------------------- constants

ICON_DIR = os.path.join(os.path.dirname(os.path.abspath(__file__)), "icons")
ICON_FILES = [
    "weather-clear.svg",
    "weather-clear-night.svg",
    "weather-partly-cloudy.svg",
    "weather-partly-cloudy-night.svg",
    "weather-cloudy.svg",
    "weather-fog.svg",
    "weather-drizzle.svg",
    "weather-rain.svg",
    "weather-snow.svg",
    "weather-thunder.svg",
    "weather-unknown.svg",
]

# Open-Meteo endpoints (keyless, free). Forecast gives the current reading;
# geocoding resolves a city string to lat/long. We always ASK the API for
# Celsius and convert at draw time, so the disk cache is unit-independent.
FORECAST_URL = "https://api.open-meteo.com/v1/forecast"
GEOCODE_URL = "https://geocoding-api.open-meteo.com/v1/search"

# HTTP timeout for a single fetch. Short (3s, vs now_playing's 5s for covers)
# because this runs on the pacer tick and bounds the worst-case repaint stall.
HTTP_TIMEOUT = 3.0

# Refresh cadence floor. The keyless API is a shared free resource; never poll
# faster than every 5 minutes regardless of what the config asks for.
REFRESH_FLOOR_MINUTES = 5.0
DEFAULT_REFRESH_MINUTES = 15.0

# Disk cache: one fixed file (not a growing set like now_playing's covers), so
# no eviction is needed. Celsius on disk; see display_temp.
CACHE_DIR = os.path.join(
    os.environ.get("XDG_CACHE_HOME") or os.path.expanduser("~/.cache"),
    "veiland-weather",
)
CACHE_FILE = os.path.join(CACHE_DIR, "reading.json")

# Default chip background: the translucent dark navy every status pill shares
# (identical to battery_svg / wifi). Overridable via pill_color.
PILL_BG = (15 / 255, 18 / 255, 28 / 255, 175 / 255)

# Card palette, copied from now_playing.py's compact card (cheaper than a shared
# companion for ~4 tuples). RGBA / RGB in 0..1.
GLASS = (18 / 255, 20 / 255, 28 / 255, 0.72)
PRIMARY = (0.93, 0.94, 0.96)
SECONDARY = (0.66, 0.69, 0.76)
HAIRLINE = (1.0, 1.0, 1.0, 0.10)

# Per-box font fractions. The SDK's font_size default (0.030) is a fraction of
# the whole SURFACE, which renders invisibly tiny inside a short anchored box;
# these are fractions of the widget's own height, swapped in when the user did
# not set font_size (the markup.py precedent).
FONT_FRACTION_PILL = 0.42


def log(msg: str) -> None:
    # One tagged stderr line, matching the other widgets' "weather: ..." format.
    print(f"weather: {msg}", file=sys.stderr)


# --------------------------------------------------------------- reading state


@dataclass(frozen=True)
class Reading:
    temp_c: float  # ALWAYS Celsius; converted for display in display_temp.
    code: int  # WMO weather_code, 0..99.
    is_day: bool
    fetched_at: int  # unix seconds; stored for a future stale badge, unused now.


# A Reading-or-None models "no reading yet / every fetch has failed and no cache"
# -> the placeholder (an em dash + the neutral glyph), the same Track|None idiom
# now_playing uses for "nothing playing".


# ----------------------------------------------------------- WMO code -> glyph


def pick_icon(code: int, is_day: bool) -> str:
    # The whole "logic" of the widget: a WMO weather_code -> an icon filename.
    # Only the two SKY states split day/night (a clear night must not show a
    # sun); rain/snow/fog/thunder look the same after dark. An unknown or
    # out-of-range code falls through to the neutral glyph -- never crash.
    if code == 0:
        return "weather-clear.svg" if is_day else "weather-clear-night.svg"
    if code in (1, 2):
        return (
            "weather-partly-cloudy.svg" if is_day else "weather-partly-cloudy-night.svg"
        )
    if code == 3:
        return "weather-cloudy.svg"
    if code in (45, 48):
        return "weather-fog.svg"
    if code in (51, 53, 55, 56, 57):
        return "weather-drizzle.svg"
    if code in (61, 63, 65, 66, 67, 80, 81, 82):
        return "weather-rain.svg"
    if code in (71, 73, 75, 77, 85, 86):
        return "weather-snow.svg"
    if code in (95, 96, 99):
        return "weather-thunder.svg"
    return "weather-unknown.svg"


def condition_text(code: int) -> str:
    # Human-readable label for the card layout (condensed WMO phrasing). Unknown
    # codes read as an em dash rather than a wrong word.
    return {
        0: "Clear",
        1: "Mainly clear",
        2: "Partly cloudy",
        3: "Overcast",
        45: "Fog",
        48: "Rime fog",
        51: "Light drizzle",
        53: "Drizzle",
        55: "Heavy drizzle",
        56: "Freezing drizzle",
        57: "Freezing drizzle",
        61: "Light rain",
        63: "Rain",
        65: "Heavy rain",
        66: "Freezing rain",
        67: "Freezing rain",
        71: "Light snow",
        73: "Snow",
        75: "Heavy snow",
        77: "Snow grains",
        80: "Light showers",
        81: "Showers",
        82: "Heavy showers",
        85: "Snow showers",
        86: "Snow showers",
        95: "Thunderstorm",
        96: "Thunderstorm",
        99: "Thunderstorm",
    }.get(code, "—")  # em dash


# ------------------------------------------------------------- display / units


def display_temp(reading: Reading | None, units: str) -> str:
    # Reading (Celsius) -> the shown string. None -> the placeholder em dash.
    # Rounded to a whole degree; the degree sign but no unit letter (the glyph
    # already says "weather"), matching a compact lockscreen look.
    if reading is None:
        return "—°"  # em dash + degree
    temp = reading.temp_c
    if units == "fahrenheit":
        temp = temp * 9.0 / 5.0 + 32.0
    return f"{round(temp)}°"


# --------------------------------------------------------------- config parsing


def parse_units(cfg: dict[str, Any]) -> str:
    # "celsius" | "fahrenheit"; anything else falls back to celsius + one line.
    val = cfg.get("units", "celsius")
    if val in ("celsius", "fahrenheit"):
        return str(val)
    log(f"units: expected 'celsius' or 'fahrenheit', got {val!r}; using celsius")
    return "celsius"


def parse_layout(cfg: dict[str, Any]) -> str:
    # "card" (default) | "pill"; unknown -> card, mirroring now_playing's
    # unknown-layout-falls-back-to-default rule.
    val = cfg.get("layout", "card")
    if val in ("card", "pill"):
        return str(val)
    log(f"layout: expected 'card' or 'pill', got {val!r}; using card")
    return "card"


def parse_refresh_minutes(cfg: dict[str, Any]) -> float:
    # Minutes between fetches, floored at REFRESH_FLOOR_MINUTES to be polite to
    # the free keyless API. Bad value -> the default.
    raw = cfg.get("refresh_minutes", DEFAULT_REFRESH_MINUTES)
    try:
        minutes = float(raw)
    except (TypeError, ValueError):
        log(f"refresh_minutes: expected a number, got {raw!r}; using default")
        minutes = DEFAULT_REFRESH_MINUTES
    return max(REFRESH_FLOOR_MINUTES, minutes)


def parse_coords(cfg: dict[str, Any]) -> tuple[float, float] | None:
    # Explicit latitude+longitude, both required and both in range. Returns None
    # (not an error) when neither is set -- the caller then tries `location`.
    # A partial or out-of-range pair logs and returns None.
    if "latitude" not in cfg and "longitude" not in cfg:
        return None
    try:
        lat = float(cfg["latitude"])
        lon = float(cfg["longitude"])
    except (KeyError, TypeError, ValueError):
        log("latitude/longitude: need both as numbers; ignoring")
        return None
    if not (-90.0 <= lat <= 90.0 and -180.0 <= lon <= 180.0):
        log(f"latitude/longitude out of range ({lat}, {lon}); ignoring")
        return None
    return lat, lon


# --------------------------------------------------------------- HTTP + parse


def build_forecast_url(lat: float, lon: float) -> str:
    # Always request Celsius; display_temp converts. is_day drives the day/night
    # glyph split.
    query = urllib.parse.urlencode(
        {
            "latitude": lat,
            "longitude": lon,
            "current": "temperature_2m,weather_code,is_day",
            "temperature_unit": "celsius",
        }
    )
    return f"{FORECAST_URL}?{query}"


def build_geocode_url(city: str) -> str:
    return f"{GEOCODE_URL}?{urllib.parse.urlencode({'name': city, 'count': 1})}"


def parse_forecast(data: dict[str, Any]) -> Reading | None:
    # Open-Meteo forecast JSON -> a Reading, or None on any shape/type surprise.
    # Never raises: a malformed response must not break the widget.
    try:
        current = data["current"]
        return Reading(
            temp_c=float(current["temperature_2m"]),
            code=int(current["weather_code"]),
            is_day=bool(int(current["is_day"])),
            fetched_at=int(time.time()),
        )
    except (KeyError, TypeError, ValueError) as e:
        log(f"could not parse forecast response: {e}")
        return None


def parse_geocode(data: dict[str, Any]) -> tuple[float, float] | None:
    # Geocoding JSON -> the first result's (lat, lon), or None if no match.
    try:
        first = data["results"][0]
        return float(first["latitude"]), float(first["longitude"])
    except (KeyError, IndexError, TypeError, ValueError):
        return None


# ------------------------------------------------------------------- disk cache


def write_cache(reading: Reading) -> None:
    # Atomic write (.tmp + os.replace), so a suspend mid-write never leaves a
    # torn file that would later read as garbage. Best-effort: a cache-write
    # failure is logged, never fatal.
    try:
        os.makedirs(CACHE_DIR, exist_ok=True)
        tmp = CACHE_FILE + ".tmp"
        with open(tmp, "w") as f:
            json.dump(
                {
                    "temp_c": reading.temp_c,
                    "code": reading.code,
                    "is_day": reading.is_day,
                    "fetched_at": reading.fetched_at,
                },
                f,
            )
        os.replace(tmp, CACHE_FILE)
    except OSError as e:
        log(f"could not write cache: {e}")


def read_cache() -> Reading | None:
    # Load the last-good reading, or None if absent/corrupt. Coerce every field
    # so a hand-edited or partial file degrades to None rather than crashing.
    try:
        with open(CACHE_FILE) as f:
            data = json.load(f)
        return Reading(
            temp_c=float(data["temp_c"]),
            code=int(data["code"]),
            is_day=bool(data["is_day"]),
            fetched_at=int(data["fetched_at"]),
        )
    except (OSError, ValueError, KeyError, TypeError):
        return None


# The SDK + drawing companions are imported here (not in the pure-logic section
# above) because only the draw + event-loop code below uses them. cairo comes
# before the veiland companions so pycairo registers the pycairo<->GObject
# bridge librsvg / PangoCairo draw through; Pango is imported directly only for
# the SEMIBOLD weight constant the temperature uses.
import cairo  # noqa: E402
import gi  # noqa: E402

gi.require_version("Pango", "1.0")  # noqa: E402
from gi.repository import Pango  # noqa: E402  (after gi.require_version)

import veiland_layout as vl  # noqa: E402
import veiland_plugin as vp  # noqa: E402
import veiland_svg as vs  # noqa: E402
import veiland_text as vt  # noqa: E402

# --------------------------------------------------------------------- icons


def load_icons() -> dict[str, Any]:
    # Parse every condition glyph once at startup (draw_svg is called per frame).
    # Values are Rsvg.Handle-or-None; gi ships no types so a handle is Any. A
    # missing or corrupt file logs one line and stores None -- draw_into then
    # draws just the chip/card for that state, never a traceback. (Identical to
    # battery_svg.load_icons.)
    icons: dict[str, Any] = {}
    for name in ICON_FILES:
        try:
            icons[name] = vs.load_svg(os.path.join(ICON_DIR, name))
        except vs.SvgError as e:
            log(f"{name}: {e}")
            icons[name] = None
    return icons


# --------------------------------------------------------------------- network
#
# One synchronous HTTP GET returning parsed JSON, shared by the forecast fetch
# and the geocoding lookup. urllib (stdlib) with a short timeout and a broad
# except so a DNS failure / timeout / non-JSON body degrades to None -- the
# caller keeps the last-good reading and never crashes. This is the ONLY place
# that blocks on the network, and it is only ever called off the render path
# (startup + the pacer's refresh tick).


def _http_get_json(url: str) -> dict[str, Any] | None:
    try:
        req = urllib.request.Request(url, headers={"User-Agent": "veiland"})
        with urllib.request.urlopen(req, timeout=HTTP_TIMEOUT) as r:
            data = json.load(r)
        if isinstance(data, dict):
            return data
        log(f"unexpected JSON shape from {url}")
        return None
    except Exception as e:
        log(f"fetch failed ({url}): {e}")
        return None


def fetch_reading(lat: float, lon: float) -> Reading | None:
    # Current weather at (lat, lon), or None on any failure. Composes the tested
    # pure URL builder + parser with the one HTTP helper.
    data = _http_get_json(build_forecast_url(lat, lon))
    if data is None:
        return None
    return parse_forecast(data)


def resolve_location(city: str) -> tuple[float, float] | None:
    # Geocode a city string to (lat, lon) once at startup. None (and one log
    # line) if the network failed or the name matched nothing.
    data = _http_get_json(build_geocode_url(city))
    if data is None:
        return None
    coords = parse_geocode(data)
    if coords is None:
        log(f"could not resolve location {city!r}")
    return coords


# --------------------------------------------------------------------- drawing


def rounded_rect(
    cr: cairo.Context[cairo.ImageSurface],
    x: float,
    y: float,
    w: float,
    h: float,
    r: float,
) -> None:
    # cairo has no rounded-rectangle primitive; trace one from four arcs.
    # (Same helper as now_playing.py / battery_cairo.py.)
    r = min(r, w / 2, h / 2)
    cr.new_sub_path()
    cr.arc(x + w - r, y + r, r, -math.pi / 2, 0)
    cr.arc(x + w - r, y + h - r, r, 0, math.pi / 2)
    cr.arc(x + r, y + h - r, r, math.pi / 2, math.pi)
    cr.arc(x + r, y + r, r, math.pi, 3 * math.pi / 2)
    cr.close_path()


def draw_card(
    cr: cairo.Context[cairo.ImageSurface],
    w: float,
    h: float,
    handle: Any,
    reading: Reading | None,
    units: str,
    font: vt.FontSpec,
    icon_color: vs.RGBA | None,
    show_location: bool,
    place: str | None,
) -> None:
    # The bottom-corner glass card: condition glyph on the left, big temperature
    # + condition text stacked on the right, an optional location line. Modeled
    # on now_playing.draw_compact -- same glass + hairline + non-overlapping
    # text zones -- but a simpler two/three-line column (no progress bar). Sizes
    # derive from the card height h so the layout scales with the region.
    pad = h * 0.14

    # -- glass card: translucent fill + a top hairline highlight --
    radius = h * 0.16
    rounded_rect(cr, 0, 0, w, h, radius)
    cr.set_source_rgba(*GLASS)
    cr.fill()
    rounded_rect(cr, 0, 0, w, h, radius)
    cr.set_source_rgba(*HAIRLINE)
    cr.set_line_width(1.0)
    cr.stroke()

    # -- condition glyph (square) on the left --
    art = h - 2 * pad
    if handle is not None:
        vs.draw_svg_centered(cr, handle, pad + art / 2, h / 2, art, tint=icon_color)

    # -- text column: explicit, NON-OVERLAPPING vertical zones (draw_compact's
    # pattern). With a location line the three zones sit higher/tighter; without
    # it, temperature + condition center as a pair.
    tx = pad + art + pad
    text_w = w - tx - pad
    has_place = show_location and place is not None
    if has_place:
        temp_cy, cond_cy, place_cy = h * 0.30, h * 0.56, h * 0.80
    else:
        temp_cy, cond_cy, place_cy = h * 0.36, h * 0.66, 0.0
    # temperature (big, semibold)
    vt.draw_ellipsized_centered(
        cr,
        display_temp(reading, units),
        tx,
        temp_cy,
        text_w,
        h * 0.34,
        PRIMARY,
        weight=Pango.Weight.SEMIBOLD,
        spec=font,
    )
    # condition text (or "Loading..." / "Set location" when there's no reading)
    if reading is not None:
        cond = condition_text(reading.code)
    elif place is None:
        cond = "Set location"
    else:
        cond = "Loading…"
    vt.draw_ellipsized_centered(
        cr, cond, tx, cond_cy, text_w, h * 0.16, SECONDARY, spec=font
    )
    # optional location line (the `place is not None` form, not `has_place`, so
    # mypy narrows place from str|None to str inside the branch).
    if show_location and place is not None:
        vt.draw_ellipsized_centered(
            cr, place, tx, place_cy, text_w, h * 0.13, SECONDARY, spec=font
        )


def draw_pill_layout(
    cr: cairo.Context[cairo.ImageSurface],
    w: float,
    h: float,
    handle: Any,
    reading: Reading | None,
    units: str,
    font: vt.FontSpec,
    pill_color: vs.RGBA,
    icon_color: vs.RGBA | None,
    halign: str,
    valign: str,
) -> None:
    # The compact status-cluster capsule: a rounded pill (glyph left + temp
    # right), sized to the region. Unlike the square battery/wifi chip this is a
    # capsule because it carries text; the content-anchor convention parks the
    # capsule within the region (default center) so it drops into the cluster.
    temp = display_temp(reading, units)

    # The capsule fills the region (the TOML sizes the region to fit glyph +
    # "18 deg"); anchor_offset is a true no-op at the default center, and lets a
    # user shrink+park the capsule with content_halign/valign if they want.
    cap_w, cap_h = w, h
    x, y = vl.anchor_offset(halign, valign, w, h, cap_w, cap_h)
    rounded_rect(cr, x, y, cap_w, cap_h, cap_h / 2)
    cr.set_source_rgba(*pill_color)
    cr.fill()

    # glyph on the left, inset by ~half the padding; temperature centered in the
    # remaining space to its right.
    gpad = cap_h * 0.14
    gsize = cap_h - 2 * gpad
    gcx = x + gpad + gsize / 2
    if handle is not None:
        vs.draw_svg_centered(cr, handle, gcx, y + cap_h / 2, gsize, tint=icon_color)
    text_x = gcx + gsize / 2
    text_w = x + cap_w - gpad - text_x
    vt.draw_ellipsized_centered(
        cr,
        temp,
        text_x,
        y + cap_h / 2,
        text_w,
        cap_h * FONT_FRACTION_PILL,
        PRIMARY,
        spec=font,
    )


def draw_into(
    buf: vp.LinearBuffer,
    layout_name: str,
    handle: Any,
    reading: Reading | None,
    units: str,
    font: vt.FontSpec,
    pill_color: vs.RGBA,
    icon_color: vs.RGBA | None,
    halign: str,
    valign: str,
    show_location: bool,
    place: str | None,
    border_on: bool,
    border_color: vl.RGBA,
) -> None:
    # Zero-copy: wrap buf.map()'s memoryview in a cairo surface and draw straight
    # into GPU-visible memory. cairo needs the MAP stride, not buf.stride.
    # (Structurally identical to now_playing.draw_into.)
    with buf.map() as (mem, map_stride):
        surface = cairo.ImageSurface.create_for_data(
            mem, cairo.FORMAT_ARGB32, buf.width, buf.height, map_stride
        )
        cr = cairo.Context(surface)
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.set_operator(cairo.OPERATOR_OVER)
        w, h = float(buf.width), float(buf.height)
        # Two layouts, picked from config. Unknown names already fell back to
        # "card" in parse_layout.
        if layout_name == "pill":
            draw_pill_layout(
                cr,
                w,
                h,
                handle,
                reading,
                units,
                font,
                pill_color,
                icon_color,
                halign,
                valign,
            )
        else:
            draw_card(
                cr,
                w,
                h,
                handle,
                reading,
                units,
                font,
                icon_color,
                show_location,
                place,
            )
        if border_on:
            vl.draw_debug_border(cr, w, h, border_color)
        surface.flush()
        surface.finish()


# --------------------------------------------------------------------- main


def _visible(reading: Reading | None) -> tuple[object, ...]:
    # Everything that changes what the widget LOOKS like -- so a refresh whose
    # only difference is the fetch timestamp does not trigger a repaint. (The
    # displayed temperature is whole-degree, but a sub-degree drift can still
    # flip the rounded value, so compare the raw temp_c; a redraw on a genuine
    # 0.1C change is harmless and rare.)
    if reading is None:
        return ("none",)
    return (reading.temp_c, reading.code, reading.is_day)


def main() -> None:
    conn = vp.Connection.connect("weather", "0.1.0")
    cfg = conn.wait_for_configure()  # handshake only; no network here

    plugin_cfg: dict[str, Any] = json.loads(
        os.environ.get("VEILAND_PLUGIN_CONFIG") or "{}"
    )
    layout_name = parse_layout(plugin_cfg)
    units = parse_units(plugin_cfg)
    refresh_seconds = parse_refresh_minutes(plugin_cfg) * 60.0
    network_on = bool(plugin_cfg.get("network", True))
    show_location = bool(plugin_cfg.get("show_location", True))
    pill_color = vs.parse_color(plugin_cfg, "pill_color", PILL_BG, tag="weather")
    icon_color = vs.parse_color(plugin_cfg, "icon_color", None, tag="weather")
    font = vt.font_from_config(plugin_cfg, tag="weather")
    halign, valign = vl.anchor_from_config(plugin_cfg, tag="weather")
    border_on, border_color = vl.debug_border_from_config(plugin_cfg, tag="weather")

    # -- resolve the location once. Explicit lat/long wins; else geocode a city
    # string. `place` is the label the card can show (the configured city, or a
    # "lat, lon" when only coordinates were given). No coords -> no fetching. --
    coords = parse_coords(plugin_cfg)
    place: str | None = None
    city = plugin_cfg.get("location")
    if coords is not None:
        if city is not None:
            log("both latitude/longitude and location set; using coordinates")
        place = f"{coords[0]:.2f}, {coords[1]:.2f}"
    elif isinstance(city, str) and city.strip():
        place = city.strip()
        if network_on:
            resolved = resolve_location(place)
            if resolved is not None:
                coords = resolved
        else:
            log("network off; cannot geocode a city -> showing cache/placeholder")
    else:
        log("no location set; set `location` or latitude/longitude to fetch weather")

    icons = load_icons()

    # -- seed from the disk cache so the first frame shows the last-good reading
    # (or the placeholder). Then, if we have coordinates and the network is on,
    # one bootstrap fetch BEFORE the loop -- startup is then identical to the
    # steady state (a fetch feeding `current`), and it never blocks
    # wait_for_configure. A brand-new widget has nothing to stall. --
    current: Reading | None = read_cache()
    if coords is not None and network_on:
        fetched = fetch_reading(*coords)
        if fetched is not None:
            current = fetched
            write_cache(fetched)

    def current_handle() -> Any:
        if current is None:
            return icons.get("weather-unknown.svg")
        return icons.get(pick_icon(current.code, current.is_day))

    dev = vp.GbmDevice()
    # BufferChain, not a single LinearBuffer: this widget REDRAWS (the reading
    # changes on the refresh tick), and a CPU plugin redrawing one buffer in
    # place races the host's live sampling -> flicker. The chain hands out the
    # buffer the host is not showing. (Same rationale as battery_svg / wifi.)
    chain = vp.BufferChain(dev, cfg.region_w, cfg.region_h)

    def render() -> None:
        draw_into(
            chain.acquire(),
            layout_name,
            current_handle(),
            current,
            units,
            font,
            pill_color,
            icon_color,
            halign,
            valign,
            show_location,
            place,
            border_on,
            border_color,
        )
        chain.send(conn)
        pacer.submitted()

    pacer = vp.FramePacer.on_demand()
    pacer.mark_dirty()  # paint the seeded/bootstrap reading on frame one

    # The pacer wakes on the refresh interval; we only actually re-FETCH when the
    # monotonic clock passes next_fetch_at (the bootstrap already fetched, so the
    # first tick's fetch is a refresh). No coords / network off -> never fetch;
    # the widget just draws the cache/placeholder.
    next_fetch_at = time.monotonic() + refresh_seconds
    for ev in pacer.events(conn, timeout=refresh_seconds):
        if ev.kind is vp.Event.RENDER:
            render()
        elif ev.kind is vp.Event.RECONFIGURE and ev.configure is not None:
            # (`is not None` narrows for mypy; the SDK always sets .configure.)
            cfg = ev.configure
            chain = chain.resize_or_keep(dev, cfg)
            pacer.mark_dirty()  # new size -> redraw
        elif ev.kind is vp.Event.TIMEOUT:
            now = time.monotonic()
            if coords is not None and network_on and now >= next_fetch_at:
                next_fetch_at = now + refresh_seconds
                fetched = fetch_reading(*coords)
                # Only redraw when what's SHOWN changed (not just the timestamp),
                # so a stable sky sits idle instead of repainting every refresh.
                if fetched is not None and _visible(fetched) != _visible(current):
                    current = fetched
                    write_cache(fetched)
                    pacer.mark_dirty()
        elif ev.kind is vp.Event.SHUTDOWN:
            break

    chain.close()
    dev.close()
    conn.close()


if __name__ == "__main__":
    main()
