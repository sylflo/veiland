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

    /// The plugin binary to spawn. A bare name (no `/`, e.g.
    /// `veiland-clock`) is resolved by the core: first beside the locker
    /// itself, then on `$PATH` (see `plugin::host_spawn::resolve_binary`).
    /// A value containing a `/` (absolute `/usr/bin/veiland-clock` or
    /// relative `target/debug/veiland-clock`) is used verbatim — the
    /// escape hatch for dev builds. No tilde expansion. Spawn / resolution
    /// failure is logged at runtime and leaves that plugin's layer empty.
    pub binary: PathBuf,

    /// Lower = behind. Ties broken by config-file order (stable sort).
    /// Negative values are legitimate ("always behind everything").
    pub z_index: i32,

    /// Optional. `None` means "fill the whole lock surface" — the
    /// default is resolved at Configure time, not here, because we
    /// don't know the surface size at config-load time. When present it
    /// is one of two mutually-exclusive forms (explicit pixels, or an
    /// anchored fraction-of-surface box); see `RegionSpec`.
    #[serde(default)]
    pub region: Option<RegionSpec>,

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

/// A resolved region in absolute surface pixels — the form the
/// compositor consumes. Both `RegionSpec` variants resolve into this
/// (the pixel form trivially, the anchored form against the live
/// surface size). `region.rs::region_to_clip_rect` and `configure_dims`
/// operate on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// Horizontal alignment of an anchored region against the surface —
/// hyprlock's `halign` vocabulary (screen-relative, no container).
/// `Center` ignores the horizontal margin.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HAlign {
    Left,
    Center,
    Right,
}

/// Vertical alignment of an anchored region against the surface —
/// hyprlock's `valign` vocabulary. `Center` ignores the vertical margin.
#[derive(Clone, Copy, Debug, PartialEq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum VAlign {
    Top,
    Center,
    Bottom,
}

/// An anchored region: a box sized as a fraction of the surface and
/// aligned to an edge/corner, resolved to pixels host-side once the
/// surface size is known. This is the resolution-independent form —
/// `width = 0.06` is 6% of surface width on any monitor, so an anchored
/// widget looks the same on 1080p and 4K (the fraction-of-surface model
/// veiland's `label`/`clock` plugins already use for text). The margins
/// inset from the aligned edge on their axis; a centred axis ignores
/// its margin. In TOML, `margin` is a shorthand that sets both axes;
/// `margin_x`/`margin_y` override it per axis.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AnchorSpec {
    pub halign: HAlign,
    pub valign: VAlign,
    /// Fraction of surface width, `0.0 < width <= 1.0` (clamped).
    pub width: f32,
    /// Fraction of surface height, `0.0 < height <= 1.0` (clamped).
    pub height: f32,
    /// Horizontal inset from the aligned edge as a fraction of surface
    /// width, `0.0..=1.0` (clamped).
    pub margin_x: f32,
    /// Vertical inset from the aligned edge as a fraction of surface
    /// height, `0.0..=1.0` (clamped).
    pub margin_y: f32,
}

/// A plugin's `[plugin.region]`, in one of two mutually-exclusive forms.
/// Kept unresolved through config load because the anchored form needs
/// the surface size (unknown at load); `resolve` turns either form into
/// a pixel `Region` at Configure time.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum RegionSpec {
    /// Explicit absolute pixels — the escape hatch, `{ x, y, w, h }`.
    Pixels(Region),
    /// Anchored fraction-of-surface box — `{ halign, valign, width,
    /// height, margin / margin_x / margin_y }`.
    Anchored(AnchorSpec),
}

impl RegionSpec {
    /// Resolve to a pixel `Region` against a concrete surface size.
    /// `Pixels` passes through; `Anchored` computes `x, y, w, h` from
    /// the fractions, size, margin, and alignment. Never panics: all
    /// arithmetic is on clamped fractions and saturates at the surface
    /// bounds. Called at Configure time (spawn + resize resend), once
    /// per output, so a mode change re-anchors correctly.
    pub fn resolve(&self, surface_w: u32, surface_h: u32) -> Region {
        match self {
            RegionSpec::Pixels(r) => *r,
            RegionSpec::Anchored(a) => a.resolve(surface_w, surface_h),
        }
    }
}

impl AnchorSpec {
    /// Fractions → pixels against the surface. `w`/`h` are rounded and
    /// clamped to at least 1px and at most the surface extent; the
    /// margin inset is applied from the aligned edge and the position
    /// is clamped so the box stays on-screen (overflow can't push it
    /// negative). Pure integer/float math, no panics.
    fn resolve(&self, surface_w: u32, surface_h: u32) -> Region {
        let sw = surface_w as f32;
        let sh = surface_h as f32;

        // Box size, clamped to [1, surface]. The fractions are already
        // clamped to (0, 1] at validate time, but clamp again here so a
        // direct call (tests) can't produce a degenerate size.
        let w = ((self.width * sw).round() as i64).clamp(1, surface_w.max(1) as i64) as u32;
        let h = ((self.height * sh).round() as i64).clamp(1, surface_h.max(1) as i64) as u32;

        // Margin inset in pixels, per axis (fraction of that axis).
        let mx = (self.margin_x * sw).round() as i64;
        let my = (self.margin_y * sh).round() as i64;

        // Free space along each axis after placing the box.
        let free_x = surface_w as i64 - w as i64;
        let free_y = surface_h as i64 - h as i64;

        // x from halign: left = margin; right = free - margin; center =
        // free/2 (margin ignored). Clamp to [0, free] so a margin larger
        // than the free space can't push the box off-screen.
        let x = match self.halign {
            HAlign::Left => mx.clamp(0, free_x.max(0)),
            HAlign::Right => (free_x - mx).clamp(0, free_x.max(0)),
            HAlign::Center => (free_x / 2).max(0),
        };
        let y = match self.valign {
            VAlign::Top => my.clamp(0, free_y.max(0)),
            VAlign::Bottom => (free_y - my).clamp(0, free_y.max(0)),
            VAlign::Center => (free_y / 2).max(0),
        };

        Region {
            x: x as i32,
            y: y as i32,
            w,
            h,
        }
    }
}

/// Intermediate all-optional shape `[plugin.region]` deserialises into,
/// so we can decide which of the two mutually-exclusive forms was
/// written and produce a clear error on a mix or an incomplete form.
/// `serde(deny_unknown_fields)` catches typos like `witdh`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawRegion {
    x: Option<i32>,
    y: Option<i32>,
    w: Option<u32>,
    h: Option<u32>,
    halign: Option<HAlign>,
    valign: Option<VAlign>,
    width: Option<f32>,
    height: Option<f32>,
    margin: Option<f32>,
    margin_x: Option<f32>,
    margin_y: Option<f32>,
}

impl<'de> serde::Deserialize<'de> for RegionSpec {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = RawRegion::deserialize(deserializer)?;

        let has_pixels = raw.x.is_some() || raw.y.is_some() || raw.w.is_some() || raw.h.is_some();
        let has_anchor = raw.halign.is_some()
            || raw.valign.is_some()
            || raw.width.is_some()
            || raw.height.is_some()
            || raw.margin.is_some()
            || raw.margin_x.is_some()
            || raw.margin_y.is_some();

        // Mutually exclusive: mixing the two forms is a mistake, not a
        // precedence question. Reject loudly rather than silently pick.
        if has_pixels && has_anchor {
            return Err(serde::de::Error::custom(
                "region mixes the pixel form (x/y/w/h) with the anchored form \
                 (halign/valign/width/height/margin/margin_x/margin_y); \
                 use one or the other",
            ));
        }

        if has_anchor {
            // width/height are required for the anchored form; halign/
            // valign/margin default (centre / centre / 0).
            let width = raw.width.ok_or_else(|| {
                serde::de::Error::custom(
                    "anchored region needs `width` (a fraction of the surface)",
                )
            })?;
            let height = raw.height.ok_or_else(|| {
                serde::de::Error::custom(
                    "anchored region needs `height` (a fraction of the surface)",
                )
            })?;
            // `margin` sets both axes; `margin_x`/`margin_y` override
            // their axis (so `margin = 0.03, margin_y = 0` reads as
            // "3% inset, but flush on the vertical axis").
            let margin = raw.margin.unwrap_or(0.0);
            Ok(RegionSpec::Anchored(AnchorSpec {
                halign: raw.halign.unwrap_or(HAlign::Center),
                valign: raw.valign.unwrap_or(VAlign::Center),
                width,
                height,
                margin_x: raw.margin_x.unwrap_or(margin),
                margin_y: raw.margin_y.unwrap_or(margin),
            }))
        } else {
            // Pixel form: all four required (matches the pre-anchor
            // schema, where `Region` had no optional fields).
            let missing = |f: &str| {
                serde::de::Error::custom(format!("pixel region needs `{}` (an integer)", f))
            };
            Ok(RegionSpec::Pixels(Region {
                x: raw.x.ok_or_else(|| missing("x"))?,
                y: raw.y.ok_or_else(|| missing("y"))?,
                w: raw.w.ok_or_else(|| missing("w"))?,
                h: raw.h.ok_or_else(|| missing("h"))?,
            }))
        }
    }
}

/// An RGBA colour, parsed from a CSS-style `rgba(r, g, b, a)` string.
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

/// Parse a CSS-style `rgba(r, g, b, a)` / `rgb(r, g, b)` colour string.
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
    let inner = inner
        .strip_suffix(')')
        .ok_or_else(|| format!("colour {:?} is missing its closing ')'", s))?;

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
            return Err(format!("colour channel {} out of range [0, 255]", v));
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
/// non-fatal: returns the compiled-in default scene (see
/// `default_scene`). Malformed file is fatal.
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
                "veiland-core: no config file at {:?}; using the default scene \
                (raymarched tunnel).",
                path
            );
            eprintln!(
                "veiland-core: to customise it, copy <datadir>/config.example.toml \
                (e.g. /usr/share/veiland/config.example.toml) to {:?} and edit.",
                path
            );
            return default_scene();
        }
        Err(e) => return Err(ConfigError::Io(e)),
    };
    let mut config: Config = toml::from_str(&text).map_err(ConfigError::Parse)?;
    validate(&mut config)?;
    Ok(config)
}

/// The scene veiland renders when the user has no config file: the
/// raymarcher scene from the README's hero shot, compiled into the binary.
///
/// It is embedded rather than read from `<prefix>/share/veiland/` because
/// the install prefix isn't knowable at build time — `/usr/share` is right
/// for the .deb/.rpm/PKGBUILD and wrong for Nix, which puts the package in
/// the store. The scene renders procedurally and references nothing on
/// disk, so a `cargo run` dev build shows exactly what an installed
/// package does.
fn default_scene() -> Result<Config, ConfigError> {
    const DEFAULT: &str = include_str!("default-scene.toml");

    // Validated like any user config: the TOML is ours, but a typo in it
    // should fail loudly at startup rather than produce a subtly broken
    // lock screen. `cargo test` catches it earlier still — see
    // `default_scene_parses_and_validates`.
    let mut config: Config = toml::from_str(DEFAULT).map_err(ConfigError::Parse)?;
    validate(&mut config)?;
    Ok(config)
}

fn validate(config: &mut Config) -> Result<(), ConfigError> {
    validate_password(&mut config.password);

    // Owned names, not `&str`, so the loop can take `&mut p` to clamp
    // anchored-region fractions in place without a borrow conflict.
    // Validation runs once at startup over a handful of plugins, so the
    // clones are free.
    let mut seen: Vec<String> = Vec::with_capacity(config.plugins.len());
    for (i, p) in config.plugins.iter_mut().enumerate() {
        if p.name.is_empty() {
            return Err(ConfigError::Invalid(format!(
                "[[plugin]] #{} has empty name",
                i
            )));
        }
        if seen.contains(&p.name) {
            return Err(ConfigError::Invalid(format!(
                "duplicate plugin name: {:?}",
                p.name
            )));
        }
        seen.push(p.name.clone());

        match &mut p.region {
            Some(RegionSpec::Pixels(r)) => {
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
            // Anchored fractions are clamped, not rejected — same
            // clip-don't-reject stance as pixel regions. width/height
            // must be a positive fraction of the surface (0, 1]; margin
            // is [0, 1]. A value outside the range is a typo we correct
            // with a warning rather than lock the user out over.
            Some(RegionSpec::Anchored(a)) => {
                clamp_anchor_fraction(&p.name, "width", &mut a.width, 1e-4, 1.0);
                clamp_anchor_fraction(&p.name, "height", &mut a.height, 1e-4, 1.0);
                clamp_anchor_fraction(&p.name, "margin_x", &mut a.margin_x, 0.0, 1.0);
                clamp_anchor_fraction(&p.name, "margin_y", &mut a.margin_y, 0.0, 1.0);
            }
            None => {}
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

/// Clamp one anchored-region fraction into `[lo, hi]`, warning if it
/// moved. Same clip-don't-reject stance as pixel regions: a bad value
/// is a typo we correct, never a reason to fail the locker start. NaN
/// is treated as out-of-range and pulled to `lo` (a NaN fraction would
/// otherwise produce a NaN pixel size at resolve time).
fn clamp_anchor_fraction(plugin: &str, field: &str, value: &mut f32, lo: f32, hi: f32) {
    let clamped = if value.is_nan() {
        lo
    } else {
        value.clamp(lo, hi)
    };
    if clamped != *value {
        eprintln!(
            "veiland-core: plugin {:?} region {} = {} out of range [{}, {}]; clamped to {}",
            plugin, field, value, lo, hi, clamped
        );
        *value = clamped;
    }
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
        match region {
            RegionSpec::Pixels(r) => {
                assert_eq!(r.x, 0);
                assert_eq!(r.y, 0);
                assert_eq!(r.w, 1920);
                assert_eq!(r.h, 1080);
            }
            other => panic!("expected a pixel region, got {:?}", other),
        }
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
        assert_eq!(
            p.dot_color,
            Color::new(220.0 / 255.0, 220.0 / 255.0, 220.0 / 255.0, 1.0)
        );
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
        assert_eq!(
            p.inner_color,
            Color::new(10.0 / 255.0, 20.0 / 255.0, 30.0 / 255.0, 0.5)
        );
        assert_eq!(
            p.outer_color,
            Color::new(200.0 / 255.0, 210.0 / 255.0, 220.0 / 255.0, 0.8)
        );
        assert_eq!(p.dot_color, Color::new(1.0, 1.0, 1.0, 1.0));
        assert_eq!(p.placeholder_text, "Type here");
        assert_eq!(
            p.placeholder_color,
            Color::new(100.0 / 255.0, 110.0 / 255.0, 120.0 / 255.0, 0.5)
        );
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
        assert_eq!(
            parse_rgba("rgba(0, 0, 0, 1.5)").unwrap(),
            Color::new(0.0, 0.0, 0.0, 1.0)
        );
        assert_eq!(
            parse_rgba("rgba(0, 0, 0, -0.2)").unwrap(),
            Color::new(0.0, 0.0, 0.0, 0.0)
        );
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
    fn default_scene_parses_and_validates() {
        // The scene compiled into the binary. This is the test that catches
        // a typo in default-scene.toml at `cargo test` time rather than on a
        // user's lock screen.
        let config = default_scene().expect("embedded default scene must parse and validate");
        let names: Vec<&str> = config.plugins.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["raymarcher"]);
        // The [password] table is deliberate styling, not defaults; assert
        // one non-default field so a dropped table shows up here rather
        // than as a mis-styled pill on someone's lock screen.
        assert_eq!(config.password.y_percent, Some(68));
    }

    #[test]
    fn missing_config_file_yields_default_scene() {
        // The behaviour this whole change exists for: no config file means
        // the default scene, not an empty plugin list.
        let missing = std::path::Path::new("/nonexistent/veiland/config.toml");
        let config = load_from_path(missing).expect("a missing config file is not an error");
        assert!(
            !config.plugins.is_empty(),
            "missing config should fall back to the default scene, not zero plugins"
        );
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

    // ---- anchored regions (Part B) --------------------------------------

    fn only_region(text: &str) -> Result<RegionSpec, ConfigError> {
        // Parse a single plugin with a [plugin.region] and return its spec.
        let full = format!(
            "[[plugin]]\nname = \"x\"\nbinary = \"/x\"\nz_index = 0\n{}",
            text
        );
        let config = parse(&full)?;
        Ok(config.plugins[0].region.expect("region should be present"))
    }

    #[test]
    fn pixel_region_parses_to_pixels_variant() {
        let spec = only_region("region = { x = 10, y = 20, w = 300, h = 80 }")
            .expect("pixel region parses");
        assert_eq!(
            spec,
            RegionSpec::Pixels(Region {
                x: 10,
                y: 20,
                w: 300,
                h: 80
            })
        );
    }

    #[test]
    fn anchored_region_parses_with_defaults() {
        // Only width/height are required; halign/valign default to centre,
        // margin to 0.
        let spec =
            only_region("region = { width = 0.06, height = 0.1 }").expect("anchored region parses");
        assert_eq!(
            spec,
            RegionSpec::Anchored(AnchorSpec {
                halign: HAlign::Center,
                valign: VAlign::Center,
                width: 0.06,
                height: 0.1,
                margin_x: 0.0,
                margin_y: 0.0,
            })
        );
    }

    #[test]
    fn anchored_region_full_roundtrips() {
        let spec = only_region(
            "region = { halign = \"right\", valign = \"top\", \
             width = 0.06, height = 0.1, margin = 0.02 }",
        )
        .expect("full anchored region parses");
        assert_eq!(
            spec,
            RegionSpec::Anchored(AnchorSpec {
                halign: HAlign::Right,
                valign: VAlign::Top,
                width: 0.06,
                height: 0.1,
                margin_x: 0.02,
                margin_y: 0.02,
            })
        );
    }

    #[test]
    fn margin_shorthand_with_per_axis_override() {
        // `margin` fills both axes; a per-axis key overrides its axis
        // only. Here: 3% horizontal inset, flush vertically.
        let spec = only_region(
            "region = { halign = \"left\", valign = \"bottom\", \
             width = 0.26, height = 0.12, margin = 0.03, margin_y = 0.0 }",
        )
        .expect("shorthand + override parses");
        assert_eq!(
            spec,
            RegionSpec::Anchored(AnchorSpec {
                halign: HAlign::Left,
                valign: VAlign::Bottom,
                width: 0.26,
                height: 0.12,
                margin_x: 0.03,
                margin_y: 0.0,
            })
        );
    }

    #[test]
    fn per_axis_margins_without_shorthand() {
        // margin_x alone, no `margin`: the unset axis defaults to 0.
        let spec = only_region("region = { width = 0.1, height = 0.1, margin_x = 0.05 }")
            .expect("per-axis margin parses");
        assert_eq!(
            spec,
            RegionSpec::Anchored(AnchorSpec {
                halign: HAlign::Center,
                valign: VAlign::Center,
                width: 0.1,
                height: 0.1,
                margin_x: 0.05,
                margin_y: 0.0,
            })
        );
    }

    #[test]
    fn mixing_pixel_and_anchor_forms_rejected() {
        // The two forms are mutually exclusive; a mix is a Parse error
        // (it fails in the custom Deserialize, before validate runs).
        let text = r#"
            [[plugin]]
            name = "x"
            binary = "/x"
            z_index = 0
            region = { x = 10, y = 20, halign = "right", width = 0.1, height = 0.1 }
        "#;
        match parse(text) {
            Err(ConfigError::Parse(e)) => {
                assert!(
                    e.to_string().contains("mixes"),
                    "error should explain the mix, got {:?}",
                    e.to_string()
                );
            }
            other => panic!("expected Parse error for mixed region, got {:?}", other),
        }

        // A per-axis margin key alone also marks the anchored form, so
        // it can't ride along with pixel coordinates either.
        let text = r#"
            [[plugin]]
            name = "x"
            binary = "/x"
            z_index = 0
            region = { x = 10, y = 20, w = 100, h = 50, margin_y = 0.1 }
        "#;
        assert!(
            matches!(parse(text), Err(ConfigError::Parse(_))),
            "margin_y with pixel coordinates should be a mix error"
        );
    }

    #[test]
    fn anchored_region_missing_size_rejected() {
        // halign present but width/height absent → incomplete anchored form.
        let text = r#"
            [[plugin]]
            name = "x"
            binary = "/x"
            z_index = 0
            region = { halign = "right", valign = "top" }
        "#;
        match parse(text) {
            Err(ConfigError::Parse(e)) => {
                assert!(
                    e.to_string().contains("width"),
                    "error should mention the missing width, got {:?}",
                    e.to_string()
                );
            }
            other => panic!("expected Parse error for sizeless anchor, got {:?}", other),
        }
    }

    #[test]
    fn region_unknown_field_rejected() {
        // deny_unknown_fields catches a typo like `witdh`.
        let text = r#"
            [[plugin]]
            name = "x"
            binary = "/x"
            z_index = 0
            region = { witdh = 0.1, height = 0.1 }
        "#;
        assert!(
            matches!(parse(text), Err(ConfigError::Parse(_))),
            "a misspelled region field should be a parse error"
        );
    }

    #[test]
    fn bad_halign_keyword_rejected() {
        let text = r#"
            [[plugin]]
            name = "x"
            binary = "/x"
            z_index = 0
            region = { halign = "rihgt", width = 0.1, height = 0.1 }
        "#;
        assert!(
            matches!(parse(text), Err(ConfigError::Parse(_))),
            "an invalid halign keyword should be a parse error"
        );
    }

    #[test]
    fn anchored_fractions_clamped_not_rejected() {
        // width > 1 and margin < 0 are typos we clamp, not reject. The
        // negative shorthand lands in both per-axis margins; both clamp.
        let spec = only_region("region = { width = 1.5, height = 0.1, margin = -0.2 }")
            .expect("out-of-range fractions clamp, not reject");
        match spec {
            RegionSpec::Anchored(a) => {
                assert_eq!(a.width, 1.0, "width clamped to 1.0");
                assert_eq!(a.margin_x, 0.0, "negative margin_x clamped to 0");
                assert_eq!(a.margin_y, 0.0, "negative margin_y clamped to 0");
            }
            other => panic!("expected anchored, got {:?}", other),
        }
    }

    #[test]
    fn resolve_pixels_passes_through() {
        let spec = RegionSpec::Pixels(Region {
            x: 10,
            y: 20,
            w: 300,
            h: 80,
        });
        // Independent of surface size.
        assert_eq!(
            spec.resolve(1920, 1080),
            Region {
                x: 10,
                y: 20,
                w: 300,
                h: 80
            }
        );
        assert_eq!(
            spec.resolve(3840, 2160),
            Region {
                x: 10,
                y: 20,
                w: 300,
                h: 80
            }
        );
    }

    #[test]
    fn resolve_top_right_corner() {
        // 6% x 10% box, anchored top-right, 2% margin. On 1920x1080:
        // w = round(0.06*1920) = 115, h = round(0.10*1080) = 108
        // mx = round(0.02*1920) = 38, my = round(0.02*1080) = 22
        // x = (1920-115) - 38 = 1767 ; y = 22
        let a = AnchorSpec {
            halign: HAlign::Right,
            valign: VAlign::Top,
            width: 0.06,
            height: 0.10,
            margin_x: 0.02,
            margin_y: 0.02,
        };
        assert_eq!(
            a.resolve(1920, 1080),
            Region {
                x: 1767,
                y: 22,
                w: 115,
                h: 108
            }
        );
    }

    #[test]
    fn resolve_is_resolution_independent_in_fractions() {
        // The SAME anchored spec on 1080p and 4K produces a box that is
        // the same FRACTION of each surface and hugs the same corner —
        // the whole point of the fractional model. Check the top-right
        // inset ratio matches on both.
        let a = AnchorSpec {
            halign: HAlign::Right,
            valign: VAlign::Top,
            width: 0.06,
            height: 0.10,
            margin_x: 0.02,
            margin_y: 0.02,
        };
        let r1 = a.resolve(1920, 1080);
        let r2 = a.resolve(3840, 2160);
        // 4K is exactly 2x 1080p, so every resolved pixel value doubles —
        // within 1px, since each axis rounds independently (0.02*1920=38.4
        // rounds to 38, but 0.02*3840=76.8 rounds to 77, not 76).
        let near_double = |a: i64, b: i64| (a - b * 2).abs() <= 1;
        assert!(near_double(r2.w as i64, r1.w as i64), "w should ~double");
        assert!(near_double(r2.h as i64, r1.h as i64), "h should ~double");
        // Right-edge inset (surface_w - (x + w)) also ~doubles: the box
        // hugs the same corner at the same fractional inset on both.
        let inset1 = 1920 - (r1.x + r1.w as i32);
        let inset2 = 3840 - (r2.x + r2.w as i32);
        assert!(
            near_double(inset2 as i64, inset1 as i64),
            "right inset should ~double: 1080p={}, 4K={}",
            inset1,
            inset2
        );
    }

    #[test]
    fn resolve_top_right_on_1440p() {
        // 2560x1440 is not a clean multiple of 1080p, so this exercises
        // the rounding on a real third resolution (the user runs 1080p +
        // 4K; 1440p is the in-between case). width=0.06, height=0.10,
        // margin=0.02, top-right:
        // w = round(0.06*2560) = round(153.6) = 154
        // h = round(0.10*1440) = 144
        // mx = round(0.02*2560) = round(51.2) = 51 ; my = round(0.02*1440) = round(28.8) = 29
        // free_x = 2560-154 = 2406 ; x = 2406-51 = 2355 ; y = 29
        let a = AnchorSpec {
            halign: HAlign::Right,
            valign: VAlign::Top,
            width: 0.06,
            height: 0.10,
            margin_x: 0.02,
            margin_y: 0.02,
        };
        assert_eq!(
            a.resolve(2560, 1440),
            Region {
                x: 2355,
                y: 29,
                w: 154,
                h: 144
            }
        );
    }

    #[test]
    fn resolve_center_ignores_margin() {
        // A centred axis centres the box and ignores the margin.
        let a = AnchorSpec {
            halign: HAlign::Center,
            valign: VAlign::Center,
            width: 0.5,
            height: 0.5,
            margin_x: 0.3, // deliberately large; must be ignored
            margin_y: 0.3,
        };
        let r = a.resolve(1000, 1000);
        // w = h = 500, centred → x = y = (1000-500)/2 = 250.
        assert_eq!(
            r,
            Region {
                x: 250,
                y: 250,
                w: 500,
                h: 500
            }
        );
    }

    #[test]
    fn resolve_bottom_left_corner() {
        let a = AnchorSpec {
            halign: HAlign::Left,
            valign: VAlign::Bottom,
            width: 0.1,
            height: 0.1,
            margin_x: 0.05,
            margin_y: 0.05,
        };
        // On 1000x1000: w=h=100, mx=my=50.
        // x = 50 (left + margin) ; y = (1000-100) - 50 = 850.
        assert_eq!(
            a.resolve(1000, 1000),
            Region {
                x: 50,
                y: 850,
                w: 100,
                h: 100
            }
        );
    }

    #[test]
    fn resolve_per_axis_margins_are_independent() {
        // margin_x insets from the left, margin_y = 0 leaves the box
        // flush with the bottom — the chip-row case a single scalar
        // couldn't express.
        let a = AnchorSpec {
            halign: HAlign::Left,
            valign: VAlign::Bottom,
            width: 0.1,
            height: 0.1,
            margin_x: 0.05,
            margin_y: 0.0,
        };
        // On 1000x1000: w=h=100, mx=50, my=0.
        // x = 50 ; y = (1000-100) - 0 = 900.
        assert_eq!(
            a.resolve(1000, 1000),
            Region {
                x: 50,
                y: 900,
                w: 100,
                h: 100
            }
        );
    }

    #[test]
    fn resolve_margin_overflow_clamps_on_screen() {
        // A margin larger than the free space must not push the box
        // off-screen (negative x). Box fills the whole width (free_x=0),
        // any margin clamps x to 0.
        let a = AnchorSpec {
            halign: HAlign::Right,
            valign: VAlign::Top,
            width: 1.0,
            height: 0.1,
            margin_x: 0.2,
            margin_y: 0.2,
        };
        let r = a.resolve(1000, 1000);
        assert_eq!(r.x, 0, "full-width box clamps x to 0 despite the margin");
        assert_eq!(r.w, 1000);
    }
}
