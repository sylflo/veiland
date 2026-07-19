#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The now-playing widget: a glanceable "currently playing" card for the locked
# screen -- album art, title/artist, a progress bar -- composited over the
# wallpaper. It is READ-ONLY (a display, like the clock): the core cannot yet
# forward clicks into a plugin region, so there are no transport controls.
#
# Data: MPRIS over D-Bus (jeepney, a pure-Python client). We find the active
# org.mpris.MediaPlayer2.* player, read Metadata/PlaybackStatus/Position, and
# subscribe to PropertiesChanged. The D-Bus socket goes on the pacer's
# extra_fds, so a track change wakes us immediately; a 1s tick advances the
# progress bar between changes. No player -> the quiet "Nothing playing" state.
#
# Album art: mpris:artUrl. file:// covers are decoded locally (PIL). http(s)://
# covers (e.g. Spotify) are fetched + cached to disk ONLY if the config sets
# fetch_remote_art = true (default false) -- a locked screen makes no network
# request unless the user opts in. The accent colour is SAMPLED from the cover
# pixels when we have them; otherwise a stable per-title hash tint. No cover ->
# a music-note fallback.
#
# One plugin, two layouts (compact + star), picked from config -- the plugin
# reads its own config (SDK does config transport, plugin does interpretation):
#   layout = cfg.get("layout", "compact")
#
# Text uses PangoCairo, not cairo's toy show_text: real titles need shaping and
# clean end-ellipsization (a long CJK title is the test case), which show_text
# cannot do. This needs the Pango/PangoCairo GI typelibs (the flake's dev shell
# wires pango + harfbuzz onto GI_TYPELIB_PATH / LD_LIBRARY_PATH).
#
# A real plugin vendors veiland_plugin.py next to itself. This example adds the
# repo's python/ dir to sys.path so it runs straight from the tree.

import colorsys
import hashlib
import json
import math
import os
import sys
import urllib.parse
import urllib.request

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# gi bindings come before the SDK import only for the version pins; pycairo is
# imported so PangoCairo can render onto a cairo.Context. (E402: after the
# sys.path shim so the SDK import resolves.)
import gi  # noqa: E402

gi.require_version("Pango", "1.0")  # noqa: E402
gi.require_version("PangoCairo", "1.0")  # noqa: E402

import cairo  # noqa: E402
from gi.repository import Pango, PangoCairo  # noqa: E402
from jeepney import (  # noqa: E402
    DBusAddress,
    MatchRule,
    Properties,
    message_bus,
    new_method_call,
)
from jeepney.io.blocking import open_dbus_connection  # noqa: E402
from PIL import Image  # noqa: E402

import veiland_plugin as vp  # noqa: E402

# ------------------------------------------------------------- MPRIS over D-Bus
#
# A tiny read-only MPRIS client on the session bus. It finds the active
# org.mpris.MediaPlayer2.* player, reads the Player interface properties, and
# subscribes to PropertiesChanged so a track change wakes the plugin. Everything
# here is best-effort: any D-Bus hiccup yields "nothing playing", never a crash
# (this is a locker; the widget must never take it down). Read-only by design --
# we never call Play/Pause/Next; the core can't forward clicks anyway.

MPRIS_PREFIX = "org.mpris.MediaPlayer2."
MPRIS_PATH = "/org/mpris/MediaPlayer2"
PLAYER_IFACE = "org.mpris.MediaPlayer2.Player"


def log(msg):
    # One-line stderr log, tagged. Used at the untrusted-I/O boundaries below
    # (D-Bus, cover fetch): they catch broadly and degrade rather than crash the
    # locker, but a persistent failure must not be silent -- log it so it is
    # diagnosable instead of an unexplained "nothing playing".
    print(f"now-playing: {msg}", file=sys.stderr, flush=True)


class MprisClient:
    def __init__(self):
        # One blocking session-bus connection. Its socket fd (fileno()) goes on
        # the pacer's extra_fds so PropertiesChanged wakes us with no polling.
        self.conn = open_dbus_connection(bus="SESSION")
        # Subscribe to Player PropertiesChanged from any sender: the broadcast
        # signal MPRIS players emit on track/status/position change. jeepney
        # queues matching signals; we drain them (drain_signals) to reset the
        # tick without caring about their contents -- we just re-read on wake.
        rule = MatchRule(
            type="signal",
            interface="org.freedesktop.DBus.Properties",
            member="PropertiesChanged",
            path=MPRIS_PATH,
        )
        self.conn.send_and_get_reply(message_bus.AddMatch(rule))

    def fileno(self):
        # The bus socket fd, for the pacer's extra_fds. jeepney's blocking
        # connection wraps a real socket at .sock.
        return self.conn.sock.fileno()

    def drain_signals(self):
        # The pacer selected the bus fd readable, so there is at least one
        # message waiting; consume all currently-queued ones without blocking.
        # We don't parse them -- their arrival is the whole message ("something
        # changed, re-read"). receive(timeout=0) raises when nothing is left, so
        # that (and any transient error) is the loop's clean exit.
        while True:
            try:
                self.conn.receive(timeout=0)
            except Exception:
                break

    def _list_players(self):
        dbus = DBusAddress(
            "/org/freedesktop/DBus",
            bus_name="org.freedesktop.DBus",
            interface="org.freedesktop.DBus",
        )
        reply = self.conn.send_and_get_reply(new_method_call(dbus, "ListNames"))
        return [n for n in reply.body[0] if n.startswith(MPRIS_PREFIX)]

    def read(self):
        # Return the active player's state dict, or None if nothing is playing.
        # "Active" = the first player that is Playing; else the first that
        # exists (so a paused player still shows).
        try:
            players = self._list_players()
        except Exception as e:
            # Deliberate broad catch: D-Bus is untrusted, flaky external I/O and
            # this is a locker plugin -- it must degrade to "nothing playing",
            # never crash the widget (see the no-panic-on-plugin-input rule). We
            # log so a persistent bus failure is diagnosable, not silent.
            log(f"mpris ListNames failed: {e}")
            return None
        if not players:
            return None

        best = None
        for bus_name in players:
            props = self._get_props(bus_name)
            if props is None:
                continue
            if props.get("PlaybackStatus") == "Playing":
                return self._to_track(props)  # a playing one wins outright
            if best is None:
                best = props  # remember the first readable (likely paused)
        return self._to_track(best) if best else None

    def _get_props(self, bus_name):
        try:
            addr = DBusAddress(MPRIS_PATH, bus_name=bus_name, interface=PLAYER_IFACE)
            reply = self.conn.send_and_get_reply(Properties(addr).get_all())
            # a{sv}: {name: (signature, value)} -> unwrap the variant tuples.
            return {k: v[1] for k, v in reply.body[0].items()}
        except Exception as e:
            # Same boundary as read(): a player that vanished mid-read or returns
            # junk must not take down the widget. Log and skip this player.
            log(f"mpris GetAll({bus_name}) failed: {e}")
            return None

    @staticmethod
    def _to_track(props):
        # Map raw MPRIS properties to the track dict the drawing code expects.
        md = {k: v[1] for k, v in props.get("Metadata", {}).items()}
        artists = md.get("xesam:artist", [])
        artist = ", ".join(artists) if isinstance(artists, list) else str(artists)
        return {
            "title": md.get("xesam:title") or "Unknown title",
            "artist": artist or "Unknown artist",
            "art_url": md.get("mpris:artUrl") or "",
            "elapsed": props.get("Position", 0) / 1_000_000,  # us -> s
            "total": md.get("mpris:length", 0) / 1_000_000,
            "playing": props.get("PlaybackStatus") == "Playing",
        }

    def close(self):
        try:
            self.conn.close()
        except Exception:
            pass


# --------------------------------------------------------------- album art

# Remote covers (e.g. Spotify) are cached to disk so a re-lock or a plugin
# respawn (monitor hotplug) doesn't re-fetch. The bound is deliberately blunt:
# covers are tiny and re-fetchable, so when the dir hits _CACHE_MAX we just clear
# it wholesale rather than track per-file ages -- the simplest thing that keeps
# the cache from growing without limit.
_CACHE_DIR = os.path.join(
    os.environ.get("XDG_CACHE_HOME") or os.path.expanduser("~/.cache"),
    "veiland-now-playing",
)
_CACHE_MAX = 128


def _cache_path(url):
    return os.path.join(_CACHE_DIR, hashlib.sha256(url.encode()).hexdigest()[:32])


def _fetch_to_cache(url, dest):
    # Fetch url to dest, clearing the cache first if it's full. Atomic write via
    # a .tmp rename so a mid-fetch interruption (e.g. suspend) never leaves a
    # truncated file that would later decode as a broken cover.
    os.makedirs(_CACHE_DIR, exist_ok=True)
    entries = os.listdir(_CACHE_DIR)
    if len(entries) >= _CACHE_MAX:
        for f in entries:
            os.remove(os.path.join(_CACHE_DIR, f))
    req = urllib.request.Request(url, headers={"User-Agent": "veiland"})
    with urllib.request.urlopen(req, timeout=5) as r:
        data = r.read()
    tmp = dest + ".tmp"
    with open(tmp, "wb") as f:
        f.write(data)
    os.replace(tmp, dest)


def load_cover(art_url, fetch_remote):
    # Resolve mpris:artUrl to a PIL RGB image, or None for the note fallback.
    #   file://       -> decode locally (no network, always).
    #   http(s)://    -> only if fetch_remote; fetch once (cached), then decode.
    #   else / error  -> None. Never raises: a bad cover must not break the card.
    if not art_url:
        return None
    try:
        if art_url.startswith("file://"):
            path = urllib.parse.unquote(urllib.parse.urlparse(art_url).path)
            return Image.open(path).convert("RGB")
        if art_url.startswith(("http://", "https://")):
            if not fetch_remote:
                return None
            cached = _cache_path(art_url)
            if not os.path.exists(cached):
                _fetch_to_cache(art_url, cached)
            return Image.open(cached).convert("RGB")
    except Exception as e:
        log(f"cover load failed: {e}")
        return None
    return None


def accent_from_cover(image):
    # The sampled-from-art accent: average the cover to one colour (a 1x1 box
    # downscale does the averaging), then push saturation and clamp lightness so
    # the progress bar / dot read as a colour, not mud. Returns (r, g, b) 0..1.
    r, g, b = (c / 255 for c in image.resize((1, 1)).getpixel((0, 0))[:3])
    h, light, s = colorsys.rgb_to_hls(r, g, b)
    s = min(1.0, s * 1.6 + 0.15)
    light = min(0.72, max(0.42, light))
    return colorsys.hls_to_rgb(h, light, s)


def tint_from_title(title):
    # Stable fallback tint when there's no cover to sample: hash the title to a
    # hue at fixed saturation/lightness. Same song -> same colour.
    hue = int(hashlib.sha256(title.encode()).hexdigest(), 16) % 360 / 360
    return colorsys.hls_to_rgb(hue, 0.58, 0.6)


def read_track(mpris, fetch_remote, cover_cache):
    # The one call the render loop makes: read the player, resolve the cover to a
    # ready-to-paint cairo surface + accent, and return the track dict the
    # drawing code wants (or None -> "nothing playing"). Resolving the cover
    # (decode + reorder to a cairo surface + sample the accent, maybe a network
    # fetch) is memoized by art_url in cover_cache so we do it once per track,
    # not every frame -- the loop calls this ~1x/second, and the byte reorder in
    # _pil_to_surface is the priciest op in the draw.
    track = mpris.read()
    if track is None:
        return None

    url = track["art_url"]
    if url in cover_cache:
        surface, accent = cover_cache[url]
    else:
        image = load_cover(url, fetch_remote)
        surface = _pil_to_surface(image) if image else None
        accent = accent_from_cover(image) if image else tint_from_title(track["title"])
        cover_cache.clear()  # one entry: only the current track's cover matters
        # Cache the cairo surface, not the PIL image: create_for_data keeps a
        # reference to its backing bytearray, so holding the surface keeps the
        # bytes alive too. Same object is repainted (scaled) every frame.
        cover_cache[url] = (surface, accent)

    track["cover"] = surface  # cairo surface or None (-> note fallback)
    track["accent"] = accent
    return track


# ------------------------------------------------------------------- colors

GLASS = (15 / 255, 18 / 255, 28 / 255, 176 / 255)  # rgba(15,18,28,0.69) exactly
PRIMARY = (0xF2 / 255, 0xF4 / 255, 0xF8 / 255)  # near-white title
SECONDARY = (0x9A / 255, 0xA3 / 255, 0xB4 / 255)  # dimmed artist / metadata
TRACK_BG = (1, 1, 1, 0.16)  # unfilled progress track
HAIRLINE = (1, 1, 1, 0.09)  # top highlight on the glass


# ------------------------------------------------------------------- helpers


def rounded_rect(cr, x, y, w, h, r):
    # cairo has no rounded-rectangle primitive; trace one from four arcs.
    # (Same helper as battery_cairo.py.)
    r = min(r, w / 2, h / 2)
    cr.new_sub_path()
    cr.arc(x + w - r, y + r, r, -math.pi / 2, 0)
    cr.arc(x + w - r, y + h - r, r, 0, math.pi / 2)
    cr.arc(x + r, y + h - r, r, math.pi / 2, math.pi)
    cr.arc(x + r, y + r, r, math.pi, 3 * math.pi / 2)
    cr.close_path()


def fmt_time(seconds):
    seconds = max(0, int(seconds))
    return f"{seconds // 60}:{seconds % 60:02d}"


def bar_fill(accent, playing):
    # The progress-bar fill colour: the accent when playing, desaturated halfway
    # toward grey when paused so playing vs paused reads at a glance. Shared by
    # both layouts so the paused look stays one decision.
    if playing:
        return accent
    return tuple((a + s) / 2 for a, s in zip(accent, SECONDARY))


def _line_layout(cr, text, max_w, px, weight):
    # One shaped, end-ellipsized single line. Shared by the top-left and
    # vertically-centered draw helpers.
    layout = PangoCairo.create_layout(cr)
    font = Pango.FontDescription()
    font.set_family("sans-serif")
    font.set_absolute_size(px * Pango.SCALE)
    font.set_weight(weight)
    layout.set_font_description(font)
    layout.set_width(int(max_w * Pango.SCALE))
    layout.set_ellipsize(Pango.EllipsizeMode.END)
    layout.set_text(text, -1)
    return layout


def draw_ellipsized(cr, text, x, y, max_w, px, rgb, weight=Pango.Weight.NORMAL):
    # Draw a shaped, end-ellipsized line with its TOP-LEFT at (x, y). The whole
    # reason this widget uses PangoCairo and not cairo's toy text.
    layout = _line_layout(cr, text, max_w, px, weight)
    cr.move_to(x, y)
    cr.set_source_rgb(*rgb)
    PangoCairo.show_layout(cr, layout)


def draw_ellipsized_centered(
    cr, text, x, cy, max_w, px, rgb, weight=Pango.Weight.NORMAL
):
    # Same, but vertically CENTERED on cy: the measured line height (which
    # differs by script -- CJK is taller than Latin) grows symmetrically around
    # cy instead of downward, so a tall CJK title can't shove what's below it.
    layout = _line_layout(cr, text, max_w, px, weight)
    _, logical = layout.get_pixel_extents()
    cr.move_to(x, cy - logical.height / 2)
    cr.set_source_rgb(*rgb)
    PangoCairo.show_layout(cr, layout)


def draw_ellipsized_right(cr, text, right_x, y, px, rgb, weight=Pango.Weight.NORMAL):
    # Same shaped line, but its RIGHT edge sits at right_x (measure, then place).
    # Used for the right-aligned total time in both layouts. 1e6 max width so
    # the time never ellipsizes -- it is always short.
    layout = _line_layout(cr, text, 1e6, px, weight)
    _, logical = layout.get_pixel_extents()
    cr.move_to(right_x - logical.width, y)
    cr.set_source_rgb(*rgb)
    PangoCairo.show_layout(cr, layout)


def _pil_to_surface(image):
    # PIL RGB -> a cairo ARGB32 ImageSurface. cairo's ARGB32 is premultiplied
    # BGRA in memory (little-endian); the cover is opaque, so premultiply is a
    # no-op and we just reorder RGB->BGRA with a full-alpha byte. bytearray so
    # create_for_data gets a writable buffer.
    w, h = image.size
    rgb = image.tobytes("raw", "RGB")
    buf = bytearray(w * h * 4)
    buf[0::4] = rgb[2::3]  # B
    buf[1::4] = rgb[1::3]  # G
    buf[2::4] = rgb[0::3]  # R
    buf[3::4] = b"\xff" * (w * h)  # A
    stride = cairo.ImageSurface.format_stride_for_width(cairo.FORMAT_ARGB32, w)
    # Row padding: if the stride is wider than w*4, tobytes gave us tight rows;
    # rebuild per-row only when needed (rare -- ARGB32 stride is w*4 for most w).
    if stride != w * 4:
        padded = bytearray(stride * h)
        for y in range(h):
            padded[y * stride : y * stride + w * 4] = buf[y * w * 4 : (y + 1) * w * 4]
        buf = padded
    return cairo.ImageSurface.create_for_data(buf, cairo.FORMAT_ARGB32, w, h, stride)


def draw_cover(cr, surface, x, y, size):
    # Paint a cover surface into the size x size square at (x, y), scaled to
    # fill. The surface is built once per track (_pil_to_surface is not cheap --
    # a full-image byte reorder) and cached, so this is called with the same
    # object every frame; only the scale here is per-frame. Caller has already
    # clipped to the rounded-rect art well.
    sw, sh = surface.get_width(), surface.get_height()
    cr.save()
    cr.translate(x, y)
    cr.scale(size / sw, size / sh)
    cr.set_source_surface(surface, 0, 0)
    cr.get_source().set_filter(cairo.FILTER_BILINEAR)
    cr.paint()
    cr.restore()


def draw_note_glyph(cr, cx, cy, size, rgb):
    # A simple eighth-note: a filled ellipse head + a stem. Used when the track
    # has no cover art (the tasteful fallback, never a broken-image box).
    cr.save()
    cr.set_source_rgb(*rgb)
    head_r = size * 0.22
    hx, hy = cx - size * 0.12, cy + size * 0.24
    cr.save()
    cr.translate(hx, hy)
    cr.scale(1.25, 1.0)
    cr.arc(0, 0, head_r, 0, 2 * math.pi)
    cr.restore()
    cr.fill()
    # stem up from the right of the head, with a small flag
    stem_w = max(1.5, size * 0.05)
    stem_top = cy - size * 0.30
    cr.rectangle(hx + head_r * 1.05, stem_top, stem_w, (hy) - stem_top)
    cr.fill()
    cr.move_to(hx + head_r * 1.05 + stem_w, stem_top)
    cr.curve_to(
        cx + size * 0.30,
        stem_top + size * 0.10,
        cx + size * 0.28,
        stem_top + size * 0.28,
        cx + size * 0.14,
        stem_top + size * 0.34,
    )
    cr.set_line_width(stem_w)
    cr.stroke()
    cr.restore()


def draw_pause_badge(cr, x, y, size, accent):
    # The star layout's paused indicator: a small "Paused" pill over the art
    # corner (the compact layout uses a hollow dot instead -- the star card is
    # big enough to carry a legible badge). Two bars + a word.
    bar_w = size * 0.14
    bar_h = size * 0.5
    gap = size * 0.12
    text = "PAUSED"
    lay = _line_layout(cr, text, 1e6, size * 0.62, Pango.Weight.SEMIBOLD)
    _, log = lay.get_pixel_extents()
    pill_h = size * 1.1
    pill_w = bar_w * 2 + gap + size * 0.4 + log.width + size * 0.5
    rounded_rect(cr, x, y, pill_w, pill_h, pill_h / 2)
    cr.set_source_rgba(10 / 255, 13 / 255, 20 / 255, 0.66)
    cr.fill()
    # two pause bars
    bx = x + size * 0.35
    by = y + (pill_h - bar_h) / 2
    cr.set_source_rgb(*PRIMARY)
    cr.rectangle(bx, by, bar_w, bar_h)
    cr.rectangle(bx + bar_w + gap, by, bar_w, bar_h)
    cr.fill()
    # label
    cr.move_to(bx + bar_w * 2 + gap + size * 0.35, y + (pill_h - log.height) / 2)
    cr.set_source_rgb(*PRIMARY)
    PangoCairo.show_layout(cr, lay)


# ----------------------------------------------------------------- star layout


def draw_star(cr, w, h, track):
    # The card as centerpiece: a large PORTRAIT card (big art on top, meta
    # stacked below) centered on a TRANSPARENT full surface -- whatever plugin
    # sits behind it (a wallpaper/gradient at a lower z_index, or the core's
    # background) shows through. The star's region is the WHOLE lock surface (see
    # now_playing_star.toml), so w,h here are the full screen -- unlike compact,
    # where the card IS the region. Card size is driven by the surface's smaller
    # dimension so it stays a sensible portrait block on any aspect ratio.
    accent = track["accent"] if track else SECONDARY
    playing = track["playing"] if track else False

    # card geometry: a portrait box, art + a text stack under it
    card_w = min(w * 0.30, h * 0.42, 420.0)
    pad = card_w * 0.075
    art = card_w - 2 * pad
    # text stack height under the art: title + artist + (bar + times if playing)
    stack_h = art * (0.62 if track else 0.34)
    card_h = pad + art + art * 0.10 + stack_h + pad
    cx = (w - card_w) / 2
    cy = (h - card_h) / 2

    # -- glass card + top hairline highlight --
    radius = card_w * 0.06
    rounded_rect(cr, cx, cy, card_w, card_h, radius)
    cr.set_source_rgba(*GLASS)
    cr.fill()
    rounded_rect(cr, cx, cy, card_w, card_h, radius)
    cr.set_source_rgba(*HAIRLINE)
    cr.set_line_width(1.0)
    cr.stroke()

    # -- album art (big square) at the top --
    ax, ay = cx + pad, cy + pad
    art_r = art * 0.055
    cr.save()
    rounded_rect(cr, ax, ay, art, art, art_r)
    cr.clip()
    if track and track.get("cover"):
        draw_cover(cr, track["cover"], ax, ay, art)
    else:
        cr.set_source_rgba(1, 1, 1, 0.06)
        cr.rectangle(ax, ay, art, art)
        cr.fill()
        draw_note_glyph(cr, ax + art / 2, ay + art / 2, art * 0.8, PRIMARY)
    cr.restore()

    # -- paused badge over the art's top-right corner --
    if track and not playing:
        bsize = art * 0.11
        draw_pause_badge(cr, ax + pad * 0.5, ay + pad * 0.5, bsize, accent)

    # -- text stack under the art --
    tx = cx + pad
    tw = card_w - 2 * pad
    ty0 = ay + art + art * 0.10  # top of the text stack
    title_cy = ty0 + stack_h * 0.14
    artist_cy = ty0 + stack_h * 0.36
    dot_r = card_w * 0.016
    tstart = tx
    if track and playing:
        cr.save()
        cr.set_source_rgb(*accent)
        cr.arc(tx + dot_r, title_cy, dot_r, 0, 2 * math.pi)
        cr.fill()
        cr.restore()
        tstart = tx + dot_r * 2 + pad * 0.5
    draw_ellipsized_centered(
        cr,
        track["title"] if track else "Nothing playing",
        tstart,
        title_cy,
        tx + tw - tstart,
        card_w * 0.058,
        PRIMARY,
        weight=Pango.Weight.SEMIBOLD,
    )
    draw_ellipsized_centered(
        cr,
        track["artist"] if track else "No active player",
        tx,
        artist_cy,
        tw,
        card_w * 0.046,
        SECONDARY,
    )

    # -- progress: filled track + times, only when there is a track --
    if track:
        bar_h = card_w * 0.016
        bar_y = ty0 + stack_h * 0.62
        rounded_rect(cr, tx, bar_y, tw, bar_h, bar_h / 2)
        cr.set_source_rgba(*TRACK_BG)
        cr.fill()
        frac = track["elapsed"] / track["total"] if track["total"] else 0
        frac = max(0.0, min(1.0, frac))
        if tw * frac > bar_h:
            rounded_rect(cr, tx, bar_y, tw * frac, bar_h, bar_h / 2)
            cr.set_source_rgb(*bar_fill(accent, playing))
            cr.fill()
        times_px = card_w * 0.036
        ty = bar_y + bar_h + card_w * 0.03
        draw_ellipsized(
            cr, fmt_time(track["elapsed"]), tx, ty, tw * 0.5, times_px, SECONDARY
        )
        draw_ellipsized_right(
            cr, fmt_time(track["total"]), tx + tw, ty, times_px, SECONDARY
        )


# --------------------------------------------------------------- compact layout


def draw_compact(cr, w, h, track):
    # The bottom-left chip: square art left, title/artist stacked right, a filled
    # progress track spanning the bottom. Sizes are derived from the card height
    # so the layout scales with whatever region the host gives us.
    pad = h * 0.14
    accent = track["accent"] if track else SECONDARY
    playing = track["playing"] if track else False

    # -- glass card: translucent fill + a top hairline highlight --
    radius = h * 0.16
    rounded_rect(cr, 0, 0, w, h, radius)
    cr.set_source_rgba(*GLASS)
    cr.fill()

    # -- a faint accent bloom in the top-left corner (over the art side) --
    if track:
        bloom = cairo.RadialGradient(0, 0, 0, 0, 0, w * 0.45)
        bloom.add_color_stop_rgba(0.0, *accent, 0.16)
        bloom.add_color_stop_rgba(1.0, *accent, 0.0)
        rounded_rect(cr, 0, 0, w, h, radius)
        cr.set_source(bloom)
        cr.fill()

    # -- album art (square) on the left --
    art = h - 2 * pad
    ax, ay = pad, pad
    art_r = art * 0.16
    cr.save()
    rounded_rect(cr, ax, ay, art, art, art_r)
    cr.clip()
    if track and track.get("cover"):
        draw_cover(cr, track["cover"], ax, ay, art)
    else:
        # fallback well: faint accent tint + centered music note
        cr.set_source_rgba(1, 1, 1, 0.06)
        cr.rectangle(ax, ay, art, art)
        cr.fill()
        draw_note_glyph(cr, ax + art / 2, ay + art / 2, art * 0.9, PRIMARY)
    cr.restore()

    # -- text column: explicit, NON-OVERLAPPING vertical zones --
    # Everything is a fraction of the card height h, so the four stacked
    # elements (title / artist / bar / times) never collide regardless of
    # script. Each text line is CENTERED on its zone's y: a tall CJK ascent
    # grows symmetrically around the center instead of downward into the zone
    # below (the bug an earlier top-anchored version had). Zone centers:
    #   title 0.26h  ·  artist 0.46h  ·  bar 0.66h  ·  times 0.86h
    tx = ax + art + pad
    text_w = w - tx - pad
    dot_r = h * 0.048
    title_cy = h * 0.26
    artist_cy = h * 0.46
    tstart = tx
    if track and playing:
        cr.save()
        cr.set_source_rgb(*accent)
        cr.arc(tx + dot_r, title_cy, dot_r, 0, 2 * math.pi)
        cr.fill()
        cr.restore()
        tstart = tx + dot_r * 2 + pad * 0.4
    draw_ellipsized_centered(
        cr,
        track["title"] if track else "Nothing playing",
        tstart,
        title_cy,
        tx + text_w - tstart,
        h * 0.18,
        PRIMARY,
        weight=Pango.Weight.SEMIBOLD,
    )
    draw_ellipsized_centered(
        cr,
        track["artist"] if track else "No active player",
        tx,
        artist_cy,
        text_w,
        h * 0.145,
        SECONDARY,
    )

    # -- progress: a filled track + elapsed / total times, only when playing --
    if track:
        bar_h = h * 0.05
        bar_y = h * 0.66 - bar_h / 2
        bar_x = tx
        bar_w = text_w
        # track
        rounded_rect(cr, bar_x, bar_y, bar_w, bar_h, bar_h / 2)
        cr.set_source_rgba(*TRACK_BG)
        cr.fill()
        frac = track["elapsed"] / track["total"] if track["total"] else 0
        frac = max(0.0, min(1.0, frac))
        if bar_w * frac > bar_h:
            rounded_rect(cr, bar_x, bar_y, bar_w * frac, bar_h, bar_h / 2)
            cr.set_source_rgb(*bar_fill(accent, playing))  # accent, grey if paused
            cr.fill()
        # times, centered on the 0.86h zone
        times_px = h * 0.11
        ty = h * 0.86 - times_px / 2
        draw_ellipsized(
            cr, fmt_time(track["elapsed"]), bar_x, ty, bar_w * 0.5, times_px, SECONDARY
        )
        # right-aligned total
        draw_ellipsized_right(
            cr, fmt_time(track["total"]), bar_x + bar_w, ty, times_px, SECONDARY
        )


# ------------------------------------------------------------------ draw entry


def draw_into(buf, layout_name, track):
    # Zero-copy: wrap buf.map()'s memoryview in a cairo surface and draw straight
    # into GPU-visible memory. cairo needs the MAP stride, not buf.stride.
    with buf.map() as (mem, map_stride):
        surface = cairo.ImageSurface.create_for_data(
            mem, cairo.FORMAT_ARGB32, buf.width, buf.height, map_stride
        )
        cr = cairo.Context(surface)
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.set_operator(cairo.OPERATOR_OVER)
        # Two layouts, picked from config. Unknown names fall back to compact
        # rather than drawing nothing.
        if layout_name == "star":
            draw_star(cr, buf.width, buf.height, track)
        else:
            draw_compact(cr, buf.width, buf.height, track)
        surface.flush()
        surface.finish()


def display_signature(track):
    # Everything that changes what the card LOOKS like -- so we can skip a
    # redraw when nothing visible changed. Note the 1s tick fires every second,
    # but the elapsed time only *displays* to whole seconds, so the signature
    # only changes once per shown second: no track -> a constant; else the shown
    # fields plus the whole-second elapsed. This is what stops the widget from
    # re-rendering (and re-sending a whole-screen buffer) on every compositor
    # repaint while the user is typing -- only real content changes redraw.
    if track is None:
        return ("idle",)
    return (
        track["title"],
        track["artist"],
        track["art_url"],  # cover change (same title, new art) still redraws
        track["playing"],
        int(track["elapsed"]),
        track["total"],
    )


# ----------------------------------------------------------------- main


def main():
    conn = vp.Connection.connect("now-playing", "0.1.0")
    cfg = conn.wait_for_configure()

    # Config transport is the SDK's job; interpretation is ours.
    #   layout           = "compact" | "star"   (default compact)
    #   fetch_remote_art = bool: allow fetching http(s):// covers (default false;
    #                      a locked screen makes no network request unless asked)
    plugin_cfg = json.loads(os.environ.get("VEILAND_PLUGIN_CONFIG") or "{}")
    layout_name = plugin_cfg.get("layout", "compact")
    fetch_remote = bool(plugin_cfg.get("fetch_remote_art", False))

    mpris = MprisClient()
    cover_cache = {}  # art_url -> (cairo surface | None, accent); one entry at a time

    def current_track():
        return read_track(mpris, fetch_remote, cover_cache)

    dev = vp.GbmDevice()
    # BufferChain, not a single LinearBuffer: this card redraws (the progress bar
    # advances, the track changes), and a CPU plugin redrawing one buffer in
    # place races the host's live zero-copy sampling -> a flicker whose frequency
    # scales with draw time (this card's is heavy -> it was constant). The chain
    # hands out the buffer the host is NOT showing, so the shown one is never
    # mid-edit. See veiland_plugin.BufferChain.
    chain = vp.BufferChain(dev, cfg.region_w, cfg.region_h)

    last_sig = None  # signature of what we last drew; None -> never drawn
    _UNREAD = object()  # sentinel: no track read this cycle (distinct from None)
    pending = _UNREAD  # a track a change-check already read, for RENDER to reuse

    def check_dirty():
        # Read the player once, stash it for the upcoming RENDER, and mark_dirty
        # only if what's shown actually changed. The stash is what stops RENDER
        # from re-reading (a second ListNames + per-player GetAll round-trip) the
        # state we just read on this same wake.
        nonlocal pending
        track = current_track()
        if display_signature(track) != last_sig:
            pending = track
            pacer.mark_dirty()

    pacer = vp.FramePacer.on_demand()
    # The MPRIS bus socket is an extra fd: a PropertiesChanged (track/status
    # change) wakes us immediately. A 1s tick advances the progress bar between
    # changes. We only mark_dirty() when the *displayed* content changed (see
    # display_signature), so a static card sits idle instead of re-rendering
    # every tick / compositor repaint.
    for ev in pacer.events(conn, timeout=1.0, extra_fds=[mpris.fileno()]):
        if ev.kind is vp.Event.RENDER:
            # Reuse the track the change-check already read this cycle; only read
            # afresh when RENDER was triggered without one (the first frame, or a
            # RECONFIGURE-forced redraw).
            track = current_track() if pending is _UNREAD else pending
            pending = _UNREAD
            draw_into(chain.acquire(), layout_name, track)
            last_sig = display_signature(track)
            chain.send(conn)
            pacer.submitted()
        elif ev.kind is vp.Event.RECONFIGURE:
            cfg = ev.configure
            chain = chain.resize_or_keep(dev, cfg)
            last_sig = None  # new surface size -> force a redraw
            pending = _UNREAD  # its size, not the track, changed -> read at RENDER
            pacer.mark_dirty()
        elif ev.kind is vp.Event.FD_READY:
            # The bus woke us: a player emitted PropertiesChanged. Drain the
            # queued signal(s) -- their arrival is the message -- then redraw if
            # what's shown actually changed.
            mpris.drain_signals()
            check_dirty()
        elif ev.kind is vp.Event.TIMEOUT:
            check_dirty()  # re-read and redraw ONLY if what's shown changed
        elif ev.kind is vp.Event.SHUTDOWN:
            break

    chain.close()
    dev.close()
    mpris.close()
    conn.close()


if __name__ == "__main__":
    main()
