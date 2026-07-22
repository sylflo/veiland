#!/usr/bin/env python3
# SPDX-License-Identifier: GPL-3.0-or-later
#
# The markup widget: ONE block of Pango markup with {variable} substitution,
# composited over the wallpaper -- the dynamic text tier, the answer to
# freeform styled labels. It is READ-ONLY, like the clock: it displays, it never
# controls, and no keystroke ever reaches it.
#
# It is ADDITIVE to veiland-label (the Rust static-text plugin), not a
# replacement. veiland-label is the lean, no-Python-needed tier (one style, a
# fixed string); markup is the rich/dynamic tier for text that CHANGES ({time})
# or MIXES styles (inline <span>). Same lean-Rust vs rich-Python split the whole
# project makes -- reach for veiland-label when a Python interpreter on the lock
# path is a cost you don't want, and markup when you want variables or markup.
#
# What it draws: the `text` config string, which may contain BOTH Pango <span>
# markup (inline sizes/weights/colors, no styling-DSL to invent) AND {variable}
# placeholders. Placeholders are substituted str.replace-style -- NEVER
# str.format: the template is untrusted config, and a stray "{" must mis-render
# at worst, never raise. Supported: {user} ($USER), {name} (GECOS full name,
# the same resolution avatar.py uses), {host} (hostname), {time:STRFTIME},
# {date:STRFTIME}. A bad strftime spec substitutes the literal placeholder and
# logs one line. Malformed <span> markup falls back to plain text + one line.
# Neither ever takes down the locker (the untrusted-input rule; see CLAUDE.md).
#
# The base font comes from veiland_text.font_from_config (markup is its first
# consumer) -- the uniform font_family/font_size/font_weight/italic keys, same
# names as the Rust label plugin. Any inline <span size=... weight=...> in the
# text OVERRIDES it for that run, which is exactly what veiland-label cannot do.
#
# Redraw discipline mirrors now_playing.py / avatar.py: FramePacer.on_demand(),
# a ~1s tick, and a display-signature check -- we mark_dirty() only when the
# SUBSTITUTED output string actually changed. So a "{time:%H:%M}" line redraws
# once a minute, a "{date:%A %d %B}" line once a day, and a fully static line
# never redraws after the first frame. Draw is zero-copy (buf.map() -> cairo ->
# PangoCairo) through a BufferChain, which every redrawing CPU widget needs to
# avoid the in-place-buffer flicker race.
#
# A command-runner tier (a line whose text is a shell command's output, re-run
# every N seconds) is deliberately DEFERRED to a later version -- variables cover
# the common cases without spawning subprocesses.
#
# This needs the gi stack: pygobject3 + Pango/PangoCairo typelibs (the flake's
# dev shell wires them). A real plugin vendors veiland_plugin.py (and
# veiland_text.py) next to itself; this example adds the repo's python/ dir to
# sys.path so it runs from the tree.

from __future__ import annotations

import os
import sys
from dataclasses import dataclass, replace
from typing import Any

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))

# These follow the sys.path shim so the SDK imports resolve (E402). gi version
# pins come before gi.repository; cairo is imported before PangoCairo can render
# onto a cairo.Context.
import json  # noqa: E402
import pwd  # noqa: E402
import re  # noqa: E402
import socket  # noqa: E402
import time  # noqa: E402

import gi  # noqa: E402

gi.require_version("Pango", "1.0")  # noqa: E402
gi.require_version("PangoCairo", "1.0")  # noqa: E402

import cairo  # noqa: E402
from gi.repository import Pango, PangoCairo  # noqa: E402

import veiland_layout as vl  # noqa: E402
import veiland_plugin as vp  # noqa: E402
import veiland_svg as vs  # noqa: E402
import veiland_text as vt  # noqa: E402

# Near-white text over the wallpaper, and a soft dark drop shadow so the block
# stays legible on a light background. Both RGBA 0..1 where alpha IS the opacity,
# overridable per config (text_color / shadow_color) via veiland_svg.parse_color.
TEXT = (1.0, 1.0, 1.0, 0.96)
SHADOW = (0.0, 0.0, 0.0, 0.45)

# The base font size, as a fraction of the region HEIGHT. font_from_config's own
# default (0.030) is calibrated for the Rust label's fraction-of-SURFACE; a
# markup region is a short anchored box, so the same fraction there is tiny. This
# larger default fills a time+date block sensibly; an inline <span size=...>
# scales relative to it, and the user overrides the whole base via font_size.
DEFAULT_SIZE = 0.20

# A time+date block is the canonical demo: a big time over a smaller date, the
# clock look everyone recognises, done here as ONE markup string instead of the
# clock plugin's two fixed lines -- proof the dynamic tier subsumes it.
DEFAULT_TEXT = (
    '<span size="xx-large" weight="bold">{time:%H:%M}</span>\n{date:%A %d %B}'
)

# {name:...}-style placeholders: a bare key, or key + ":" + a strftime-ish
# format that runs to the closing brace. Non-greedy so "{a}{b}" splits in two,
# and "[^{}]" in the format so a stray unmatched "{" can't swallow the rest.
_PLACEHOLDER = re.compile(r"\{(\w+)(?::([^{}]*))?\}")


def log(msg: str) -> None:
    print(f"markup: {msg}", file=sys.stderr, flush=True)


# ------------------------------------------------------------- config reading


def resolve_name() -> str:
    # GECOS full name -> $USER -> "there". The GECOS field ("Sylvain Chateau,,,")
    # is where a full name already lives on most systems; only the part before
    # the first comma is the name proper. Same resolution as avatar.py, minus its
    # config override -- here the name is a {variable}, styled by the template.
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


def resolve_text(cfg: dict[str, Any]) -> str:
    raw = cfg.get("text", DEFAULT_TEXT)
    if not isinstance(raw, str):
        log(f"text: expected a string, got {raw!r}; using the default block")
        return DEFAULT_TEXT
    return raw


def resolve_font(cfg: dict[str, Any]) -> vt.FontSpec:
    # The uniform font_family/font_size/font_weight/italic keys, but with THIS
    # widget's larger size default: font_from_config bakes in 0.030
    # (fraction-of-surface, tiny in a short region box), so when the user leaves
    # font_size out we swap in DEFAULT_SIZE. An explicit font_size is honoured
    # untouched -- font_from_config already validated it.
    font = vt.font_from_config(cfg, tag="markup")
    if "font_size" not in cfg:
        font = replace(font, size=DEFAULT_SIZE)
    return font


# ------------------------------------------------------- variable substitution


def substitute(template: str, name: str) -> str:
    # Replace every {key} / {key:fmt} placeholder in the (untrusted) template.
    # str.replace-style via re.sub, NEVER str.format: a literal "{" that isn't a
    # known placeholder is left untouched rather than raising KeyError/ValueError.
    # {time}/{date} take an optional strftime format; a bad format substitutes the
    # LITERAL placeholder text (so the user sees what they typed and can fix it)
    # and logs one line -- it never crashes the widget.
    now = time.localtime()

    def one(m: re.Match[str]) -> str:
        key = m.group(1)
        fmt = m.group(2)
        if key == "user":
            return os.environ.get("USER") or "there"
        if key == "name":
            return name
        if key == "host":
            return socket.gethostname()
        if key in ("time", "date"):
            spec = fmt if fmt is not None else ("%H:%M" if key == "time" else "%x")
            try:
                return time.strftime(spec, now)
            except (ValueError, TypeError) as e:
                log(f"{{{key}:{spec}}}: bad strftime spec ({e}); left as-is")
                return m.group(0)  # the literal "{time:...}" -- visible, fixable
        # An unknown {key}: leave it verbatim. It might be a literal brace the
        # user wants shown, or a typo -- either way, mis-render, never raise.
        return m.group(0)

    return _PLACEHOLDER.sub(one, template)


# ------------------------------------------------------------------- drawing


def _font_description(font: vt.FontSpec, px: float) -> Pango.FontDescription:
    # The base font for the whole block, from the uniform font_from_config keys.
    # px is the resolved pixel size (font.size is a fraction of the box; the
    # caller multiplies). set_absolute_size takes Pango units (px * SCALE). An
    # inline <span size=...> in the markup overrides this for its own run.
    desc = Pango.FontDescription()
    desc.set_family(font.family)
    desc.set_absolute_size(px * Pango.SCALE)
    desc.set_weight(font.weight)
    if font.italic:
        desc.set_style(Pango.Style.ITALIC)
    return desc


def build_layout(
    cr: cairo.Context[cairo.ImageSurface],
    markup: str,
    font: vt.FontSpec,
    px: float,
    halign: str,
    max_w: float,
) -> Pango.Layout:
    # One multi-line Pango layout for the whole block. set_markup parses the
    # <span> tags; if the markup is malformed we fall back to set_text (the raw
    # string shown as plain text) + one log line, so a broken tag mis-renders,
    # never crashes (untrusted-input rule). Pango.parse_markup is the pre-check
    # -- it raises GLib.GError on bad markup, which set_markup would swallow into
    # an unhelpful state, so we gate on it and choose the path explicitly.
    layout = PangoCairo.create_layout(cr)
    layout.set_font_description(_font_description(font, px))
    layout.set_width(int(max_w * Pango.SCALE))
    layout.set_wrap(Pango.WrapMode.WORD_CHAR)
    layout.set_alignment(
        {
            "left": Pango.Alignment.LEFT,
            "center": Pango.Alignment.CENTER,
            "right": Pango.Alignment.RIGHT,
        }[halign]
    )
    try:
        # parse_markup returns (ok, attrs, text, accel); on bad markup it raises
        # GLib.GError. We only care that it PARSES -- set_markup re-parses, but
        # this pre-check lets us branch to plain text cleanly on failure.
        Pango.parse_markup(markup, -1, "\0")
        layout.set_markup(markup, -1)
    except Exception as e:  # GLib.GError (untyped via gi) and any parse fault
        log(f"markup parse failed ({e}); rendering as plain text")
        layout.set_text(markup, -1)
    return layout


@dataclass(frozen=True)
class Style:
    template: str
    name: str
    font: vt.FontSpec
    halign: str  # content_halign: left|center|right -- placement + line justify
    valign: str  # content_valign: top|center|bottom -- vertical placement
    text: vs.RGBA
    shadow: vs.RGBA
    border_on: bool  # debug_border: stroke the region-box edge to see placement
    border_color: vs.RGBA


def draw_into(buf: vp.LinearBuffer, style: Style, shown: str) -> None:
    # Zero-copy: wrap buf.map()'s memoryview in a cairo surface and draw straight
    # into GPU-visible memory. The buffer IS the region (the host owns WHERE it
    # sits); we place the laid-out block within our own box via the shared
    # content-anchor convention (veiland_layout), so content_halign="left" parks
    # the text flush at the box's left edge instead of always centering it.
    with buf.map() as (mem, map_stride):
        surface = cairo.ImageSurface.create_for_data(
            mem, cairo.FORMAT_ARGB32, buf.width, buf.height, map_stride
        )
        cr = cairo.Context(surface)
        cr.set_operator(cairo.OPERATOR_CLEAR)
        cr.paint()
        cr.set_operator(cairo.OPERATOR_OVER)

        w, h = float(buf.width), float(buf.height)
        px = style.font.size * h  # font_size is a fraction of the box height
        # Lay the block out at the full region width so multi-line justification
        # (tied to content_halign -- the settled one-key-does-both decision) has
        # room to work, then measure the block's own extent and let the ANCHOR,
        # not a hardcoded x=0, own where that block sits in the box. anchor_offset
        # returns (0, 0) when the block already fills the box, so a full-width
        # block never shifts -- the feature only moves what has slack.
        layout = build_layout(cr, shown, style.font, px, style.halign, w)
        _, logical = layout.get_pixel_extents()
        block_w, block_h = float(logical.width), float(logical.height)
        x, y = vl.anchor_offset(style.halign, style.valign, w, h, block_w, block_h)
        # anchor_offset places the block's LEFT edge at x. But Pango, handed the
        # full region width plus a CENTER/RIGHT alignment, has ALREADY shifted the
        # glyphs right by logical.x to justify them in that width. show_layout
        # draws the origin at logical.x, so subtract it back out: the anchor alone
        # then owns box placement, while Pango's alignment still justifies the
        # LINES relative to each other within the block. For left/top logical.x is
        # 0 and this is a no-op -- without it, center/right double-shift off-box.
        draw_x = x - float(logical.x)

        # Drop shadow first (offset a hair down-right), then the text over it.
        # alpha 0 on the shadow color drops it -- bare text, no shadow.
        if style.shadow[3] > 0.0:
            off = max(1.0, px * 0.04)
            cr.move_to(draw_x + off, y + off)
            cr.set_source_rgba(*style.shadow)
            PangoCairo.show_layout(cr, layout)
        cr.move_to(draw_x, y)
        cr.set_source_rgba(*style.text)
        PangoCairo.show_layout(cr, layout)

        # Debug border LAST, over the content: a 1px rectangle inset half a pixel
        # from the buffer edge so the whole stroke lands inside the box on the
        # pixel grid. Because the host sizes the buffer 1:1 with the region, this
        # traces the (otherwise invisible) region box, making anchor tuning
        # visible. Off unless debug_border = true (untrusted-input rule).
        if style.border_on:
            cr.set_source_rgba(*style.border_color)
            cr.set_line_width(1.0)
            cr.rectangle(0.5, 0.5, w - 1.0, h - 1.0)
            cr.stroke()

        surface.flush()  # commit cairo's writes before we unmap
        surface.finish()


# ----------------------------------------------------------------- main


def main() -> None:
    conn = vp.Connection.connect("markup", "0.1.0")
    cfg = conn.wait_for_configure()

    plugin_cfg: dict[str, Any] = json.loads(
        os.environ.get("VEILAND_PLUGIN_CONFIG") or "{}"
    )
    content_halign, content_valign = vl.anchor_from_config(plugin_cfg, tag="markup")
    border_on, border_color = vl.debug_border_from_config(plugin_cfg, tag="markup")
    style = Style(
        template=resolve_text(plugin_cfg),
        name=resolve_name(),
        font=resolve_font(plugin_cfg),
        halign=content_halign,
        valign=content_valign,
        text=vs.parse_color(plugin_cfg, "text_color", TEXT, tag="markup"),
        shadow=vs.parse_color(plugin_cfg, "shadow_color", SHADOW, tag="markup"),
        border_on=border_on,
        border_color=border_color,
    )

    dev = vp.GbmDevice()
    # BufferChain, not one LinearBuffer: a {time} line redraws, and a CPU plugin
    # redrawing a single buffer in place races the host's live zero-copy sampling
    # into a flicker. The chain hands out the buffer the host is NOT showing.
    chain = vp.BufferChain(dev, cfg.region_w, cfg.region_h)

    shown = substitute(style.template, style.name)
    # A 1s tick catches the finest displayed granularity (a {time:%H:%M:%S} line
    # or the %M rollover). We only mark_dirty when the SUBSTITUTED string changed,
    # so a minute-resolution line still redraws just once a minute and a static
    # line never redraws after the first frame. A purely static template (no
    # placeholders resolving to a clock) idles with no tick at all.
    tick = 1.0 if _PLACEHOLDER.search(style.template) else None
    pacer = vp.FramePacer.on_demand()
    for ev in pacer.events(conn, timeout=tick):
        if ev.kind is vp.Event.RENDER:
            shown = substitute(style.template, style.name)
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
            if substitute(style.template, style.name) != shown:
                pacer.mark_dirty()  # a {time}/{date} field rolled over
        elif ev.kind is vp.Event.SHUTDOWN:
            break

    chain.close()
    dev.close()
    conn.close()


if __name__ == "__main__":
    main()
