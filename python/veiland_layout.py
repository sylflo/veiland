# SPDX-License-Identifier: GPL-3.0-or-later
#
# Optional layout companion for the veiland Python plugin SDK: a 9-point content
# anchor, the two config parsers that feed it, and the debug-border draw helper.
# WHERE a widget draws its content inside the buffer it was handed is a
# presentation concern on a different axis from transport/buffer/pacing, so it
# lives in its own companion rather than growing veiland_plugin.py (~1150 lines)
# with unrelated surface. It joins the companion set (veiland_svg / veiland_dbus /
# veiland_text / veiland_layout): each a single-purpose file a widget vendors when
# it wants that capability.
#
# It imports cairo for the one draw helper (draw_debug_border) -- cheap, since a
# widget compositing into a mapped buffer already builds a cairo context, so
# cairo is present by construction. The anchor/parser half is pure float math and
# dict reading and would run without cairo, but keeping the border DRAW next to
# its config PARSE (debug_border_from_config) is the cohesive place for it: the
# whole feature in one file, reachable with the one import every widget here
# already makes.
#
# It is a CONVENTION, not a placement system the SDK imposes -- the exact status
# font_from_config / parse_color already have (a carrot, not a fence). Region
# placement (WHERE the buffer sits on the lock surface) is CORE and a trust
# boundary the host enforces; this is the other, plugin-owned half: once the core
# hands over a W x H buffer, where the plugin draws inside it is invisible to the
# core. The core never sees or enforces it. These helpers just hand a widget one
# agreed meaning for content_halign / content_valign so a widget that WANTS
# cross-widget consistency gets it for free; a widget that ignores them and
# hardcodes its own placement is entirely within its rights and nothing breaks.
#
# anchor_offset does the math (measure your block, get back a top-left);
# anchor_from_config / debug_border_from_config read the keys the way parse_color
# reads colors -- one parser each, never a crash on a bad value; draw_debug_border
# strokes the region-box outline the parser enables.

from __future__ import annotations

import sys
from collections.abc import Mapping
from typing import Any

import cairo

__all__ = [
    "RGBA",
    "anchor_from_config",
    "anchor_offset",
    "debug_border_from_config",
    "draw_debug_border",
]

# An (r, g, b, a) color, 0..1 floats, alpha IS the opacity -- the same shape
# veiland_svg.RGBA carries, redeclared here so a widget that vendors only
# veiland_layout (not the SVG companion) still has the alias. tuple[...] not | so
# the alias evaluates on the SDK's 3.9 floor.
RGBA = tuple[float, float, float, float]

# The 9-point anchor's vocabulary. content_halign / content_valign each name one
# axis; the first entry is the default (center) so an absent key is byte-for-byte
# today's behavior for every widget.
_HALIGN = ("center", "left", "right")
_VALIGN = ("center", "top", "bottom")

# A bright magenta the plan settled on for the debug border: loud enough that it
# never blends with typical white-on-dark widget content, unmistakably an overlay
# and not decoration. Opaque.
DEFAULT_BORDER_COLOR: RGBA = (1.0, 0.0, 1.0, 1.0)


def _resolve_align(
    cfg: Mapping[str, Any], key: str, allowed: tuple[str, ...], tag: str
) -> str:
    # One axis of the anchor: read `key` from the config, accept it only if it is
    # one of `allowed`, else fall back to allowed[0] (the default) plus one stderr
    # line. A mis-typed alignment mis-places the block, it never crashes the
    # widget -- the untrusted-input rule, exactly as parse_color does for colors.
    raw = cfg.get(key, allowed[0])
    if raw in allowed:
        return str(raw)
    else:
        print(
            f"{tag}: {key}: expected one of {allowed}, got {raw!r}; "
            f"using {allowed[0]!r}",
            file=sys.stderr,
        )
        return allowed[0]


def anchor_from_config(cfg: Mapping[str, Any], tag: str = "veiland") -> tuple[str, str]:
    """Read (content_halign, content_valign) from a plugin-config dict:

      content_halign = "left" | "center" | "right"   (default "center")
      content_valign = "top"  | "center" | "bottom"  (default "center")

    Both keys are optional; each bad/unknown value logs one stderr line tagged
    with the plugin name and falls back to center (the untrusted-input rule).
    Absent -> ("center", "center"), which is byte-identical to what every widget
    drew before it read these keys. Feed the result to anchor_offset.

    This is a shared OPT-IN convention (like font_family), not a core-interpreted
    key: the widget reads and honors it, veiland-core never sees it. content_*
    names, not bare halign/valign, keep it distinct from the region's own
    halign/valign (where the BOX goes vs. where the STUFF goes inside the box)."""
    return (
        _resolve_align(cfg, "content_halign", _HALIGN, tag),
        _resolve_align(cfg, "content_valign", _VALIGN, tag),
    )


def anchor_offset(
    halign: str,
    valign: str,
    region_w: float,
    region_h: float,
    block_w: float,
    block_h: float,
) -> tuple[float, float]:
    """Return the (x, y) top-left at which to draw a block_w x block_h content
    block so it sits at the requested anchor within a region_w x region_h box.

    Pure math, no drawing -- the widget measures its own block (Pango extents, an
    icon's bounding box, a card size), calls this, and draws at the returned
    (x, y) in its own cairo/whatever:

        x = 0                       left
            (region_w - block_w)/2  center
            region_w - block_w      right
        y = 0                       top
            (region_h - block_h)/2  center
            region_h - block_h      bottom

    When the block is as large as the region every anchor returns (0, 0): the
    feature moves only what has room to move (a full-width pill or a compact card
    that fills its box doesn't shift), which is the intended no-op. An unknown
    align string falls back to center rather than raising -- this function may be
    called with values that did not pass through anchor_from_config, and plugin
    config is untrusted, so it must never KeyError."""
    if halign == "left":
        x = 0.0
    elif halign == "right":
        x = region_w - block_w
    else:  # "center" and any unexpected value
        x = (region_w - block_w) / 2.0

    if valign == "top":
        y = 0.0
    elif valign == "bottom":
        y = region_h - block_h
    else:  # "center" and any unexpected value
        y = (region_h - block_h) / 2.0

    return (x, y)


def debug_border_from_config(
    cfg: Mapping[str, Any], tag: str = "veiland"
) -> tuple[bool, RGBA]:
    """Read (debug_border, debug_border_color) from a plugin-config dict:

      debug_border       = true                  (default false)
      debug_border_color = [r, g, b, a] in 0..1  (default bright magenta)

    Returns (enabled, rgba). When enabled, the widget strokes a 1px rectangle
    just inside its own buffer edge -- because the host sizes the buffer 1:1 with
    the region, that rectangle exactly traces the (otherwise invisible) region
    box, turning content-anchor tuning from guesswork into something you can see.
    The DRAWING is one cr.rectangle + cr.stroke the widget does; this parser
    stays drawing-free (so a text widget needn't pull in a graphics stack to read
    the keys).

    A shared OPT-IN debug key, same convention story as content_halign: absent ->
    (False, magenta), so a normal locked session pays nothing. A malformed color
    falls back to the default magenta plus one stderr line (the untrusted-input
    rule) -- a bad debug line never crashes the widget."""
    enabled = bool(cfg.get("debug_border", False))
    color = _parse_rgba(cfg, "debug_border_color", DEFAULT_BORDER_COLOR, tag)
    return (enabled, color)


def draw_debug_border(
    cr: cairo.Context[Any], width: float, height: float, rgba: RGBA
) -> None:
    """Stroke a 1px rectangle just inside the (0, 0, width, height) buffer edge in
    rgba -- the debug_border_from_config half that does the DRAWING. Because the
    host sizes the buffer 1:1 with the region, this rectangle exactly traces the
    (otherwise invisible) region box, so a widget can see where the host placed it
    and where its content sits. Call it LAST, over the content, inside your
    already-built cairo context:

        border_on, border_color = vl.debug_border_from_config(cfg, tag="my-widget")
        ...
        if border_on:
            vl.draw_debug_border(cr, w, h, border_color)

    The 0.5px inset lands the whole 1px stroke inside the box on the pixel grid so
    it stays crisp. Saves/restores the source and line width via a fresh path but
    leaves the caller's state otherwise untouched (it does not save/restore the
    matrix -- pass buffer-space width/height, which every caller already has)."""
    cr.set_source_rgba(*rgba)
    cr.set_line_width(1.0)
    cr.rectangle(0.5, 0.5, width - 1.0, height - 1.0)
    cr.stroke()


def _parse_rgba(cfg: Mapping[str, Any], key: str, default: RGBA, tag: str) -> RGBA:
    # The same [r, g, b, a]-in-0..1 clamp veiland_svg.parse_color does, inlined
    # here so this file needs no import from the SVG companion (which would drag in
    # the gi/librsvg stack). Absent -> default; malformed -> default plus one
    # stderr line. Kept private: debug_border_color is the only color this
    # companion reads, and parse_color remains the public parser.
    raw = cfg.get(key)
    if raw is None:
        return default
    try:
        r, g, b, a = (min(1.0, max(0.0, float(v))) for v in raw)
    except (TypeError, ValueError):
        print(
            f"{tag}: {key}: expected [r, g, b, a] numbers in 0..1, "
            f"got {raw!r}; using default",
            file=sys.stderr,
        )
        return default
    return (r, g, b, a)
