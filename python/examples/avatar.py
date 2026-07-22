#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The avatar widget: JUST the round user disc -- a picture cover-cropped into a
# circle, or a tinted initials disc, with a thin rim -- the "this is MY
# lockscreen" ingredient. Pure config: no D-Bus, no network, no polling. It draws
# once at Configure and then idles (static plugins idling is legal).
#
# It is disc-ONLY by design. The greeting that used to live here is now a markup
# region (python/examples/markup.py, which gained an optional background chip):
# one region holds the disc, one holds the greeting, each its own region + content
# anchor -- the project's own "compose plugins" thesis. docs/examples/avatar.toml
# wires both blocks into the display-manager look out of the box. This split is a
# BEHAVIOR CHANGE from the earlier combined widget (which stacked a greeting pill
# under the disc); avatar was unreleased, so that is acceptable.
#
# Zero-config personalisation, each step falling back to the next:
#   name:   [plugin.config] name -> the GECOS full name in /etc/passwd -> $USER
#           (the name only seeds the initials + the disc's hashed tint now)
#   avatar: [plugin.config] avatar -> ~/.face (the display-manager convention)
#           -> a tinted initials disc (hue hashed from the name, the same
#           stable-tint trick now_playing.py uses for coverless tracks)
#
# The disc fills the shorter side of its region and is placed by the shared
# content anchor (content_halign/content_valign via veiland_layout), so a
# square-ish region shows a full centered disc and the 9 anchor points move it
# cleanly. The initials letter uses PangoCairo (real shaping), so this needs the
# gi stack: pygobject3 + Pango/PangoCairo typelibs (the flake's dev shell wires
# them). Image decode is PIL, as now_playing does for covers.
#
# A real plugin vendors veiland_plugin.py (and veiland_text.py / veiland_layout.py)
# next to itself. This example adds the repo's python/ dir to sys.path so it runs
# from the tree.

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

import gi  # noqa: E402

gi.require_version("Pango", "1.0")  # noqa: E402
gi.require_version("PangoCairo", "1.0")  # noqa: E402

import cairo  # noqa: E402
from gi.repository import Pango, PangoCairo  # noqa: E402
from PIL import Image  # noqa: E402

import veiland_layout as vl  # noqa: E402
import veiland_plugin as vp  # noqa: E402
import veiland_svg as vs  # noqa: E402
import veiland_text as vt  # noqa: E402

# The shaped single-line layout builder lives in the text companion (shared with
# now_playing.py). Alias it to the old private name so the initials draw reads
# unchanged.
_line_layout = vt.line_layout

# A thin translucent ring on the disc rim, RGBA 0..1 where alpha IS the opacity,
# overridable per config (ring_color) via veiland_svg.parse_color. alpha 0 -> no
# ring.
RING = (1.0, 1.0, 1.0, 0.22)


def log(msg: str) -> None:
    print(f"avatar: {msg}", file=sys.stderr)


# ------------------------------------------------------------- config reading


def resolve_name(cfg: dict[str, Any]) -> str:
    # name -> GECOS full name -> $USER. The GECOS field ("Sylvain Chateau,,,")
    # is where a full name already lives on most systems, so an empty
    # [plugin.config] still seeds the initials + tint by name; only the part
    # before the first comma is the name proper.
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


def draw_avatar_disc(
    cr: cairo.Context[cairo.ImageSurface],
    surface: cairo.ImageSurface | None,
    name: str,
    cx: float,
    cy: float,
    d: float,
    ring: vs.RGBA,
    font: vt.FontSpec,
) -> None:
    # The picture cover-cropped into a circle, or the initials disc; then the
    # ring stroked on the rim. Everything derives from the diameter, so the disc
    # renders at any region size.
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
        layout = _line_layout(cr, letter, d, d * 0.42, Pango.Weight.MEDIUM, font)
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


@dataclass(frozen=True)
class Style:
    avatar: cairo.ImageSurface | None
    name: str
    font: vt.FontSpec  # family/italic for the initials letter (font_from_config)
    ring: vs.RGBA
    halign: str  # content_halign: anchor the d x d disc within its region box
    valign: str  # content_valign
    border_on: bool  # debug_border: stroke the region-box edge to see placement
    border_color: vs.RGBA


def draw_into(buf: vp.LinearBuffer, style: Style) -> None:
    # Zero-copy: wrap buf.map()'s memoryview in a cairo surface and draw
    # straight into GPU-visible memory. The buffer IS the region (the host owns
    # WHERE it sits). The disc is a d x d square block that fills the shorter
    # side; the shared content anchor (veiland_layout) places that block within
    # the box, so content_halign/valign move the disc and the default
    # (center, center) sits it dead-centre.
    with buf.map() as (mem, map_stride):
        surface = cairo.ImageSurface.create_for_data(
            mem, cairo.FORMAT_ARGB32, buf.width, buf.height, map_stride
        )
        cr = cairo.Context(surface)
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.set_operator(cairo.OPERATOR_OVER)

        w, h = float(buf.width), float(buf.height)
        d = min(w, h)  # the disc fills the shorter side of the region
        x, y = vl.anchor_offset(style.halign, style.valign, w, h, d, d)
        draw_avatar_disc(
            cr,
            style.avatar,
            style.name,
            x + d / 2,
            y + d / 2,
            d,
            style.ring,
            style.font,
        )

        # Debug border: trace the region box (= buffer edge) when debug_border is
        # set, so the (invisible) box avatar was handed is visible. Off by default
        # (untrusted-input rule).
        if style.border_on:
            vl.draw_debug_border(cr, w, h, style.border_color)

        surface.flush()  # commit cairo's writes before we unmap
        surface.finish()


# ----------------------------------------------------------------- main


def main() -> None:
    conn = vp.Connection.connect("avatar", "0.1.0")
    cfg = conn.wait_for_configure()

    plugin_cfg: dict[str, Any] = json.loads(
        os.environ.get("VEILAND_PLUGIN_CONFIG") or "{}"
    )
    content_halign, content_valign = vl.anchor_from_config(plugin_cfg, tag="avatar")
    border_on, border_color = vl.debug_border_from_config(plugin_cfg, tag="avatar")
    style = Style(
        avatar=load_avatar(plugin_cfg),
        name=resolve_name(plugin_cfg),
        # font_family + italic theme the initials letter; font_size stays
        # geometry-derived here (the letter is sized from the diameter), so only
        # the family/italic fields of this FontSpec are consulted.
        font=vt.font_from_config(plugin_cfg, tag="avatar"),
        ring=vs.parse_color(plugin_cfg, "ring_color", RING, tag="avatar"),
        halign=content_halign,
        valign=content_valign,
        border_on=border_on,
        border_color=border_color,
    )

    dev = vp.GbmDevice()
    # BufferChain even though the disc is static: any redraw of a single in-place
    # buffer (a reconfigure on a monitor change) races the host's live sampling
    # into a flicker, so every redrawing CPU widget keeps the chain.
    chain = vp.BufferChain(dev, cfg.region_w, cfg.region_h)

    pacer = vp.FramePacer.on_demand()
    for ev in pacer.events(conn):
        if ev.kind is vp.Event.RENDER:
            draw_into(chain.acquire(), style)
            chain.send(conn)
            pacer.submitted()
        elif ev.kind is vp.Event.RECONFIGURE and ev.configure is not None:
            # (`is not None` narrows for mypy; the SDK always sets .configure
            # on a RECONFIGURE event.)
            cfg = ev.configure
            chain = chain.resize_or_keep(dev, cfg)
            pacer.mark_dirty()
        elif ev.kind is vp.Event.SHUTDOWN:
            break

    chain.close()
    dev.close()
    conn.close()


if __name__ == "__main__":
    main()
