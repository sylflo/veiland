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

from __future__ import annotations

import math
import os

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


def draw_svg(cr, handle, x, y, w, h):
    """Render handle scaled to fit the (x, y, w, h) box on cairo context cr.

    Uses render_document (librsvg >= 2.46): librsvg fits the document's own
    viewBox into the viewport rectangle preserving aspect ratio (the SVG's
    default preserveAspectRatio="xMidYMid meet"), so a square-viewBox icon in a
    square box fills it centered, and a non-square box letterboxes rather than
    distorts. Saves/restores cr so nothing leaks into later drawing."""
    cr.save()
    viewport = Rsvg.Rectangle()
    viewport.x = x
    viewport.y = y
    viewport.width = w
    viewport.height = h
    handle.render_document(cr, viewport)
    cr.restore()


def draw_svg_centered(cr, handle, cx, cy, size):
    """Draw handle as a size x size square centered on (cx, cy). The common
    case for a status glyph: you have a center and a target size, not a
    top-left box. Thin wrapper over draw_svg."""
    half = size / 2.0
    draw_svg(cr, handle, cx - half, cy - half, size, size)


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
