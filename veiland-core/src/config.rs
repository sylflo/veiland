// SPDX-License-Identifier: GPL-3.0-or-later

//! Veiland's user-facing config file. Loaded once at host startup
//! from `$VEILAND_CONFIG` (dev/test override) or
//! `$XDG_CONFIG_HOME/veiland/config.toml` (defaults to
//! `$HOME/.config/veiland/config.toml`). Drives the multi-plugin
//! spawn in M6.
//!
//! See `docs/config.md` (M6 step 8) for the user-facing schema.

use std::path::{Path, PathBuf};

use serde::Deserialize;

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Config {
    /// Plugins to spawn, in declaration order. The host sorts by
    /// `z_index` at spawn time; ties keep config-file order.
    #[serde(rename = "plugin", default)]
    pub plugins: Vec<PluginEntry>,

    /// Password-indicator config. Missing `[password]` table →
    /// `Password::default()`. Missing individual fields → that
    /// field's per-fn default (see the struct doc-comments and
    /// `validate_password` for clamping ranges).
    #[serde(default)]
    pub password: Password,
}

#[derive(Clone, Debug, Deserialize)]
pub struct PluginEntry {
    /// Used in logs and to disambiguate `[[plugin]]` entries.
    /// Must be non-empty and unique within the config.
    pub name: String,

    /// Absolute path to the plugin binary. No tilde expansion in
    /// M6 — write the full path. Spawn failure is logged at runtime.
    pub binary: PathBuf,

    /// Lower = behind. Ties broken by config-file order (stable sort).
    /// Negative values are legitimate ("always behind everything").
    pub z_index: i32,

    /// Optional. `None` means "fill the whole lock surface" — the
    /// default is resolved at Configure time, not here, because we
    /// don't know the surface size at config-load time.
    #[serde(default)]
    pub region: Option<Region>,

    /// Output names (xdg_output.name strings, e.g. "DP-1") this
    /// plugin runs on. `None` (field absent) means "every connected
    /// output." `Some(vec)` must be non-empty; the loader rejects an
    /// explicit empty list (ambiguous — see docs/config.md §3).
    /// Names that don't match a connected output at spawn time log
    /// a warning and produce zero instances; they don't fail the
    /// locker start (a typo shouldn't lock the user out).
    #[serde(default)]
    pub monitors: Option<Vec<String>>,

    /// Optional pass-through table for plugin-specific settings.
    /// Serialised to JSON at spawn time and exported to the plugin
    /// as `VEILAND_PLUGIN_CONFIG`. Plugins parse it however they
    /// like (`serde_json` is the obvious choice). See
    /// `docs/config.md` §3 (`[plugin.config]`) and
    /// `docs/protocol.md` §2 (spawning).
    #[serde(default)]
    pub config: Option<toml::Value>,
}

#[derive(Clone, Debug, Deserialize)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// An RGBA colour, parsed from a Hyprlock-style `rgba(r, g, b, a)` string.
/// `r`/`g`/`b` are 0–255 integers, `a` is a 0.0–1.0 float. `rgb(r, g, b)`
/// (alpha implied 1.0) is also accepted.
///
/// Stored as **straight** (non-premultiplied) components in `0.0..=1.0`.
/// The renderer premultiplies at draw time — the indicator and box both
/// paint under `glBlendFunc(ONE, ONE_MINUS_SRC_ALPHA)`, so the shader
/// emits `rgb * a`. Keeping the stored value straight (and human-readable)
/// matches how the value was written in the config.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Color(pub [f32; 4]);

impl Color {
    /// Construct from 0.0..=1.0 straight components. `const` so it can back
    /// the `default_*_color` fns.
    pub const fn new(r: f32, g: f32, b: f32, a: f32) -> Self {
        Color([r, g, b, a])
    }
}

/// Parse a Hyprlock-style `rgba(r, g, b, a)` / `rgb(r, g, b)` colour string.
///
/// `r`/`g`/`b` are integers in `0..=255` (rejected if out of range — a
/// typo like `rgba(300, 0, 0, 1)` is a config error worth surfacing). `a`
/// is a float, **clamped** to `0.0..=1.0` rather than rejected (an alpha of
/// `1.5` is an obvious "fully opaque" intent; clamping is friendlier than a
/// hard parse error). The returned `Color` holds straight components scaled
/// to `0.0..=1.0`.
///
/// Returns a descriptive `Err(String)` on anything malformed so the TOML
/// deserialiser can attach it to the offending line (`toml::de::Error`
/// carries line/column context around the custom message).
fn parse_rgba(s: &str) -> Result<Color, String> {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();

    // Strip the `rgba(...)` / `rgb(...)` wrapper. We accept either prefix;
    // the part count (3 vs 4) is what actually decides whether an alpha is
    // present, so a stray `rgb(r,g,b,a)` still parses as 4 components.
    let inner = if let Some(rest) = lower.strip_prefix("rgba(") {
        rest
    } else if let Some(rest) = lower.strip_prefix("rgb(") {
        rest
    } else {
        return Err(format!(
            "colour must look like rgba(r, g, b, a) or rgb(r, g, b); got {:?}",
            s
        ));
    };
    let inner = inner.strip_suffix(')').ok_or_else(|| {
        format!("colour {:?} is missing its closing ')'", s)
    })?;

    let parts: Vec<&str> = inner.split(',').map(|p| p.trim()).collect();
    if parts.len() != 3 && parts.len() != 4 {
        return Err(format!(
            "colour {:?} needs 3 (rgb) or 4 (rgba) comma-separated components, \
            got {}",
            s,
            parts.len()
        ));
    }

    let channel = |idx: usize| -> Result<f32, String> {
        let raw = parts[idx];
        let v: i32 = raw
            .parse()
            .map_err(|_| format!("colour channel {:?} is not an integer", raw))?;
        if !(0..=255).contains(&v) {
            return Err(format!(
                "colour channel {} out of range [0, 255]",
                v
            ));
        }
        Ok(v as f32 / 255.0)
    };
    let r = channel(0)?;
    let g = channel(1)?;
    let b = channel(2)?;

    let a = if parts.len() == 4 {
        let raw = parts[3];
        let v: f32 = raw
            .parse()
            .map_err(|_| format!("colour alpha {:?} is not a number", raw))?;
        // Clamp rather than reject — see the doc comment.
        v.clamp(0.0, 1.0)
    } else {
        1.0
    };

    Ok(Color::new(r, g, b, a))
}

impl<'de> serde::Deserialize<'de> for Color {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        parse_rgba(&s).map_err(serde::de::Error::custom)
    }
}

/// Password-indicator configuration. All fields are optional in
/// the on-disk schema; missing fields take per-field defaults
/// (see the `default_*` fns below for the constants).
///
/// `x` and `y_percent` are `Option<i32>` rather than plain `i32`
/// because their defaults are *surface-relative* and can't be
/// resolved at config-load time (we don't know the surface size
/// yet). The renderer maps `None` to "centre horizontally on
/// this surface" / "75% down this surface".
///
/// The other three (`dot_diameter`, `dot_spacing`, `max_dots`)
/// have absolute defaults, so they're plain values and the
/// renderer doesn't need to think about `None`.
#[derive(Clone, Debug, Deserialize)]
pub struct Password {
    /// Horizontal centre of the dot row, in surface-pixel coords.
    /// `None` → centred (renderer computes `width / 2` per output).
    #[serde(default)]
    pub x: Option<i32>,

    /// Vertical position as a percentage of surface height (0..=100).
    /// `None` → 75. Clamped to [0, 100] at load with a warning if
    /// out of range. (i32, not u32, so out-of-range values from
    /// users who write negatives are caught at clamping rather than
    /// rejected at parse time.)
    #[serde(default)]
    pub y_percent: Option<i32>,

    /// Dot diameter in surface pixels. Default 12; clamped to [1, 100].
    #[serde(default = "default_dot_diameter")]
    pub dot_diameter: u32,

    /// Centre-to-centre stride between consecutive dots in surface
    /// pixels. Default 20; clamped to [1, 200]. With diameter 12,
    /// the default leaves an 8-px gap between dot edges.
    #[serde(default = "default_dot_spacing")]
    pub dot_spacing: u32,

    /// Cap on the number of visible dots. Default 32; clamped to
    /// [1, 256]. Beyond this, the indicator row freezes — the user
    /// keeps typing but the dot count stops growing.
    #[serde(default = "default_max_dots")]
    pub max_dots: u32,

    /// Draw the input box (rounded pill) behind the dots. Default
    /// `true` — the box is the default look and shows the user where
    /// to type even before any keystroke. `false` reproduces the
    /// pre-box behaviour: bare dots floating on the wallpaper, dots
    /// positioned by `x`/`y_percent`.
    #[serde(default = "default_show_box")]
    pub show_box: bool,

    /// Input-box width in surface pixels. Default 400; clamped to
    /// [1, 8192] (the threat-model implausible-size ceiling). Ignored
    /// when `show_box = false`.
    #[serde(default = "default_box_width")]
    pub box_width: u32,

    /// Input-box height in surface pixels. Default 90; clamped to
    /// [1, 8192]. Ignored when `show_box = false`.
    #[serde(default = "default_box_height")]
    pub box_height: u32,

    /// Outline thickness in surface pixels. Default 2; clamped to
    /// [0, box_height/2] (0 = no outline; can't exceed half-height or
    /// the outline would consume the box). Ignored when
    /// `show_box = false`.
    #[serde(default = "default_outline_thickness")]
    pub outline_thickness: u32,

    /// Corner radius in surface pixels. Default -1, a sentinel meaning
    /// "full pill" (radius = box_height/2). Any other value is clamped
    /// to [0, min(box_width, box_height)/2]. Ignored when
    /// `show_box = false`.
    #[serde(default = "default_rounding")]
    pub rounding: i32,

    /// Box fill colour. Default a dark translucent blue-grey
    /// `rgba(34, 41, 56, 0.55)` sampled from the reference mockup.
    #[serde(default = "default_inner_color")]
    pub inner_color: Color,

    /// Box outline colour. Default a light translucent
    /// `rgba(180, 190, 210, 0.55)`.
    #[serde(default = "default_outer_color")]
    pub outer_color: Color,

    /// Dot colour. Default `rgba(220, 220, 220, 1.0)` — the value the
    /// dots were hardcoded to before colours were configurable.
    #[serde(default = "default_dot_color")]
    pub dot_color: Color,

    /// Placeholder text shown centred in the box when nothing has been
    /// typed yet. Default `"Enter to remember..."`. An empty string
    /// disables the placeholder (empty box). Rendered by the core via
    /// `veiland-text`; once the user types, the dots replace it.
    #[serde(default = "default_placeholder_text")]
    pub placeholder_text: String,

    /// Placeholder text colour. Default a dim translucent light grey
    /// `rgba(200, 205, 215, 0.6)` so it reads as a hint, not a value.
    #[serde(default = "default_placeholder_color")]
    pub placeholder_color: Color,

    /// Placeholder font family (CSS-style name; falls back to system
    /// Sans). Default `"Sans"`.
    #[serde(default = "default_placeholder_font_family")]
    pub placeholder_font_family: String,

    /// Placeholder font size in surface pixels. Default 18; clamped to
    /// [1, 512]. (Surface pixels, not scaled — the core has no per-output
    /// scale plumbed into the field yet, matching the box dimensions.)
    #[serde(default = "default_placeholder_font_size")]
    pub placeholder_font_size: u32,

    /// Box fill colour override when the last auth attempt failed.
    /// Applied for ~1.5 s then reverts to `inner_color`.
    #[serde(default = "default_fail_color")]
    pub fail_color: Color,

    /// Box fill colour override while caps lock is active.
    #[serde(default = "default_capslock_color")]
    pub capslock_color: Color,
}

impl Default for Password {
    fn default() -> Self {
        Self {
            x: None,
            y_percent: None,
            dot_diameter: default_dot_diameter(),
            dot_spacing: default_dot_spacing(),
            max_dots: default_max_dots(),
            show_box: default_show_box(),
            box_width: default_box_width(),
            box_height: default_box_height(),
            outline_thickness: default_outline_thickness(),
            rounding: default_rounding(),
            inner_color: default_inner_color(),
            outer_color: default_outer_color(),
            dot_color: default_dot_color(),
            placeholder_text: default_placeholder_text(),
            placeholder_color: default_placeholder_color(),
            placeholder_font_family: default_placeholder_font_family(),
            placeholder_font_size: default_placeholder_font_size(),
            fail_color: default_fail_color(),
            capslock_color: default_capslock_color(),
        }
    }
}

fn default_dot_diameter() -> u32 {
    12
}
fn default_dot_spacing() -> u32 {
    20
}
fn default_max_dots() -> u32 {
    32
}
fn default_show_box() -> bool {
    true
}
fn default_box_width() -> u32 {
    400
}
fn default_box_height() -> u32 {
    90
}
fn default_outline_thickness() -> u32 {
    2
}
/// `-1` is the "full pill" sentinel (radius = box_height/2); see the
/// `rounding` field doc.
fn default_rounding() -> i32 {
    -1
}
/// Dark translucent fill sampled from the reference mockup.
fn default_inner_color() -> Color {
    Color::new(34.0 / 255.0, 41.0 / 255.0, 56.0 / 255.0, 0.55)
}
/// Light translucent outline.
fn default_outer_color() -> Color {
    Color::new(180.0 / 255.0, 190.0 / 255.0, 210.0 / 255.0, 0.55)
}
/// The dots' pre-config hardcoded colour.
fn default_dot_color() -> Color {
    Color::new(220.0 / 255.0, 220.0 / 255.0, 220.0 / 255.0, 1.0)
}
fn default_placeholder_text() -> String {
    "Enter to remember...".to_string()
}
/// Dim translucent light grey — reads as a hint, not a typed value.
fn default_placeholder_color() -> Color {
    Color::new(200.0 / 255.0, 205.0 / 255.0, 215.0 / 255.0, 0.6)
}
fn default_placeholder_font_family() -> String {
    "Sans".to_string()
}
fn default_placeholder_font_size() -> u32 {
    18
}
fn default_fail_color() -> Color {
    Color::new(180.0 / 255.0, 40.0 / 255.0, 40.0 / 255.0, 0.75)
}
fn default_capslock_color() -> Color {
    Color::new(200.0 / 255.0, 150.0 / 255.0, 30.0 / 255.0, 0.75)
}

#[derive(Debug)]
pub enum ConfigError {
    /// Couldn't find a sensible default path
    /// (neither `$VEILAND_CONFIG` nor `$XDG_CONFIG_HOME` nor `$HOME`).
    NoHomeDir,
    /// File exists but I/O failed (permissions, etc.). NotFound is
    /// not an error here — see `load`'s missing-file behaviour.
    Io(std::io::Error),
    /// File exists, contents are not valid TOML or don't match our
    /// schema. Caller should print this; `toml::de::Error` already
    /// includes line/column context.
    Parse(toml::de::Error),
    /// Schema parsed but post-validation failed (empty name,
    /// duplicate name, etc.)
    Invalid(String),
}

impl std::fmt::Display for ConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ConfigError::NoHomeDir => write!(
                f,
                "no config-file location available (set $VEILAND_CONFIG, \
                $XDG_CONFIG_HOME, or $HOME)"
            ),
            ConfigError::Io(e) => write!(f, "reading config file: {}", e),
            ConfigError::Parse(e) => write!(f, "parsing config file: {}", e),
            ConfigError::Invalid(msg) => write!(f, "invalid config: {}", msg),
        }
    }
}

impl std::error::Error for ConfigError {}

/// Resolve the config-file path, then load it. Missing file is
/// non-fatal: returns `Ok(Config::default())` with an empty plugin
/// list and the caller logs prominently. Malformed file is fatal.
pub fn load() -> Result<Config, ConfigError> {
    let path = resolve_path()?;
    load_from_path(&path)
}

fn resolve_path() -> Result<PathBuf, ConfigError> {
    if let Ok(p) = std::env::var("VEILAND_CONFIG") {
        return Ok(PathBuf::from(p));
    }
    let base = match std::env::var("XDG_CONFIG_HOME") {
        Ok(p) if !p.is_empty() => PathBuf::from(p),
        _ => {
            let home = std::env::var("HOME").map_err(|_| ConfigError::NoHomeDir)?;
            if home.is_empty() {
                return Err(ConfigError::NoHomeDir);
            }
            PathBuf::from(home).join(".config")
        }
    };
    Ok(base.join("veiland").join("config.toml"))
}

fn load_from_path(path: &Path) -> Result<Config, ConfigError> {
    let text = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!(
                "veiland-core: no config file at {:?}; running with zero plugins.",
                path
            );
            eprintln!(
                "veiland-core: write {:?} with [[plugin]] entries to enable plugins.",
                path
            );
            return Ok(Config::default());
        }
        Err(e) => return Err(ConfigError::Io(e)),
    };
    let mut config: Config = toml::from_str(&text).map_err(ConfigError::Parse)?;
    validate(&mut config)?;
    Ok(config)
}

fn validate(config: &mut Config) -> Result<(), ConfigError> {
    validate_password(&mut config.password);

    let mut seen: Vec<&str> = Vec::with_capacity(config.plugins.len());
    for (i, p) in config.plugins.iter().enumerate() {
        if p.name.is_empty() {
            return Err(ConfigError::Invalid(format!(
                "[[plugin]] #{} has empty name",
                i
            )));
        }
        if seen.contains(&p.name.as_str()) {
            return Err(ConfigError::Invalid(format!(
                "duplicate plugin name: {:?}",
                p.name
            )));
        }
        seen.push(&p.name);

        if let Some(r) = &p.region {
            // Soft check: log on implausible coords, don't reject.
            // Resolves Q2 ("clip, don't reject") at the loader layer.
            const MAX_PLAUSIBLE: i32 = 8192;
            let suspicious = r.x.abs() > MAX_PLAUSIBLE
                || r.y.abs() > MAX_PLAUSIBLE
                || r.w > MAX_PLAUSIBLE as u32
                || r.h > MAX_PLAUSIBLE as u32;
            if suspicious {
                eprintln!(
                    "veiland-core: plugin {:?} has implausible region {:?} \
                    (likely a typo; GL will clip it but you probably didn't mean this)",
                    p.name, r
                );
            }
        }

        if let Some(monitors) = &p.monitors
            && monitors.is_empty()
        {
            return Err(ConfigError::Invalid(format!(
                "plugin {:?} has empty monitors list; \
                either omit the field (means 'all outputs') \
                or list at least one output name",
                p.name
            )));
        }
    }
    Ok(())
}

/// Clamp password-indicator fields to safe ranges, logging a
/// warning when a value had to move. Never fatal: out-of-range
/// values from a user config shouldn't lock them out, and the
/// clamped values still produce a usable indicator.
///
/// `x` isn't clamped — any surface-pixel value is meaningful
/// (negative places the dots off the left edge, large places them
/// off the right; both are user errors but neither is dangerous).
/// `y_percent` is clamped to [0, 100] because percentages outside
/// that range have no meaning.
fn validate_password(p: &mut Password) {
    if let Some(y) = p.y_percent {
        let clamped = y.clamp(0, 100);
        if clamped != y {
            eprintln!(
                "veiland-core: [password] y_percent = {} out of range [0, 100]; \
                clamped to {}",
                y, clamped
            );
            p.y_percent = Some(clamped);
        }
    }

    let clamped_diameter = p.dot_diameter.clamp(1, 100);
    if clamped_diameter != p.dot_diameter {
        eprintln!(
            "veiland-core: [password] dot_diameter = {} out of range [1, 100]; \
            clamped to {}",
            p.dot_diameter, clamped_diameter
        );
        p.dot_diameter = clamped_diameter;
    }

    let clamped_spacing = p.dot_spacing.clamp(1, 200);
    if clamped_spacing != p.dot_spacing {
        eprintln!(
            "veiland-core: [password] dot_spacing = {} out of range [1, 200]; \
            clamped to {}",
            p.dot_spacing, clamped_spacing
        );
        p.dot_spacing = clamped_spacing;
    }

    let clamped_max = p.max_dots.clamp(1, 256);
    if clamped_max != p.max_dots {
        eprintln!(
            "veiland-core: [password] max_dots = {} out of range [1, 256]; \
            clamped to {}",
            p.max_dots, clamped_max
        );
        p.max_dots = clamped_max;
    }

    // Box dimensions: same 8192 implausible-size ceiling the plugin
    // region check uses. A box bigger than any real display is a typo.
    const MAX_BOX: u32 = 8192;
    let clamped_bw = p.box_width.clamp(1, MAX_BOX);
    if clamped_bw != p.box_width {
        eprintln!(
            "veiland-core: [password] box_width = {} out of range [1, {}]; \
            clamped to {}",
            p.box_width, MAX_BOX, clamped_bw
        );
        p.box_width = clamped_bw;
    }
    let clamped_bh = p.box_height.clamp(1, MAX_BOX);
    if clamped_bh != p.box_height {
        eprintln!(
            "veiland-core: [password] box_height = {} out of range [1, {}]; \
            clamped to {}",
            p.box_height, MAX_BOX, clamped_bh
        );
        p.box_height = clamped_bh;
    }

    // Outline can't exceed half the (already-clamped) box height, or it
    // would consume the box. 0 is legal (no outline, fill only).
    let max_outline = p.box_height / 2;
    if p.outline_thickness > max_outline {
        eprintln!(
            "veiland-core: [password] outline_thickness = {} exceeds box_height/2 = {}; \
            clamped to {}",
            p.outline_thickness, max_outline, max_outline
        );
        p.outline_thickness = max_outline;
    }

    // rounding: -1 is the "full pill" sentinel, passed through untouched.
    // Any other value clamps to [0, min(box_width, box_height)/2] — a
    // radius larger than the half-extent is geometrically meaningless.
    if p.rounding != -1 {
        let max_radius = (p.box_width.min(p.box_height) / 2) as i32;
        let clamped_r = p.rounding.clamp(0, max_radius);
        if clamped_r != p.rounding {
            eprintln!(
                "veiland-core: [password] rounding = {} out of range [0, {}] \
                (or -1 for full pill); clamped to {}",
                p.rounding, max_radius, clamped_r
            );
            p.rounding = clamped_r;
        }
    }

    // Placeholder font size: a sane pixel range. Text isn't clamped or
    // validated beyond this — any string is fine; an empty one just
    // disables the placeholder at draw time.
    let clamped_ph = p.placeholder_font_size.clamp(1, 512);
    if clamped_ph != p.placeholder_font_size {
        eprintln!(
            "veiland-core: [password] placeholder_font_size = {} out of range [1, 512]; \
            clamped to {}",
            p.placeholder_font_size, clamped_ph
        );
        p.placeholder_font_size = clamped_ph;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test helper: parse + validate from a string fixture, bypassing
    /// the filesystem and path resolution. The I/O paths in `load`
    /// and `resolve_path` are intentionally not unit-tested — they
    /// depend on the runtime env, and the dev-loop `$VEILAND_CONFIG`
    /// override is what we actually use to exercise them by hand.
    fn parse(text: &str) -> Result<Config, ConfigError> {
        let mut config: Config = toml::from_str(text).map_err(ConfigError::Parse)?;
        validate(&mut config)?;
        Ok(config)
    }

    #[test]
    fn happy_path_two_plugins() {
        let text = r#"
            [[plugin]]
            name = "gradient"
            binary = "/path/to/veiland-gradient"
            z_index = 0
            region = { x = 0, y = 0, w = 1920, h = 1080 }

            [[plugin]]
            name = "clock"
            binary = "/path/to/veiland-clock"
            z_index = 10

            [plugin.config]
            timezone = "Europe/Paris"
            format_24h = true
        "#;
        let config = parse(text).expect("happy path should parse");
        assert_eq!(config.plugins.len(), 2);

        assert_eq!(config.plugins[0].name, "gradient");
        assert_eq!(
            config.plugins[0].binary,
            std::path::PathBuf::from("/path/to/veiland-gradient")
        );
        assert_eq!(config.plugins[0].z_index, 0);
        let region = config.plugins[0]
            .region
            .as_ref()
            .expect("first plugin has a region");
        assert_eq!(region.x, 0);
        assert_eq!(region.y, 0);
        assert_eq!(region.w, 1920);
        assert_eq!(region.h, 1080);
        assert!(config.plugins[0].config.is_none());

        assert_eq!(config.plugins[1].name, "clock");
        assert_eq!(config.plugins[1].z_index, 10);
        assert!(
            config.plugins[1].region.is_none(),
            "region is optional; second entry omitted it"
        );
        // The pass-through table is preserved as `toml::Value` and
        // serialised to JSON at spawn time (step 2). Here we just
        // verify it round-tripped into the entry.
        let custom = config.plugins[1]
            .config
            .as_ref()
            .expect("second plugin has a [plugin.config] table");
        let table = custom.as_table().expect("[plugin.config] is a table");
        assert_eq!(
            table.get("timezone").and_then(|v| v.as_str()),
            Some("Europe/Paris")
        );
        assert_eq!(
            table.get("format_24h").and_then(|v| v.as_bool()),
            Some(true)
        );
    }

    #[test]
    fn empty_file_yields_empty_plugin_list() {
        let config = parse("").expect("empty file should parse to empty config");
        assert!(config.plugins.is_empty());
    }

    #[test]
    fn declaration_order_is_preserved() {
        // The host sorts by z_index at spawn time (step 2), with
        // ties broken by config-file order. The loader's job is to
        // preserve that order; the sort happens elsewhere.
        let text = r#"
            [[plugin]]
            name = "first"
            binary = "/a"
            z_index = 10

            [[plugin]]
            name = "second"
            binary = "/b"
            z_index = 5

            [[plugin]]
            name = "third"
            binary = "/c"
            z_index = 10
        "#;
        let config = parse(text).expect("ordering fixture should parse");
        let names: Vec<&str> = config.plugins.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["first", "second", "third"]);
    }

    #[test]
    fn negative_z_index_is_valid() {
        // "Always behind everything" — a wallpaper plugin's natural
        // declaration. i32, not u32, exists for exactly this case.
        let text = r#"
            [[plugin]]
            name = "wallpaper"
            binary = "/x"
            z_index = -100
        "#;
        let config = parse(text).expect("negative z_index is legitimate");
        assert_eq!(config.plugins[0].z_index, -100);
    }

    #[test]
    fn region_is_optional() {
        let text = r#"
            [[plugin]]
            name = "no-region"
            binary = "/x"
            z_index = 0
        "#;
        let config = parse(text).expect("missing region is fine");
        assert!(config.plugins[0].region.is_none());
    }

    #[test]
    fn empty_name_rejected() {
        let text = r#"
            [[plugin]]
            name = ""
            binary = "/x"
            z_index = 0
        "#;
        match parse(text) {
            Err(ConfigError::Invalid(msg)) => {
                assert!(
                    msg.contains("empty name"),
                    "error message should mention empty name, got {:?}",
                    msg
                );
            }
            other => panic!("expected Invalid, got {:?}", other),
        }
    }

    #[test]
    fn duplicate_name_rejected() {
        let text = r#"
            [[plugin]]
            name = "twin"
            binary = "/a"
            z_index = 0

            [[plugin]]
            name = "twin"
            binary = "/b"
            z_index = 1
        "#;
        match parse(text) {
            Err(ConfigError::Invalid(msg)) => {
                assert!(
                    msg.contains("twin"),
                    "error message should name the duplicate, got {:?}",
                    msg
                );
            }
            other => panic!("expected Invalid containing 'twin', got {:?}", other),
        }
    }

    #[test]
    fn malformed_toml_rejected() {
        let text = "this is not toml [[[";
        match parse(text) {
            Err(ConfigError::Parse(_)) => {}
            other => panic!("expected Parse, got {:?}", other),
        }
    }

    #[test]
    fn missing_required_field_rejected() {
        // `name` is required; serde reports this as a parse error
        // (not Invalid), because the schema mismatch happens during
        // deserialization, before `validate` runs.
        let text = r#"
            [[plugin]]
            binary = "/x"
            z_index = 0
        "#;
        match parse(text) {
            Err(ConfigError::Parse(_)) => {}
            other => panic!("expected Parse, got {:?}", other),
        }
    }

    #[test]
    fn wrong_field_type_rejected() {
        // `z_index = "high"` — string where i32 expected. Parse
        // error, not Invalid: same reason as above.
        let text = r#"
            [[plugin]]
            name = "x"
            binary = "/x"
            z_index = "high"
        "#;
        match parse(text) {
            Err(ConfigError::Parse(_)) => {}
            other => panic!("expected Parse, got {:?}", other),
        }
    }

    #[test]
    fn monitors_absent_means_none() {
        // No `monitors` field → field-absent semantics ("every connected
        // output"). The loader represents this as `None`; the spawn
        // matcher in main.rs maps `None` to "match everything".
        let text = r#"
            [[plugin]]
            name = "everywhere"
            binary = "/x"
            z_index = 0
        "#;
        let config = parse(text).expect("absent monitors is fine");
        assert!(config.plugins[0].monitors.is_none());
    }

    #[test]
    fn monitors_with_names_accepted() {
        // A non-empty list round-trips into the typed field. Order is
        // preserved (Vec, not Set) because the spawn matcher walks it
        // linearly and case-sensitivity is exact-match — order doesn't
        // change behaviour but losing it would be a surprising codec.
        let text = r#"
            [[plugin]]
            name = "selective"
            binary = "/x"
            z_index = 0
            monitors = ["DP-1", "HDMI-A-1"]
        "#;
        let config = parse(text).expect("monitors list is fine");
        let monitors = config.plugins[0]
            .monitors
            .as_ref()
            .expect("monitors is Some");
        assert_eq!(monitors, &vec!["DP-1".to_string(), "HDMI-A-1".to_string()]);
    }

    #[test]
    fn password_absent_uses_defaults() {
        // Missing [password] table → Password::default(): no x/y_percent
        // overrides, defaults for the sized fields, and the box on by
        // default with its mockup-derived colours.
        let config = parse("").expect("empty config parses");
        let p = &config.password;
        assert!(p.x.is_none());
        assert!(p.y_percent.is_none());
        assert_eq!(p.dot_diameter, 12);
        assert_eq!(p.dot_spacing, 20);
        assert_eq!(p.max_dots, 32);
        assert!(p.show_box, "box is on by default");
        assert_eq!(p.box_width, 400);
        assert_eq!(p.box_height, 90);
        assert_eq!(p.outline_thickness, 2);
        assert_eq!(p.rounding, -1, "full-pill sentinel");
        assert_eq!(p.dot_color, Color::new(220.0 / 255.0, 220.0 / 255.0, 220.0 / 255.0, 1.0));
        assert_eq!(p.placeholder_text, "Enter to remember...");
        assert_eq!(p.placeholder_font_family, "Sans");
        assert_eq!(p.placeholder_font_size, 18);
    }

    #[test]
    fn password_partial_table_keeps_other_defaults() {
        // Only `x` written; the others should keep their defaults.
        // Proves per-field #[serde(default = "...")] is wired up.
        let text = r#"
            [password]
            x = 800
        "#;
        let config = parse(text).expect("partial password parses");
        assert_eq!(config.password.x, Some(800));
        assert!(config.password.y_percent.is_none());
        assert_eq!(config.password.dot_diameter, 12);
        assert_eq!(config.password.dot_spacing, 20);
        assert_eq!(config.password.max_dots, 32);
    }

    #[test]
    fn password_full_table_roundtrips() {
        let text = r#"
            [password]
            x = 960
            y_percent = 50
            dot_diameter = 16
            dot_spacing = 24
            max_dots = 64
            show_box = true
            box_width = 500
            box_height = 100
            outline_thickness = 3
            rounding = 12
            inner_color = "rgba(10, 20, 30, 0.5)"
            outer_color = "rgba(200, 210, 220, 0.8)"
            dot_color = "rgb(255, 255, 255)"
            placeholder_text = "Type here"
            placeholder_color = "rgba(100, 110, 120, 0.5)"
            placeholder_font_family = "Liberation Sans"
            placeholder_font_size = 22
        "#;
        let config = parse(text).expect("full password parses");
        let p = &config.password;
        assert_eq!(p.x, Some(960));
        assert_eq!(p.y_percent, Some(50));
        assert_eq!(p.dot_diameter, 16);
        assert_eq!(p.dot_spacing, 24);
        assert_eq!(p.max_dots, 64);
        assert!(p.show_box);
        assert_eq!(p.box_width, 500);
        assert_eq!(p.box_height, 100);
        assert_eq!(p.outline_thickness, 3);
        assert_eq!(p.rounding, 12);
        assert_eq!(p.inner_color, Color::new(10.0 / 255.0, 20.0 / 255.0, 30.0 / 255.0, 0.5));
        assert_eq!(p.outer_color, Color::new(200.0 / 255.0, 210.0 / 255.0, 220.0 / 255.0, 0.8));
        assert_eq!(p.dot_color, Color::new(1.0, 1.0, 1.0, 1.0));
        assert_eq!(p.placeholder_text, "Type here");
        assert_eq!(p.placeholder_color, Color::new(100.0 / 255.0, 110.0 / 255.0, 120.0 / 255.0, 0.5));
        assert_eq!(p.placeholder_font_family, "Liberation Sans");
        assert_eq!(p.placeholder_font_size, 22);
    }

    #[test]
    fn password_placeholder_empty_disables() {
        // An empty string is the documented "no placeholder" signal; it
        // must round-trip as empty (the renderer skips drawing it).
        let text = r#"
            [password]
            placeholder_text = ""
        "#;
        let config = parse(text).expect("empty placeholder parses");
        assert_eq!(config.password.placeholder_text, "");
    }

    #[test]
    fn password_show_box_false_roundtrips() {
        let text = r#"
            [password]
            show_box = false
        "#;
        let config = parse(text).expect("show_box=false parses");
        assert!(!config.password.show_box);
    }

    #[test]
    fn password_outline_clamped_to_half_height() {
        // outline can't exceed box_height/2 = 45.
        let text = r#"
            [password]
            box_height = 90
            outline_thickness = 999
        "#;
        let config = parse(text).expect("oversized outline clamps, not rejects");
        assert_eq!(config.password.outline_thickness, 45);
    }

    #[test]
    fn password_box_dims_clamped() {
        let text = r#"
            [password]
            box_width = 99999
            box_height = 0
        "#;
        let config = parse(text).expect("out-of-range box dims clamp, not reject");
        assert_eq!(config.password.box_width, 8192);
        assert_eq!(config.password.box_height, 1);
    }

    #[test]
    fn password_rounding_sentinel_preserved() {
        // -1 is the full-pill sentinel and must pass through unclamped.
        let text = r#"
            [password]
            rounding = -1
        "#;
        let config = parse(text).expect("rounding -1 parses");
        assert_eq!(config.password.rounding, -1);
    }

    #[test]
    fn password_rounding_clamped_to_half_extent() {
        // rounding > min(w,h)/2 = 45 clamps; non-sentinel path.
        let text = r#"
            [password]
            box_width = 400
            box_height = 90
            rounding = 9999
        "#;
        let config = parse(text).expect("oversized rounding clamps");
        assert_eq!(config.password.rounding, 45);
    }

    #[test]
    fn password_bad_color_rejected_at_parse() {
        // A malformed colour string surfaces as a Parse error (it fails
        // during deserialisation, before validate runs).
        let text = r#"
            [password]
            inner_color = "rgba(300, 0, 0, 1.0)"
        "#;
        match parse(text) {
            Err(ConfigError::Parse(_)) => {}
            other => panic!("expected Parse error for bad colour, got {:?}", other),
        }
    }

    #[test]
    fn password_y_percent_clamped_high() {
        // 150 > 100 → clamped to 100 (with a warning we don't capture
        // here — eprintln goes to stderr, not testable cheaply).
        let text = r#"
            [password]
            y_percent = 150
        "#;
        let config = parse(text).expect("out-of-range y_percent clamps, not rejects");
        assert_eq!(config.password.y_percent, Some(100));
    }

    #[test]
    fn password_y_percent_clamped_negative() {
        let text = r#"
            [password]
            y_percent = -10
        "#;
        let config = parse(text).expect("negative y_percent clamps, not rejects");
        assert_eq!(config.password.y_percent, Some(0));
    }

    #[test]
    fn password_dot_diameter_clamped() {
        // dot_diameter = 0 → clamped to 1 (zero would degenerate the
        // shader; one is the smallest meaningful dot).
        let text = r#"
            [password]
            dot_diameter = 0
        "#;
        let config = parse(text).expect("zero diameter clamps, not rejects");
        assert_eq!(config.password.dot_diameter, 1);
    }

    #[test]
    fn password_dot_spacing_clamped_high() {
        let text = r#"
            [password]
            dot_spacing = 99999
        "#;
        let config = parse(text).expect("huge spacing clamps, not rejects");
        assert_eq!(config.password.dot_spacing, 200);
    }

    #[test]
    fn password_max_dots_clamped() {
        let text = r#"
            [password]
            max_dots = 0
        "#;
        let config = parse(text).expect("zero max_dots clamps, not rejects");
        assert_eq!(config.password.max_dots, 1);
    }

    #[test]
    fn color_parses_rgba() {
        assert_eq!(
            parse_rgba("rgba(34, 41, 56, 0.55)").unwrap(),
            Color::new(34.0 / 255.0, 41.0 / 255.0, 56.0 / 255.0, 0.55)
        );
    }

    #[test]
    fn color_parses_rgb_implied_opaque() {
        // rgb() with no alpha → fully opaque.
        assert_eq!(
            parse_rgba("rgb(255, 255, 255)").unwrap(),
            Color::new(1.0, 1.0, 1.0, 1.0)
        );
    }

    #[test]
    fn color_tolerates_whitespace_and_case() {
        assert_eq!(
            parse_rgba("  RGBA( 10 ,20, 30 ,1.0 )  ").unwrap(),
            Color::new(10.0 / 255.0, 20.0 / 255.0, 30.0 / 255.0, 1.0)
        );
    }

    #[test]
    fn color_channel_out_of_range_rejected() {
        // 300 > 255: a typo worth surfacing, not silently clamping.
        let err = parse_rgba("rgba(300, 0, 0, 1.0)").unwrap_err();
        assert!(
            err.contains("out of range"),
            "expected range error, got {:?}",
            err
        );
    }

    #[test]
    fn color_alpha_clamped() {
        // Alpha > 1.0 is obvious "fully opaque" intent → clamp, don't reject.
        assert_eq!(parse_rgba("rgba(0, 0, 0, 1.5)").unwrap(), Color::new(0.0, 0.0, 0.0, 1.0));
        assert_eq!(parse_rgba("rgba(0, 0, 0, -0.2)").unwrap(), Color::new(0.0, 0.0, 0.0, 0.0));
    }

    #[test]
    fn color_garbage_rejected() {
        assert!(parse_rgba("not a color").is_err());
        assert!(parse_rgba("rgba(1, 2)").is_err()); // too few components
        assert!(parse_rgba("rgba(1, 2, 3, 4, 5)").is_err()); // too many
        assert!(parse_rgba("rgba(1, 2, 3").is_err()); // missing paren
        assert!(parse_rgba("rgba(a, b, c)").is_err()); // non-numeric channels
    }

    #[test]
    fn empty_monitors_rejected() {
        // An explicit empty list is ambiguous — did the user mean
        // "no outputs" (then why declare it?) or "I deleted my list
        // and forgot to remove the field"? Reject at load with a
        // message that names the fix.
        let text = r#"
            [[plugin]]
            name = "ambiguous"
            binary = "/x"
            z_index = 0
            monitors = []
        "#;
        match parse(text) {
            Err(ConfigError::Invalid(msg)) => {
                assert!(
                    msg.contains("empty monitors"),
                    "error should mention empty monitors, got {:?}",
                    msg
                );
                assert!(
                    msg.contains("omit the field"),
                    "error should suggest the fix, got {:?}",
                    msg
                );
            }
            other => panic!("expected Invalid, got {:?}", other),
        }
    }
}
