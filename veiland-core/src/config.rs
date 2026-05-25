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
    /// Schema-only in M6: the field is parsed so user configs with
    /// `[plugin.config]` tables don't break, but the spawn side
    /// doesn't yet serialise it to JSON or export it via
    /// `VEILAND_PLUGIN_CONFIG`. Wired up when a real plugin needs
    /// it (M7's clock will want a timezone) — at which point this
    /// `#[allow(dead_code)]` goes away.
    #[allow(dead_code)]
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
}

impl Default for Password {
    fn default() -> Self {
        Self {
            x: None,
            y_percent: None,
            dot_diameter: default_dot_diameter(),
            dot_spacing: default_dot_spacing(),
            max_dots: default_max_dots(),
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

        if let Some(monitors) = &p.monitors {
            if monitors.is_empty() {
                return Err(ConfigError::Invalid(format!(
                    "plugin {:?} has empty monitors list; \
                    either omit the field (means 'all outputs') \
                    or list at least one output name",
                    p.name
                )));
            }
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
        // overrides, defaults for the three sized fields.
        let config = parse("").expect("empty config parses");
        assert!(config.password.x.is_none());
        assert!(config.password.y_percent.is_none());
        assert_eq!(config.password.dot_diameter, 12);
        assert_eq!(config.password.dot_spacing, 20);
        assert_eq!(config.password.max_dots, 32);
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
        "#;
        let config = parse(text).expect("full password parses");
        assert_eq!(config.password.x, Some(960));
        assert_eq!(config.password.y_percent, Some(50));
        assert_eq!(config.password.dot_diameter, 16);
        assert_eq!(config.password.dot_spacing, 24);
        assert_eq!(config.password.max_dots, 64);
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
