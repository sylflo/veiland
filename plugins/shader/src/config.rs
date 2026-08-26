// SPDX-License-Identifier: GPL-3.0-or-later

//! Config for veiland-shader: which shader to run and the thermal/timing
//! knobs. Raw values arrive as a JSON-serialised TOML table (via
//! `VEILAND_PLUGIN_CONFIG`); everything here is clamped with `sane()`
//! before it reaches GL, since the config is untrusted input.

use serde::Deserialize;

/// Raw config as deserialised from the host. All fields optional; missing
/// ones take the documented defaults. Validated into [`Settings`] by
/// [`Settings::from`].
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Name of an embedded preset shipped in the binary (e.g. "nebula").
    /// Ignored if `path` is also set.
    pub preset: Option<String>,
    /// Absolute path to a user-supplied `.frag` file. Wins over `preset`
    /// if both are set. Unreadable, non-UTF-8, empty or oversized files log
    /// and fall back to the dark fill.
    pub path: Option<String>,
    /// Buffer size as a fraction of the region, 0.1..=1.0. The host
    /// upsamples bilinearly, so <1.0 trades sharpness for fewer pixels.
    pub render_scale: Option<f32>,
    /// Cap on submitted frames per second, 0..=240. 0 = compositor rate.
    pub max_fps: Option<f32>,
    /// If >0, iTime wraps modulo this many seconds. 0 = raw iTime
    /// (Shadertoy-faithful; drifts on multi-day locks — see docs).
    pub loop_seconds: Option<f32>,
    /// Layer opacity, 0.0..=1.0, default 1.0. At 1.0 the output is opaque:
    /// the shader's alpha is ignored (Shadertoy shaders write undefined
    /// alpha, so ignoring it is the safe default for a background). Below
    /// 1.0 the shader's per-pixel alpha is honoured and scaled by this
    /// value, so the layer composites over lower z-layers — the mode an
    /// overlay (moon, aurora) needs.
    pub opacity: Option<f32>,
}

/// Which shader source the config selected.
#[derive(Debug, Clone)]
pub enum Source {
    /// A named embedded preset. Resolved against the preset table; an
    /// unknown name falls back at that point.
    Preset(String),
    /// A user file path.
    Path(String),
}

/// Validated, clamped config ready to drive rendering.
#[derive(Debug, Clone)]
pub struct Settings {
    pub source: Source,
    pub render_scale: f64,
    /// `None` = uncapped (compositor rate); `Some(fps)` otherwise.
    pub max_fps: Option<f32>,
    /// `None` = raw iTime; `Some(secs)` = wrap period.
    pub loop_seconds: Option<f32>,
    /// Layer opacity in `[0, 1]`. `1.0` = opaque (shader alpha ignored);
    /// `< 1.0` = honour+scale shader alpha and composite over lower layers.
    pub opacity: f32,
}

/// Default preset when neither `preset` nor `path` is given.
const DEFAULT_PRESET: &str = "nebula";
const DEFAULT_RENDER_SCALE: f32 = 1.0;
const DEFAULT_MAX_FPS: f32 = 30.0;
const DEFAULT_OPACITY: f32 = 1.0;

/// Clamp a config float to a range, falling back if non-finite. Same idiom
/// as the raymarcher: the value crossed JSON, so NaN/inf are possible and
/// must not poison GL uniforms.
fn sane(x: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x.clamp(lo, hi)
    } else {
        fallback
    }
}

impl From<Config> for Settings {
    fn from(c: Config) -> Self {
        // path wins over preset.
        let source = match (c.path, c.preset) {
            (Some(p), _) => Source::Path(p),
            (None, Some(name)) => Source::Preset(name),
            (None, None) => Source::Preset(DEFAULT_PRESET.to_string()),
        };

        let render_scale = f64::from(sane(
            c.render_scale.unwrap_or(DEFAULT_RENDER_SCALE),
            0.1,
            1.0,
            DEFAULT_RENDER_SCALE,
        ));

        let max_fps = sane(
            c.max_fps.unwrap_or(DEFAULT_MAX_FPS),
            0.0,
            240.0,
            DEFAULT_MAX_FPS,
        );
        let max_fps = (max_fps > 0.0).then_some(max_fps);

        // loop_seconds: 0 (or unset) = no wrap. Clamp the upper bound to a
        // day so a typo can't produce an absurd period; the useful range is
        // small anyway (a few seconds to minutes).
        let loop_raw = sane(c.loop_seconds.unwrap_or(0.0), 0.0, 86_400.0, 0.0);
        let loop_seconds = (loop_raw > 0.0).then_some(loop_raw);

        let opacity = sane(
            c.opacity.unwrap_or(DEFAULT_OPACITY),
            0.0,
            1.0,
            DEFAULT_OPACITY,
        );

        Settings {
            source,
            render_scale,
            max_fps,
            loop_seconds,
            opacity,
        }
    }
}
