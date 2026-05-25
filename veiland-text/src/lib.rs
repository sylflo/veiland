// SPDX-License-Identifier: GPL-3.0-or-later

//! Text rendering for veiland plugins. See `docs/m10-plan.md`.
//!
//! M10 step 4: `FontContext` now plumbs a GPU glyph atlas. The atlas
//! itself materializes lazily on the first `Label::render` call (step 5)
//! when a live GL context exists — `FontContext::new()` stays
//! GL-context-free so plugin startup order remains forgiving.

#![deny(unsafe_code)]

mod atlas;

use cosmic_text::{FontSystem, SwashCache};

use atlas::Atlas;

/// Per-plugin-process owner of the font database and glyph rasterization
/// cache. Constructed once at plugin startup; reused across every frame.
///
/// Eager construction: `new()` scans system fonts via fontdb (cosmic-text
/// uses fontdb under the hood). This is ~30–100ms on a cold cache,
/// depending on how many fonts the user has installed. Acceptable for a
/// plugin that runs the whole session; revisit if a black first-frame
/// becomes visible.
///
/// Errors are deliberately absent. cosmic-text falls back to an empty
/// database if fontconfig has nothing to offer; downstream rendering of
/// missing-font text will produce tofu (`□`) rather than crash the
/// plugin — correct behaviour for a lockscreen helper.
pub struct FontContext {
    font_system: FontSystem,
    swash_cache: SwashCache,
    /// Lazily initialized on the first `Label::render` call (step 5);
    /// `None` until then so plugins without a GL context yet (e.g.
    /// during their own startup before the first FrameDone) can still
    /// construct a `FontContext`.
    atlas: Option<Atlas>,
}

impl FontContext {
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            atlas: None,
        }
    }
}

impl Default for FontContext {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Constructing a FontContext on a machine with no fonts is technically
    /// valid (cosmic-text returns an empty database), but renders nothing
    /// useful. This test catches the "dev box has no fonts installed"
    /// case at `cargo test` time rather than at "demo plugin shows blank
    /// screen" time. If it fails on NixOS, the dev shell needs fontconfig
    /// + a font package (`noto-fonts` or equivalent) — see `docs/m10-plan.md` Q6.
    #[test]
    fn font_context_finds_at_least_one_system_font() {
        let ctx = FontContext::new();
        let font_count = ctx.font_system.db().len();
        assert!(
            font_count > 0,
            "fontdb found zero fonts — fontconfig/system-font integration is broken; \
             see docs/m10-plan.md Q6"
        );
    }
}
