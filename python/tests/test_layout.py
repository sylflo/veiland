# SPDX-License-Identifier: GPL-3.0-or-later
#
# Pure-logic tests for veiland_layout -- the 9-point content anchor and its two
# config parsers. Unlike test_svg.py, this file needs NO module stubs:
# veiland_layout is pure stdlib (float math + dict reading), so it imports and
# runs anywhere pytest does, no graphics stack required. That is the whole point
# of keeping the anchor helpers dependency-free -- the placement math is testable
# without a display.

from __future__ import annotations

import veiland_layout as vl

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
