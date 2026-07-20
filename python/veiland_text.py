# SPDX-License-Identifier: GPL-3.0-or-later
#
# Optional text companion for the veiland Python plugin SDK. The shaped,
# end-ellipsized single-line layout every text widget needs, plus the config
# parser for the uniform font_family/font_size keys -- the same helpers that
# were copy-pasted between now_playing.py and avatar.py, promoted to one file.
# It is the veiland_svg.py / veiland_dbus.py move again: extract the boilerplate
# once it repeats, not before.
#
# This is a SEPARATE, opt-in file, NOT part of veiland_plugin.py. The SDK stays
# the single vendorable stdlib+ctypes file; text carries a Pango/PangoCairo dep,
# so an author who wants shaped text vendors this second file (like
# veiland_svg.py) alongside the SDK. It imports gi/Pango at module load and needs
# the Pango + PangoCairo typelibs on GI_TYPELIB_PATH plus their .so's dlopen-able
# (the flake's dev shell wires them; see flake.nix).
#
# Why PangoCairo and not cairo's toy show_text: real titles/names need shaping
# and clean end-ellipsization (a long CJK title is the test case), which
# show_text cannot do. The three draw helpers place one such line top-left,
# vertically centered, or right-aligned -- the arrangements the widgets actually
# use. font_from_config reads font_family/font_size the way parse_color reads
# colors: one parser, keys matching the Rust label plugin, never a crash on a bad
# value. font_size is a FRACTION of a box (like the Rust label's fraction of
# surface height); the caller multiplies it by whichever dimension it anchors to.
#
# See python/examples/now_playing.py and avatar.py for the worked patterns.

from __future__ import annotations

import sys
from collections.abc import Mapping
from dataclasses import dataclass
from typing import Any

import cairo
import gi

gi.require_version("Pango", "1.0")
gi.require_version("PangoCairo", "1.0")
from gi.repository import Pango, PangoCairo  # noqa: E402  (after gi.require_version)

__all__ = [
    "RGB",
    "FontSpec",
    "font_from_config",
    "line_layout",
    "draw_ellipsized",
    "draw_ellipsized_centered",
    "draw_ellipsized_right",
]

# An (r, g, b) color, 0..1 floats -- what the draw helpers take for the text
# fill. (Opaque; text edges are anti-aliased by the mask, not a color alpha.)
# tuple[...] not | so the alias evaluates on the SDK's 3.9 floor.
RGB = tuple[float, float, float]

# Defaults matching the Rust label plugin's config so a widget's font keys mean
# the same thing across tiers. "Sans" is fontconfig's generic sans family;
# font_size is a fraction of a box (the caller decides which side), 0.030 the
# label plugin's default fraction-of-surface-height.
DEFAULT_FONT_FAMILY = "Sans"
DEFAULT_FONT_SIZE = 0.030


@dataclass(frozen=True)
class FontSpec:
    """A resolved font: the family, a size FRACTION (of a box the caller picks),
    weight, and italic flag. Immutable and read once at startup, like the Style
    dataclasses the widgets already build. size is a fraction, NOT pixels: the
    caller multiplies it by whichever dimension it anchors to (region height, a
    pill height, ...), matching the Rust label plugin's fraction-of-surface
    model and how the widgets already derive their own sizes."""

    family: str = DEFAULT_FONT_FAMILY
    size: float = DEFAULT_FONT_SIZE
    weight: Pango.Weight = Pango.Weight.NORMAL
    italic: bool = False


def _weight_from(raw: Any, tag: str) -> Pango.Weight:
    # A CSS-style numeric weight (100..900, the Rust label's font_weight) mapped
    # to the nearest Pango.Weight. Pango.Weight members ARE those numbers, so we
    # clamp and hand the int straight in. Bad value -> NORMAL plus one line.
    if raw is None:
        return Pango.Weight.NORMAL
    try:
        n = int(raw)
    except (TypeError, ValueError):
        print(
            f"{tag}: font_weight: expected a number 100..900, got {raw!r}; "
            "using normal",
            file=sys.stderr,
        )
        return Pango.Weight.NORMAL
    return Pango.Weight(min(1000, max(100, n)))


def font_from_config(cfg: Mapping[str, Any], tag: str = "veiland-text") -> FontSpec:
    """Read a FontSpec from a plugin-config dict, using the same key names as the
    Rust label plugin so a font is configured the same way on either tier:

      font_family = "Noto Sans"   (default "Sans"; fontconfig falls back)
      font_size   = 0.05          (a FRACTION of a box, default 0.030)
      font_weight = 700           (CSS numeric 100..900, default 400/normal)
      italic      = true          (default false)

    Every key is optional; each bad value logs one stderr line tagged with the
    plugin name and falls back to its default -- a mis-typed font key mis-styles
    the widget, it never crashes it (the untrusted-input rule, as parse_color
    does for colors). font_size stays a fraction here: the caller multiplies it
    by the dimension it anchors to before passing pixels to the draw helpers."""
    family_raw = cfg.get("font_family")
    if family_raw is None:
        family = DEFAULT_FONT_FAMILY
    elif isinstance(family_raw, str) and family_raw.strip():
        family = family_raw.strip()
    else:
        print(
            f"{tag}: font_family: expected a non-empty string, got "
            f"{family_raw!r}; using {DEFAULT_FONT_FAMILY!r}",
            file=sys.stderr,
        )
        family = DEFAULT_FONT_FAMILY

    size_raw = cfg.get("font_size")
    if size_raw is None:
        size = DEFAULT_FONT_SIZE
    else:
        try:
            size = float(size_raw)
            if not size > 0.0:
                raise ValueError("non-positive")
        except (TypeError, ValueError):
            print(
                f"{tag}: font_size: expected a positive number, got "
                f"{size_raw!r}; using {DEFAULT_FONT_SIZE}",
                file=sys.stderr,
            )
            size = DEFAULT_FONT_SIZE

    return FontSpec(
        family=family,
        size=size,
        weight=_weight_from(cfg.get("font_weight"), tag),
        italic=bool(cfg.get("italic", False)),
    )


def line_layout(
    cr: cairo.Context[cairo.ImageSurface],
    text: str,
    max_w: float,
    px: float,
    weight: Pango.Weight,
    spec: FontSpec | None = None,
) -> Pango.Layout:
    """One shaped, end-ellipsized single line, sized in PIXELS (px). max_w is the
    pixel width past which it ellipsizes with an ellipsis. Shared by the three
    draw helpers below and usable directly when a caller needs the layout's
    measured extents before placing it (the widgets do, to size a pill).

    spec (from font_from_config) supplies the FAMILY and ITALIC flag -- the
    styling that is uniform across a widget. Weight and size stay per-call
    arguments, NOT taken from spec: weight is a per-LINE role (a title is
    semibold, its artist normal), and size (px) is derived from the geometry of
    the box each line sits in. So spec themes the family; the caller keeps
    deciding each line's weight and size. spec=None keeps the old default
    (fontconfig "sans-serif", upright) so callers that don't thread a font are
    unchanged."""
    layout = PangoCairo.create_layout(cr)
    font = Pango.FontDescription()
    font.set_family(spec.family if spec is not None else "sans-serif")
    font.set_absolute_size(px * Pango.SCALE)
    font.set_weight(weight)
    if spec is not None and spec.italic:
        font.set_style(Pango.Style.ITALIC)
    layout.set_font_description(font)
    layout.set_width(int(max_w * Pango.SCALE))
    layout.set_ellipsize(Pango.EllipsizeMode.END)
    layout.set_text(text, -1)
    return layout


def draw_ellipsized(
    cr: cairo.Context[cairo.ImageSurface],
    text: str,
    x: float,
    y: float,
    max_w: float,
    px: float,
    rgb: RGB,
    weight: Pango.Weight = Pango.Weight.NORMAL,
    spec: FontSpec | None = None,
) -> None:
    # Draw a shaped, end-ellipsized line with its TOP-LEFT at (x, y). The whole
    # reason a text widget uses PangoCairo and not cairo's toy text. spec themes
    # the family/italic (see line_layout); weight/size stay per-call.
    layout = line_layout(cr, text, max_w, px, weight, spec)
    cr.move_to(x, y)
    cr.set_source_rgb(*rgb)
    PangoCairo.show_layout(cr, layout)


def draw_ellipsized_centered(
    cr: cairo.Context[cairo.ImageSurface],
    text: str,
    x: float,
    cy: float,
    max_w: float,
    px: float,
    rgb: RGB,
    weight: Pango.Weight = Pango.Weight.NORMAL,
    spec: FontSpec | None = None,
) -> None:
    # Same, but vertically CENTERED on cy: the measured line height (which
    # differs by script -- CJK is taller than Latin) grows symmetrically around
    # cy instead of downward, so a tall CJK title can't shove what's below it.
    layout = line_layout(cr, text, max_w, px, weight, spec)
    _, logical = layout.get_pixel_extents()
    cr.move_to(x, cy - logical.height / 2)
    cr.set_source_rgb(*rgb)
    PangoCairo.show_layout(cr, layout)


def draw_ellipsized_right(
    cr: cairo.Context[cairo.ImageSurface],
    text: str,
    right_x: float,
    y: float,
    px: float,
    rgb: RGB,
    weight: Pango.Weight = Pango.Weight.NORMAL,
    spec: FontSpec | None = None,
) -> None:
    # Same shaped line, but its RIGHT edge sits at right_x (measure, then place).
    # Used for the right-aligned total time in both layouts. 1e6 max width so
    # the line never ellipsizes -- callers use this for short strings.
    layout = line_layout(cr, text, 1e6, px, weight, spec)
    _, logical = layout.get_pixel_extents()
    cr.move_to(right_x - logical.width, y)
    cr.set_source_rgb(*rgb)
    PangoCairo.show_layout(cr, layout)
