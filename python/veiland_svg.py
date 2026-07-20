# SPDX-License-Identifier: GPL-3.0-or-later
#
# Optional SVG companion for the veiland Python plugin SDK. Loads an SVG
# through librsvg (gi.repository.Rsvg) and renders it straight onto a cairo
# context -- the same context an author already built over buf.map(), so an
# icon composites into GPU-visible memory with no extra copy.
#
# This is a SEPARATE, opt-in file, NOT part of veiland_plugin.py. The SDK
# stays the single vendorable stdlib+ctypes file; SVG carries a librsvg dep,
# so authors who want it vendor this second file alongside the SDK. It imports
# gi/librsvg at module load, and needs the Rsvg typelib on GI_TYPELIB_PATH plus
# librsvg's .so dlopen-able (the flake's dev shell wires both; see flake.nix).
#
# Status icons are static images swapped by state: an if/else picks a file,
# draw_svg blits it. See python/examples/battery_svg.py for the worked pattern.
# parse_color turns a [plugin.config] RGBA list into a drawable color, so the
# pill/tint colors are one config line for the user and one call for the author.

from __future__ import annotations

import math
import os
import sys

import cairo
import gi

gi.require_version("Rsvg", "2.0")
from gi.repository import GLib, Rsvg  # noqa: E402  (after gi.require_version)

__all__ = [
    "SvgError",
    "SvgLoadError",
    "load_svg",
    "draw_svg",
    "draw_svg_centered",
    "draw_pill",
    "parse_color",
]


class SvgError(Exception):
    """Base for every failure in the optional SVG companion.

    Like the SDK's own faults (veiland_plugin.GbmError), this is a clean typed
    exception, never a raw GLib.GError leaking out of librsvg -- the plugin
    author decides how to react (skip the icon, draw a fallback, or exit)."""


class SvgLoadError(SvgError):
    """An SVG could not be opened or parsed (missing path, bad XML)."""


# One handle per file, keyed by resolved path: an icon set is loaded once at
# startup and drawn many times, so parsing is not repeated per frame.
_HANDLE_CACHE: dict[str, Rsvg.Handle] = {}


def load_svg(path: str) -> Rsvg.Handle:
    """Load (and cache) an SVG, returning an opaque Rsvg.Handle to pass to
    draw_svg. Raises SvgLoadError on a missing file or parse failure -- never
    a bare GLib.GError. Cached by resolved path, so repeat calls are cheap."""
    key = os.path.realpath(path)
    cached = _HANDLE_CACHE.get(key)
    if cached is not None:
        return cached
    try:
        # new_from_file is doc-deprecated in favour of new_from_gfile_sync, but
        # it emits no runtime PyGIDeprecationWarning and needs no Gio import --
        # the heavier gfile form buys nothing for a plain local path. Keep this.
        handle = Rsvg.Handle.new_from_file(key)
    except GLib.GError as e:
        raise SvgLoadError(f"could not load SVG {path!r}: {e.message}") from e
    if handle is None:
        raise SvgLoadError(f"could not load SVG {path!r}: null handle")
    _HANDLE_CACHE[key] = handle
    return handle


def draw_svg(cr, handle, x, y, w, h, tint=None):
    """Render handle scaled to fit the (x, y, w, h) box on cairo context cr.

    Uses render_document (librsvg >= 2.46): librsvg fits the document's own
    viewBox into the viewport rectangle preserving aspect ratio (the SVG's
    default preserveAspectRatio="xMidYMid meet"), so a square-viewBox icon in a
    square box fills it centered, and a non-square box letterboxes rather than
    distorts. Saves/restores cr so nothing leaks into later drawing.

    tint, if given, is an (r, g, b, a) tuple of 0..1 floats: the glyph is
    painted in that one color through its own coverage, discarding the SVG's
    baked-in fill/stroke colors. Per-path opacity (a dimmed "off" state)
    survives, multiplied with the tint's alpha. Meant for monochrome status
    glyphs; a multi-color SVG flattens to the tint."""
    if tint is None:
        cr.save()
        viewport = Rsvg.Rectangle()
        viewport.x = x
        viewport.y = y
        viewport.width = w
        viewport.height = h
        handle.render_document(cr, viewport)
        cr.restore()
        return
    # Recolor: render the glyph to a scratch surface, then paint the tint
    # through the scratch's alpha channel (cairo mask). One small allocation
    # per call; status pills redraw on state changes, not per frame.
    scratch = cairo.ImageSurface(
        cairo.FORMAT_ARGB32, max(1, math.ceil(w)), max(1, math.ceil(h))
    )
    scr = cairo.Context(scratch)
    viewport = Rsvg.Rectangle()
    viewport.x = 0
    viewport.y = 0
    viewport.width = w
    viewport.height = h
    handle.render_document(scr, viewport)
    scratch.flush()
    r, g, b, a = tint
    cr.save()
    cr.set_source_rgba(r, g, b, a)
    cr.mask_surface(scratch, x, y)
    cr.restore()


def draw_svg_centered(cr, handle, cx, cy, size, tint=None):
    """Draw handle as a size x size square centered on (cx, cy). The common
    case for a status glyph: you have a center and a target size, not a
    top-left box. Thin wrapper over draw_svg; tint passes through."""
    half = size / 2.0
    draw_svg(cr, handle, cx - half, cy - half, size, size, tint=tint)


def draw_pill(cr, cx, cy, radius, rgba):
    """Fill a circular pill of radius centered on (cx, cy) with an (r, g, b, a)
    tuple of 0..1 floats -- the translucent chip a status glyph sits on. Pure
    cairo (no SVG), kept here so a status widget composes pill + icon from two
    calls. Saves/restores cr; leaves the current path clean."""
    r, g, b, a = rgba
    cr.save()
    cr.new_path()
    cr.arc(cx, cy, radius, 0.0, 2.0 * math.pi)
    cr.set_source_rgba(r, g, b, a)
    cr.fill()
    cr.restore()


def parse_color(cfg, key, default, tag="veiland-svg"):
    """Read an RGBA color from a plugin-config dict: [r, g, b, a] numbers in
    0..1 (the same form the Rust plugins' color fields take -- alpha IS the
    opacity, there is no separate knob), clamped per channel. Key absent ->
    default, returned untouched (so a None default can mean "feature off").
    Malformed -> default plus one stderr line tagged with the plugin name: a
    bad config line mis-themes the widget, it never crashes it."""
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
