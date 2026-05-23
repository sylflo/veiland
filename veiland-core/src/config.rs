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

#[derive(Debug, Default, Deserialize)]
pub struct Config {
    /// Plugins to spawn, in declaration order. The host sorts by
    /// `z_index` at spawn time; ties keep config-file order.
    #[serde(rename = "plugin", default)]
    pub plugins: Vec<PluginEntry>,
}

#[derive(Debug, Deserialize)]
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

    /// Optional pass-through table. Serialized to JSON and handed
    /// to the plugin via the `VEILAND_PLUGIN_CONFIG` env var.
    /// Plugins that don't care never read it.
    #[serde(default)]
    pub config: Option<toml::Value>,
}

#[derive(Debug, Deserialize)]
pub struct Region {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
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
    let config: Config = toml::from_str(&text).map_err(ConfigError::Parse)?;
    validate(&config)?;
    Ok(config)
}

fn validate(config: &Config) -> Result<(), ConfigError> {
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
    }
    Ok(())
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
        let config: Config = toml::from_str(text).map_err(ConfigError::Parse)?;
        validate(&config)?;
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
}
