#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The avatar + greeting widget: a round-cropped user picture over a one-line
# greeting in the family glass pill -- the "this is MY lockscreen" ingredient,
# meant to sit just above the core's password indicator on the center column.
# Pure config: no D-Bus, no network, no polling. It draws once at Configure and
# then idles (static plugins idling is legal; only greeting = "auto" wakes on a
# slow tick to roll Good morning -> afternoon -> evening over).
#
# Zero-config personalisation, each step falling back to the next:
#   name:   [plugin.config] name -> the GECOS full name in /etc/passwd -> $USER
#   avatar: [plugin.config] avatar -> ~/.face (the display-manager convention)
#           -> a tinted initials disc (hue hashed from the name, the same
#           stable-tint trick now_playing.py uses for coverless tracks)
#
# Two layouts from one config key, like now_playing's compact/star:
#   layout = "stack"  avatar above the greeting pill (the center-column look)
#   layout = "row"    avatar inside a wide capsule beside the text, sized for a
#                     status-cluster-height corner region
#
# Text uses PangoCairo (real shaping + end-ellipsization, same as now_playing);
# the pill glyph is icons/user.svg via the veiland_svg companion, so this needs
# the gi stack: pygobject3 + Pango/PangoCairo + librsvg typelibs (the flake's
# dev shell wires them). Image decode is PIL, as now_playing does for covers.
#
# A real plugin vendors veiland_plugin.py (and veiland_svg.py) next to itself.
# This example adds the repo's python/ dir to sys.path so it runs from the tree.

from __future__ import annotations

import os
import sys
from dataclasses import dataclass
from typing import Any

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# These follow the sys.path shim so the SDK imports resolve (E402). gi version
# pins come before gi.repository; cairo is imported before veiland_svg so the
# pycairo<->GObject foreign bridge is registered in-process (see battery_svg.py).
import colorsys  # noqa: E402
import hashlib  # noqa: E402
import json  # noqa: E402
import math  # noqa: E402
import pwd  # noqa: E402
import time  # noqa: E402

import gi  # noqa: E402

gi.require_version("Pango", "1.0")  # noqa: E402
gi.require_version("PangoCairo", "1.0")  # noqa: E402

import cairo  # noqa: E402
from gi.repository import Pango, PangoCairo  # noqa: E402
from PIL import Image  # noqa: E402

import veiland_plugin as vp  # noqa: E402
import veiland_svg as vs  # noqa: E402
import veiland_text as vt  # noqa: E402

# The shaped single-line layout builder now lives in the text companion (it was
# copy-pasted here and in now_playing.py). Alias it to the old private name so
# the measuring/draw code below reads unchanged.
_line_layout = vt.line_layout

ICON_PATH = os.path.join(
    os.path.dirname(os.path.abspath(__file__)), "icons", "user.svg"
)

# Family defaults: the status pills' glass, a thin translucent ring, near-white
# text. All three are RGBA 0..1 floats where alpha IS the opacity, overridable
# per config (pill_color / ring_color / text_color) via veiland_svg.parse_color.
GLASS = (15 / 255, 18 / 255, 28 / 255, 176 / 255)
RING = (1.0, 1.0, 1.0, 0.22)
TEXT = (1.0, 1.0, 1.0, 0.94)

DEFAULT_GREETING = "Hi, {name}"


def log(msg: str) -> None:
    print(f"avatar: {msg}", file=sys.stderr)


# ------------------------------------------------------------- config reading


def resolve_name(cfg: dict[str, Any]) -> str:
    # name -> GECOS full name -> $USER. The GECOS field ("Sylvain Chateau,,,")
    # is where a full name already lives on most systems, so an empty
    # [plugin.config] still greets by name; only the part before the first
    # comma is the name proper.
    raw = cfg.get("name")
    if isinstance(raw, str) and raw.strip():
        return raw.strip()
    if raw is not None:
        log(f"name: expected a string, got {raw!r}; using defaults")
    try:
        pw = pwd.getpwuid(os.getuid())
        gecos = pw.pw_gecos.split(",")[0].strip()
        if gecos:
            return gecos
        if pw.pw_name:
            return pw.pw_name
    except KeyError:
        pass
    return os.environ.get("USER") or "there"


def resolve_greeting_template(cfg: dict[str, Any]) -> str:
    raw = cfg.get("greeting", DEFAULT_GREETING)
    if not isinstance(raw, str):
        log(f"greeting: expected a string, got {raw!r}; using default")
        return DEFAULT_GREETING
    return raw


def greeting_now(template: str, name: str) -> str:
    # "auto" buckets the local hour; anything else is a literal with {name}
    # replaced via str.replace, NOT str.format -- the template is untrusted
    # config, and a stray "{" must mis-render at worst, never raise. An empty
    # template means avatar only.
    if template == "auto":
        hour = time.localtime().tm_hour
        if 5 <= hour < 12:
            word = "Good morning"
        elif 12 <= hour < 18:
            word = "Good afternoon"
        elif 18 <= hour < 23:
            word = "Good evening"
        else:
            word = "Good night"
        return f"{word}, {name}"
    return template.replace("{name}", name)


def resolve_layout(cfg: dict[str, Any]) -> str:
    raw = cfg.get("layout", "stack")
    if raw in ("stack", "row"):
        return str(raw)
    log(f'layout: expected "stack" or "row", got {raw!r}; using "stack"')
    return "stack"


# -------------------------------------------------------------- avatar loading


def _pil_to_surface(image: Image.Image) -> cairo.ImageSurface:
    # PIL RGB -> a cairo ARGB32 ImageSurface: reorder RGB->BGRA with a full
    # alpha byte, padding rows if cairo wants a wider stride. (Same helper as
    # now_playing.py; the image is opaque by the time it gets here.)
    w, h = image.size
    rgb = image.tobytes("raw", "RGB")
    buf = bytearray(w * h * 4)
    buf[0::4] = rgb[2::3]  # B
    buf[1::4] = rgb[1::3]  # G
    buf[2::4] = rgb[0::3]  # R
    buf[3::4] = b"\xff" * (w * h)  # A
    stride = cairo.ImageSurface.format_stride_for_width(cairo.FORMAT_ARGB32, w)
    if stride != w * 4:
        padded = bytearray(stride * h)
        for y in range(h):
            padded[y * stride : y * stride + w * 4] = buf[y * w * 4 : (y + 1) * w * 4]
        buf = padded
    return cairo.ImageSurface.create_for_data(buf, cairo.FORMAT_ARGB32, w, h, stride)


def load_avatar(cfg: dict[str, Any]) -> cairo.ImageSurface | None:
    # Explicit path first, then ~/.face; None means "draw the initials disc".
    # Any failure logs one line and falls through -- a bad path or corrupt
    # image mis-themes the widget, it never crashes it. The decode happens once
    # at startup at a capped size; draw time only scales the cached surface, so
    # reconfigures (monitor changes) never re-read the file.
    raw = cfg.get("avatar")
    candidates: list[tuple[str, bool]] = []
    if isinstance(raw, str) and raw.strip():
        candidates.append((os.path.expanduser(raw.strip()), True))
    elif raw is not None:
        log(f"avatar: expected a string path, got {raw!r}; ignoring")
    candidates.append((os.path.expanduser("~/.face"), False))

    for path, explicit in candidates:
        try:
            with Image.open(path) as img:
                img.load()
                # Cap the working size: an avatar never needs more than ~512px,
                # and this keeps a mistakenly-configured wallpaper cheap.
                img.thumbnail((512, 512))
                if "A" in img.getbands():
                    # Flatten transparency onto the glass dark, so a cut-out
                    # avatar reads as sitting on its own dark disc.
                    bg = Image.new("RGB", img.size, (15, 18, 28))
                    bg.paste(img, mask=img.getchannel("A"))
                    flat = bg
                else:
                    flat = img.convert("RGB")
                return _pil_to_surface(flat)
        except FileNotFoundError:
            if explicit:
                log(f"avatar: {path}: not found; trying fallbacks")
        except (OSError, ValueError, Image.DecompressionBombError) as e:
            log(f"avatar: {path}: {e}; trying fallbacks")
    return None


def disc_colors(name: str) -> tuple[vs.RGBA, vs.RGBA]:
    # The initials disc's two gradient stops: a hue hashed from the name (same
    # name -> same colour across sessions, the tint_from_title trick) and a
    # neighbouring hue, both kept dark enough for white type.
    hue = int(hashlib.sha256(name.encode()).hexdigest(), 16) % 360 / 360
    r1, g1, b1 = colorsys.hls_to_rgb(hue, 0.48, 0.5)
    r2, g2, b2 = colorsys.hls_to_rgb((hue + 0.09) % 1.0, 0.36, 0.5)
    return (r1, g1, b1, 1.0), (r2, g2, b2, 1.0)


# ------------------------------------------------------------------- drawing


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


def draw_avatar_disc(
    cr: cairo.Context[cairo.ImageSurface],
    surface: cairo.ImageSurface | None,
    name: str,
    cx: float,
    cy: float,
    d: float,
    ring: vs.RGBA,
) -> None:
    # The picture cover-cropped into a circle, or the initials disc; then the
    # ring stroked on the rim. Everything derives from the diameter, so the one
    # function serves both layouts at any region size.
    radius = d / 2
    if surface is not None:
        cr.save()
        cr.arc(cx, cy, radius, 0, 2 * math.pi)
        cr.clip()
        sw, sh = surface.get_width(), surface.get_height()
        scale = d / min(sw, sh)  # cover-crop: fill the circle, clip the excess
        cr.translate(cx - sw * scale / 2, cy - sh * scale / 2)
        cr.scale(scale, scale)
        cr.set_source_surface(surface, 0, 0)
        cr.paint()
        cr.restore()
    else:
        top, bottom = disc_colors(name)
        grad = cairo.LinearGradient(cx - radius, cy - radius, cx + radius, cy + radius)
        grad.add_color_stop_rgba(0.0, *top)
        grad.add_color_stop_rgba(1.0, *bottom)
        cr.save()
        cr.arc(cx, cy, radius, 0, 2 * math.pi)
        cr.set_source(grad)
        cr.fill()
        cr.restore()
        letter = name[:1].upper() or "?"
        layout = _line_layout(cr, letter, d, d * 0.42, Pango.Weight.MEDIUM)
        _, logical = layout.get_pixel_extents()
        cr.move_to(cx - logical.width / 2, cy - logical.height / 2)
        cr.set_source_rgba(1, 1, 1, 0.95)
        PangoCairo.show_layout(cr, layout)

    if ring[3] > 0:
        cr.save()
        # new_path first: show_layout leaves a current point, and arc would
        # draw a stray line from it to the rim (save/restore keeps graphics
        # state, NOT the path -- same guard as veiland_svg.draw_pill).
        cr.new_path()
        cr.arc(cx, cy, radius, 0, 2 * math.pi)
        cr.set_line_width(max(1.0, d * 0.028))
        cr.set_source_rgba(*ring)
        cr.stroke()
        cr.restore()


def draw_greeting_pill(
    cr: cairo.Context[cairo.ImageSurface],
    text: str,
    glyph: Any,
    cx: float,
    cy: float,
    pill_h: float,
    max_w: float,
    pill: vs.RGBA,
    text_color: vs.RGBA,
) -> None:
    # The capsule centered on (cx, cy): measure the shaped line first, then
    # trace the pill around it. pill alpha 0 drops both the capsule and the
    # glyph -- bare floating text, matching the status pills' "no chip" mode.
    with_pill = pill[3] > 0
    px = pill_h * 0.44
    glyph_size = pill_h * 0.46
    pad = pill_h * 0.55
    gap = pill_h * 0.26

    inner_max = max_w - 2 * pad - (glyph_size + gap if with_pill else 0)
    layout = _line_layout(cr, text, max(inner_max, 1.0), px, Pango.Weight.NORMAL)
    _, logical = layout.get_pixel_extents()
    text_w = min(logical.width, inner_max)

    if with_pill:
        pill_w = pad + glyph_size + gap + text_w + pad
        x0 = cx - pill_w / 2
        cr.save()
        rounded_rect(cr, x0, cy - pill_h / 2, pill_w, pill_h, pill_h / 2)
        cr.set_source_rgba(*pill)
        cr.fill()
        cr.restore()
        if glyph is not None:
            tr, tg, tb, ta = text_color
            vs.draw_svg_centered(
                cr,
                glyph,
                x0 + pad + glyph_size / 2,
                cy,
                glyph_size,
                tint=(tr, tg, tb, ta * 0.75),
            )
        text_x = x0 + pad + glyph_size + gap
    else:
        text_x = cx - text_w / 2

    cr.move_to(text_x, cy - logical.height / 2)
    cr.set_source_rgba(*text_color)
    PangoCairo.show_layout(cr, layout)


@dataclass(frozen=True)
class Style:
    layout: str
    avatar: cairo.ImageSurface | None
    name: str
    glyph: Any  # Rsvg.Handle | None -- opaque, round-trips into veiland_svg
    pill: vs.RGBA
    ring: vs.RGBA
    text: vs.RGBA


def draw_into(buf: vp.LinearBuffer, style: Style, greeting: str) -> None:
    # Zero-copy: wrap buf.map()'s memoryview in a cairo surface and draw
    # straight into GPU-visible memory. The buffer IS the region (the host owns
    # WHERE it sits), so both layouts just center themselves in their own box,
    # everything a fraction of the box so one config scales across monitors.
    with buf.map() as (mem, map_stride):
        surface = cairo.ImageSurface.create_for_data(
            mem, cairo.FORMAT_ARGB32, buf.width, buf.height, map_stride
        )
        cr = cairo.Context(surface)
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.set_operator(cairo.OPERATOR_OVER)

        w, h = float(buf.width), float(buf.height)
        if style.layout == "row":
            # One wide capsule: avatar inset at the left, text beside it.
            pill_h = h * 0.92
            d = pill_h * 0.78
            inset = (pill_h - d) / 2
            px = pill_h * 0.38
            gap = pill_h * 0.24
            pad_r = pill_h * 0.50
            inner_max = w - (inset + d + gap + pad_r)
            layout = _line_layout(
                cr, greeting, max(inner_max, 1.0), px, Pango.Weight.NORMAL
            )
            _, logical = layout.get_pixel_extents()
            text_w = min(logical.width, inner_max) if greeting else 0.0
            pill_w = inset + d + (gap + text_w + pad_r if greeting else inset)
            x0 = (w - pill_w) / 2
            cy = h / 2
            if style.pill[3] > 0:
                cr.save()
                rounded_rect(cr, x0, cy - pill_h / 2, pill_w, pill_h, pill_h / 2)
                cr.set_source_rgba(*style.pill)
                cr.fill()
                cr.restore()
            draw_avatar_disc(
                cr, style.avatar, style.name, x0 + inset + d / 2, cy, d, style.ring
            )
            if greeting:
                cr.move_to(x0 + inset + d + gap, cy - logical.height / 2)
                cr.set_source_rgba(*style.text)
                PangoCairo.show_layout(cr, layout)
        else:
            # Stack: avatar over the pill, the used height centered vertically.
            d = h * 0.60
            gap = h * 0.10
            pill_h = h * 0.30
            used = d + (gap + pill_h if greeting else 0)
            y0 = (h - used) / 2
            cx = w / 2
            draw_avatar_disc(
                cr, style.avatar, style.name, cx, y0 + d / 2, d, style.ring
            )
            if greeting:
                draw_greeting_pill(
                    cr,
                    greeting,
                    style.glyph,
                    cx,
                    y0 + d + gap + pill_h / 2,
                    pill_h,
                    w - 4,
                    style.pill,
                    style.text,
                )

        surface.flush()  # commit cairo's writes before we unmap
        surface.finish()


# ----------------------------------------------------------------- main


def main() -> None:
    conn = vp.Connection.connect("avatar", "0.1.0")
    cfg = conn.wait_for_configure()

    plugin_cfg: dict[str, Any] = json.loads(
        os.environ.get("VEILAND_PLUGIN_CONFIG") or "{}"
    )
    name = resolve_name(plugin_cfg)
    template = resolve_greeting_template(plugin_cfg)
    glyph: Any = None
    try:
        glyph = vs.load_svg(ICON_PATH)
    except vs.SvgError as e:
        log(f"user.svg: {e}")  # pill renders without the glyph
    style = Style(
        layout=resolve_layout(plugin_cfg),
        avatar=load_avatar(plugin_cfg),
        name=name,
        glyph=glyph,
        pill=vs.parse_color(plugin_cfg, "pill_color", GLASS, tag="avatar"),
        ring=vs.parse_color(plugin_cfg, "ring_color", RING, tag="avatar"),
        text=vs.parse_color(plugin_cfg, "text_color", TEXT, tag="avatar"),
    )

    dev = vp.GbmDevice()
    # BufferChain even though redraws are rare (reconfigures, the "auto"
    # rollover): any redraw of a single in-place buffer races the host's live
    # sampling into a flicker, so every redrawing CPU widget keeps the chain.
    chain = vp.BufferChain(dev, cfg.region_w, cfg.region_h)

    shown = greeting_now(template, name)
    # A static widget blocks forever; only "auto" needs a tick to notice the
    # morning/afternoon/evening boundary, and a minute's lag on those is fine.
    tick = 60.0 if template == "auto" else None
    pacer = vp.FramePacer.on_demand()
    for ev in pacer.events(conn, timeout=tick):
        if ev.kind is vp.Event.RENDER:
            shown = greeting_now(template, name)
            draw_into(chain.acquire(), style, shown)
            chain.send(conn)
            pacer.submitted()
        elif ev.kind is vp.Event.RECONFIGURE and ev.configure is not None:
            # (`is not None` narrows for mypy; the SDK always sets .configure
            # on a RECONFIGURE event.)
            cfg = ev.configure
            chain = chain.resize_or_keep(dev, cfg)
            pacer.mark_dirty()
        elif ev.kind is vp.Event.TIMEOUT:
            if greeting_now(template, name) != shown:
                pacer.mark_dirty()  # the time-of-day bucket rolled over
        elif ev.kind is vp.Event.SHUTDOWN:
            break

    chain.close()
    dev.close()
    conn.close()


if __name__ == "__main__":
    main()
