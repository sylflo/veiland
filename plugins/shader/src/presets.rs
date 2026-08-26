// SPDX-License-Identifier: GPL-3.0-or-later

//! Embedded shader presets, baked into the binary with `include_str!`.
//!
//! Each preset is an original GPL-3.0-or-later fragment shader written for
//! veiland, in Shadertoy convention (a `mainImage` function). The preamble
//! is prepended at compile time by `preamble::assemble`, so these files
//! contain only the shader body.
//!
//! Preset *names* are a stable interface: user configs reference them, so
//! renaming one breaks configs. Treat the names here like CLI flags.

/// One embedded preset: its config name and its GLSL body.
struct Preset {
    name: &'static str,
    body: &'static str,
}

/// The preset table. Add rows to add presets; never rename or remove a row
/// without treating it as a breaking change.
const PRESETS: &[Preset] = &[
    Preset {
        name: "nebula",
        body: include_str!("presets/nebula.frag"),
    },
    Preset {
        name: "deepfield",
        body: include_str!("presets/deepfield.frag"),
    },
    Preset {
        name: "starfield",
        body: include_str!("presets/starfield.frag"),
    },
    Preset {
        name: "moon",
        body: include_str!("presets/moon.frag"),
    },
    Preset {
        name: "meteor",
        body: include_str!("presets/meteor.frag"),
    },
    Preset {
        name: "warp",
        body: include_str!("presets/warp.frag"),
    },
    Preset {
        name: "plasma",
        body: include_str!("presets/plasma.frag"),
    },
];

/// Look up a preset body by name. `None` if no preset has that name.
pub fn get(name: &str) -> Option<&'static str> {
    PRESETS.iter().find(|p| p.name == name).map(|p| p.body)
}

/// Comma-separated list of valid preset names, for the unknown-preset log.
pub fn names() -> String {
    PRESETS
        .iter()
        .map(|p| p.name)
        .collect::<Vec<_>>()
        .join(", ")
}
