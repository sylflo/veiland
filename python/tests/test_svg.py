# SPDX-License-Identifier: GPL-3.0-or-later
#
# Pure-logic tests for veiland_svg.parse_color -- the config -> RGBA validator
# the status pills share. veiland_svg imports the SVG stack (gi/librsvg) and
# pycairo at module load, and neither is in the minimal CI python env, so this
# file installs just-enough module stubs BEFORE the import; parse_color itself
# is stdlib-pure and never touches them. The drawing half (draw_pill, draw_svg
# and its tint path) stays untested here for the same reason the pick_icon
# bucketers are (see test_dbus.py's header): it needs the real graphics stack.

from __future__ import annotations

import sys
import types

# --------------------------------------------------------------------- stubs
# Installed with setdefault so a module something else already imported (a
# dev-shell run with the real stack loaded) is never clobbered. veiland_svg's
# import surface is: `import cairo`, `import gi`, gi.require_version(...), and
# `from gi.repository import GLib, Rsvg` -- GLib.GError is only *raised through*
# at runtime, and Rsvg/cairo attributes are only touched inside draw calls.

_repo = types.ModuleType("gi.repository")
_repo.GLib = types.SimpleNamespace(GError=Exception)  # type: ignore[attr-defined]
_repo.Rsvg = types.SimpleNamespace()  # type: ignore[attr-defined]
_gi = types.ModuleType("gi")
_gi.require_version = lambda *a, **k: None  # type: ignore[attr-defined]
_gi.repository = _repo  # type: ignore[attr-defined]
sys.modules.setdefault("gi", _gi)
sys.modules.setdefault("gi.repository", _repo)
sys.modules.setdefault("cairo", types.ModuleType("cairo"))

import veiland_svg as vs  # noqa: E402  (after the stubs, deliberately)

# ---------------------------------------------------------------- parse_color

DEFAULT = (0.1, 0.2, 0.3, 0.4)


def test_absent_key_returns_default_untouched():
    assert vs.parse_color({}, "pill_color", DEFAULT) is DEFAULT


def test_absent_key_passes_none_default_through():
    # None default == "feature off" (the pills' icon_color un-tinted state).
    assert vs.parse_color({}, "icon_color", None) is None


def test_valid_list_becomes_float_tuple():
    got = vs.parse_color({"c": [0.5, 0, 1, 0.25]}, "c", DEFAULT)
    assert got == (0.5, 0.0, 1.0, 0.25)
    assert all(isinstance(v, float) for v in got)


def test_ints_accepted():
    # TOML integers arrive as JSON ints ([1, 0, 0, 1] is a natural spelling).
    assert vs.parse_color({"c": [1, 0, 0, 1]}, "c", DEFAULT) == (1.0, 0.0, 0.0, 1.0)


def test_out_of_range_channels_clamp():
    # A 0-255 habit clamps toward something sane rather than erroring out.
    got = vs.parse_color({"c": [255, -1, 2, 0.5]}, "c", DEFAULT)
    assert got == (1.0, 0.0, 1.0, 0.5)


def test_wrong_length_falls_back_and_logs(capsys):
    assert vs.parse_color({"c": [1, 0, 0]}, "c", DEFAULT, tag="wifi") is DEFAULT
    err = capsys.readouterr().err
    assert "wifi: c:" in err and "using default" in err


def test_non_list_falls_back(capsys):
    # Hex strings are NOT the format (nothing in veiland takes them).
    assert vs.parse_color({"c": "#ff0000"}, "c", DEFAULT) is DEFAULT
    assert "using default" in capsys.readouterr().err


def test_non_numeric_item_falls_back(capsys):
    assert vs.parse_color({"c": [1, 0, "x", 1]}, "c", DEFAULT) is DEFAULT
    assert "using default" in capsys.readouterr().err
