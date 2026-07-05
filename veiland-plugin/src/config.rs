// SPDX-License-Identifier: GPL-3.0-or-later

use serde::de::DeserializeOwned;

pub(crate) fn parse_config<C: DeserializeOwned + Default>(
    raw: Option<&str>,
    plugin_name: &str,
) -> C {
    match raw {
        Some(s) => match serde_json::from_str::<C>(s) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "veiland-{}: failed to parse VEILAND_PLUGIN_CONFIG as JSON: {} \
                     — falling back to defaults",
                    plugin_name, e
                );
                C::default()
            }
        },
        None => {
            eprintln!(
                "veiland-{}: VEILAND_PLUGIN_CONFIG unset — using defaults",
                plugin_name
            );
            C::default()
        }
    }
}

pub fn load_config<C: DeserializeOwned + Default>(plugin_name: &str) -> C {
    let raw = std::env::var("VEILAND_PLUGIN_CONFIG").ok();
    parse_config(raw.as_deref(), plugin_name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, PartialEq, Deserialize)]
    struct T {
        #[serde(default)]
        x: u32,
    }
    impl Default for T {
        fn default() -> Self {
            T { x: 99 }
        }
    }

    #[test]
    fn valid_json_parses() {
        let c: T = parse_config(Some(r#"{"x": 7}"#), "test");
        assert_eq!(c.x, 7);
    }

    #[test]
    fn garbage_json_falls_back() {
        let c: T = parse_config(Some("not json"), "test");
        assert_eq!(c, T::default());
    }

    #[test]
    fn missing_env_falls_back() {
        let c: T = parse_config(None, "test");
        assert_eq!(c, T::default());
    }
}
