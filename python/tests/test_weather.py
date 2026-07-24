# SPDX-License-Identifier: GPL-3.0-or-later
#
# Pure-logic tests for weather.py: the WMO code -> glyph / label bucketers, the
# Open-Meteo JSON parsers, unit conversion, config coercion, and the disk-cache
# round-trip. weather.py's draw half imports the cairo + gi (librsvg/Pango)
# stack and the veiland companions at module load; none of that is in the
# minimal CI python env, so this file installs just-enough stubs BEFORE
# importing weather, exactly as test_svg.py does. The functions under test are
# stdlib-pure and never touch the graphics stack; the drawing half stays
# untested here (it needs the real librsvg/Pango/GBM path).

from __future__ import annotations

import os
import sys
import types

# --------------------------------------------------------------------- stubs
# weather.py pulls `import cairo` and the veiland companions, which do
# `import gi` + `from gi.repository import GLib, Rsvg, Pango, PangoCairo`.
# veiland_text reads Pango.Weight.NORMAL at class-definition (import) time; the
# rest of gi is only touched inside draw calls we don't run.
#
# We must AUGMENT rather than setdefault: pytest collects every test in one
# process, so a sibling suite (test_svg) may have already installed a
# gi.repository stub carrying only Rsvg/GLib -- setdefault would then no-op and
# leave `from gi.repository import Pango` failing. So take whatever gi.repository
# is live (a sibling's stub, or a fresh one) and ensure each attribute this
# module needs is present without clobbering ones already set.
_gi = sys.modules.setdefault("gi", types.ModuleType("gi"))
if not hasattr(_gi, "require_version"):
    _gi.require_version = lambda *a, **k: None  # type: ignore[attr-defined]
_repo = sys.modules.setdefault("gi.repository", types.ModuleType("gi.repository"))
_gi.repository = _repo  # type: ignore[attr-defined]
if not hasattr(_repo, "GLib"):
    _repo.GLib = types.SimpleNamespace(GError=Exception)  # type: ignore[attr-defined]
if not hasattr(_repo, "Rsvg"):
    _repo.Rsvg = types.SimpleNamespace()  # type: ignore[attr-defined]
if not hasattr(_repo, "Pango"):
    _repo.Pango = types.SimpleNamespace(  # type: ignore[attr-defined]
        Weight=types.SimpleNamespace(NORMAL=400)
    )
if not hasattr(_repo, "PangoCairo"):
    _repo.PangoCairo = types.SimpleNamespace()  # type: ignore[attr-defined]
sys.modules.setdefault("cairo", types.ModuleType("cairo"))

# weather.py lives in examples/, which conftest.py does not add to sys.path
# (it adds only python/, the SDK root); add examples/ here.
sys.path.insert(
    0,
    os.path.join(
        os.path.dirname(os.path.dirname(os.path.abspath(__file__))), "examples"
    ),
)

import weather as w  # noqa: E402  (after the stubs + examples/ path insert)

# ------------------------------------------------------------------- pick_icon


def test_pick_icon_clear_splits_day_night():
    assert w.pick_icon(0, True) == "weather-clear.svg"
    assert w.pick_icon(0, False) == "weather-clear-night.svg"


def test_pick_icon_partly_cloudy_splits_day_night():
    assert w.pick_icon(1, True) == "weather-partly-cloudy.svg"
    assert w.pick_icon(2, False) == "weather-partly-cloudy-night.svg"


def test_pick_icon_overcast_has_no_night_variant():
    assert w.pick_icon(3, True) == "weather-cloudy.svg"
    assert w.pick_icon(3, False) == "weather-cloudy.svg"


def test_pick_icon_precip_buckets():
    assert w.pick_icon(48, True) == "weather-fog.svg"
    assert w.pick_icon(55, True) == "weather-drizzle.svg"
    assert w.pick_icon(65, True) == "weather-rain.svg"
    assert w.pick_icon(82, False) == "weather-rain.svg"  # rain showers
    assert w.pick_icon(75, True) == "weather-snow.svg"
    assert w.pick_icon(86, True) == "weather-snow.svg"  # snow showers
    assert w.pick_icon(96, True) == "weather-thunder.svg"


def test_pick_icon_unknown_code_is_neutral():
    assert w.pick_icon(42, True) == "weather-unknown.svg"
    assert w.pick_icon(100, False) == "weather-unknown.svg"
    assert w.pick_icon(-1, True) == "weather-unknown.svg"


# --------------------------------------------------------------- condition_text


def test_condition_text_known_and_unknown():
    assert w.condition_text(0) == "Clear"
    assert w.condition_text(61) == "Light rain"
    assert w.condition_text(95) == "Thunderstorm"
    assert w.condition_text(42) == "—"


# --------------------------------------------------------------- display_temp


def test_display_temp_none_is_placeholder():
    assert w.display_temp(None, "celsius") == "—°"


def test_display_temp_celsius_rounds():
    r = w.Reading(temp_c=18.4, code=0, is_day=True, fetched_at=0)
    assert w.display_temp(r, "celsius") == "18°"


def test_display_temp_fahrenheit_converts():
    r = w.Reading(temp_c=0.0, code=0, is_day=True, fetched_at=0)
    assert w.display_temp(r, "fahrenheit") == "32°"
    r2 = w.Reading(temp_c=100.0, code=0, is_day=True, fetched_at=0)
    assert w.display_temp(r2, "fahrenheit") == "212°"


# ----------------------------------------------------------------- parse_forecast


def test_parse_forecast_valid():
    data = {"current": {"temperature_2m": 18.3, "weather_code": 3, "is_day": 1}}
    r = w.parse_forecast(data)
    assert r is not None
    assert r.temp_c == 18.3 and r.code == 3 and r.is_day is True


def test_parse_forecast_night_flag():
    data = {"current": {"temperature_2m": 5, "weather_code": 0, "is_day": 0}}
    r = w.parse_forecast(data)
    assert r is not None and r.is_day is False


def test_parse_forecast_malformed_returns_none():
    assert w.parse_forecast({}) is None  # no "current"
    assert w.parse_forecast({"current": {}}) is None  # no fields
    bad_type = {"current": {"temperature_2m": "x", "weather_code": 1, "is_day": 1}}
    assert w.parse_forecast(bad_type) is None  # bad type


# ----------------------------------------------------------------- parse_geocode


def test_parse_geocode_first_result():
    data = {"results": [{"latitude": 48.85, "longitude": 2.35, "name": "Paris"}]}
    assert w.parse_geocode(data) == (48.85, 2.35)


def test_parse_geocode_no_match_returns_none():
    assert w.parse_geocode({}) is None
    assert w.parse_geocode({"results": []}) is None


# ------------------------------------------------------------------- config


def test_parse_units():
    assert w.parse_units({"units": "fahrenheit"}) == "fahrenheit"
    assert w.parse_units({}) == "celsius"
    assert w.parse_units({"units": "kelvin"}) == "celsius"  # bad -> default


def test_parse_layout():
    assert w.parse_layout({"layout": "pill"}) == "pill"
    assert w.parse_layout({}) == "card"
    assert w.parse_layout({"layout": "banner"}) == "card"


def test_parse_refresh_minutes_floors_and_defaults():
    assert w.parse_refresh_minutes({"refresh_minutes": 30}) == 30.0
    assert w.parse_refresh_minutes({"refresh_minutes": 1}) == 5.0  # floor
    assert w.parse_refresh_minutes({}) == 15.0
    assert w.parse_refresh_minutes({"refresh_minutes": "soon"}) == 15.0


def test_parse_coords_valid_and_partial():
    assert w.parse_coords({"latitude": 48.85, "longitude": 2.35}) == (48.85, 2.35)
    assert w.parse_coords({}) is None
    assert w.parse_coords({"latitude": 48.85}) is None  # partial
    assert w.parse_coords({"latitude": 999, "longitude": 0}) is None  # range


# ------------------------------------------------------------------- cache


def test_cache_roundtrip(tmp_path, monkeypatch):
    monkeypatch.setattr(w, "CACHE_DIR", str(tmp_path))
    monkeypatch.setattr(w, "CACHE_FILE", str(tmp_path / "reading.json"))
    r = w.Reading(temp_c=12.5, code=61, is_day=True, fetched_at=1690000000)
    w.write_cache(r)
    assert w.read_cache() == r


def test_read_cache_missing_and_corrupt(tmp_path, monkeypatch):
    monkeypatch.setattr(w, "CACHE_FILE", str(tmp_path / "reading.json"))
    assert w.read_cache() is None  # missing
    (tmp_path / "reading.json").write_text("{ not json")
    assert w.read_cache() is None  # corrupt
