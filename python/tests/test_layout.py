# SPDX-License-Identifier: GPL-3.0-or-later
#
# Pure-logic tests for veiland_layout -- the 9-point content anchor, its two
# config parsers, and the debug-border draw helper. veiland_layout imports cairo
# at module load for draw_debug_border, but that helper only calls METHODS on a
# cairo context the caller passes; the anchor/parser logic touches no cairo at
# all. So we stub cairo before the import (like test_svg.py) and exercise
# draw_debug_border with a fake context that records its calls -- the whole file
# stays display-free.

from __future__ import annotations

import sys
import types
from typing import Any, cast

sys.modules.setdefault("cairo", types.ModuleType("cairo"))

import veiland_layout as vl  # noqa: E402  (after the cairo stub, deliberately)

# ------------------------------------------------------------- anchor_offset
#
# A deliberately asymmetric region (100 wide, 80 tall) and a small block
# (40 x 20) so every one of the 9 anchors lands at a distinct, hand-checkable
# (x, y). center-x = (100-40)/2 = 30, right-x = 100-40 = 60; center-y =
# (80-20)/2 = 30, bottom-y = 80-20 = 60.

REGION_W, REGION_H = 100.0, 80.0
BLOCK_W, BLOCK_H = 40.0, 20.0

_EXPECTED = {
    ("left", "top"): (0.0, 0.0),
    ("center", "top"): (30.0, 0.0),
    ("right", "top"): (60.0, 0.0),
    ("left", "center"): (0.0, 30.0),
    ("center", "center"): (30.0, 30.0),
    ("right", "center"): (60.0, 30.0),
    ("left", "bottom"): (0.0, 60.0),
    ("center", "bottom"): (30.0, 60.0),
    ("right", "bottom"): (60.0, 60.0),
}


def test_all_nine_positions():
    # Every (halign, valign) combo places the block where the doc's formula says.
    for (halign, valign), expected in _EXPECTED.items():
        got = vl.anchor_offset(halign, valign, REGION_W, REGION_H, BLOCK_W, BLOCK_H)
        assert got == expected, f"{halign}/{valign}: {got} != {expected}"


def test_block_equals_region_is_a_noop_for_every_anchor():
    # The key invariant: when the block fills the region there is no slack, so
    # every one of the 9 anchors returns (0, 0). A full-width pill or a compact
    # card that fills its box must not shift no matter what content_* is set to.
    for halign in ("left", "center", "right"):
        for valign in ("top", "center", "bottom"):
            got = vl.anchor_offset(
                halign, valign, REGION_W, REGION_H, REGION_W, REGION_H
            )
            assert got == (0.0, 0.0), f"{halign}/{valign} moved a full block"


def test_unknown_align_falls_back_to_center_without_raising():
    # anchor_offset may be called with values that never passed through
    # anchor_from_config (plugin config is untrusted), so a garbage align must
    # center rather than KeyError.
    got = vl.anchor_offset("sideways", "diagonal", REGION_W, REGION_H, BLOCK_W, BLOCK_H)
    assert got == (30.0, 30.0)


# ----------------------------------------------------------- anchor_from_config


def test_absent_keys_default_to_center_center():
    assert vl.anchor_from_config({}) == ("center", "center")


def test_valid_values_pass_through():
    cfg = {"content_halign": "left", "content_valign": "bottom"}
    assert vl.anchor_from_config(cfg) == ("left", "bottom")


def test_bad_halign_falls_back_and_logs(capsys):
    got = vl.anchor_from_config({"content_halign": "middle"}, tag="markup")
    assert got == ("center", "center")
    err = capsys.readouterr().err
    assert "markup: content_halign:" in err and "using 'center'" in err


def test_bad_valign_falls_back_and_logs(capsys):
    got = vl.anchor_from_config({"content_valign": 3}, tag="avatar")
    assert got == ("center", "center")
    err = capsys.readouterr().err
    assert "avatar: content_valign:" in err and "using 'center'" in err


# ------------------------------------------------------ debug_border_from_config


def test_debug_border_absent_is_off_with_magenta_default():
    enabled, color = vl.debug_border_from_config({})
    assert enabled is False
    assert color == (1.0, 0.0, 1.0, 1.0)


def test_debug_border_true_enables():
    enabled, _ = vl.debug_border_from_config({"debug_border": True})
    assert enabled is True


def test_debug_border_custom_color_parses_and_clamps():
    cfg = {"debug_border": True, "debug_border_color": [0, 255, 0.5, 2]}
    enabled, color = vl.debug_border_from_config(cfg)
    assert enabled is True
    assert color == (0.0, 1.0, 0.5, 1.0)  # 255 and 2 clamp to 1.0


def test_debug_border_bad_color_falls_back_and_logs(capsys):
    cfg = {"debug_border": True, "debug_border_color": "magenta"}
    enabled, color = vl.debug_border_from_config(cfg, tag="wifi")
    assert enabled is True
    assert color == (1.0, 0.0, 1.0, 1.0)
    err = capsys.readouterr().err
    assert "wifi: debug_border_color:" in err and "using default" in err


# ------------------------------------------------------------ draw_debug_border


class _RecordingContext:
    """A fake cairo context that records the calls draw_debug_border makes, so we
    can assert the geometry without a real cairo/display. Only the four methods
    the helper uses are implemented."""

    def __init__(self) -> None:
        self.calls: list[tuple[object, ...]] = []

    def set_source_rgba(self, *rgba: float) -> None:
        self.calls.append(("set_source_rgba", *rgba))

    def set_line_width(self, w: float) -> None:
        self.calls.append(("set_line_width", w))

    def rectangle(self, x: float, y: float, w: float, h: float) -> None:
        self.calls.append(("rectangle", x, y, w, h))

    def stroke(self) -> None:
        self.calls.append(("stroke",))


def test_draw_debug_border_strokes_inset_rectangle():
    cr = _RecordingContext()
    # cast: the double duck-types the four cairo methods the helper uses; the
    # helper's param is a real cairo.Context, so tell mypy we mean it.
    vl.draw_debug_border(cast(Any, cr), 200.0, 120.0, (1.0, 0.0, 1.0, 1.0))
    assert cr.calls == [
        ("set_source_rgba", 1.0, 0.0, 1.0, 1.0),
        ("set_line_width", 1.0),
        # 0.5px inset so the whole 1px stroke lands inside the box (0.5..199.5).
        ("rectangle", 0.5, 0.5, 199.0, 119.0),
        ("stroke",),
    ]
