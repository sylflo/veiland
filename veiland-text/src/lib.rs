// SPDX-License-Identifier: GPL-3.0-or-later

//! Text rendering for veiland plugins. See `docs/m10-plan.md`.
//!
//! M10 step 5a: the public `Label` API and its GL draw path land.
//! Plugins construct a `FontContext` once at startup, build a `Label`
//! per frame (cheap — just config), and call `FontContext::render`.
//! Atlas (step 4) and shader/VBO (this step) materialize lazily on the
//! first render so `FontContext::new()` stays GL-context-free.
//!
//! No rotation or shadow yet — step 5b adds those.

#![deny(unsafe_code)]

mod atlas;
mod label;

use cosmic_text::{FontSystem, SwashCache};

use atlas::Atlas;
use label::LabelGl;

pub use label::{HAlign, Label, Shadow, VAlign};

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
    /// Lazily initialized on the first `render` call; `None` until then
    /// so plugins without a GL context yet (e.g. during their own
    /// startup before the first FrameDone) can still construct a
    /// `FontContext`.
    atlas: Option<Atlas>,
    /// Same lazy-init story as `atlas`: shader compilation needs a live
    /// GL context, so we wait until the plugin has one.
    label_gl: Option<LabelGl>,
}

impl FontContext {
    pub fn new() -> Self {
        Self {
            font_system: FontSystem::new(),
            swash_cache: SwashCache::new(),
            atlas: None,
            label_gl: None,
        }
    }

    /// Draw `label` into the currently-bound framebuffer. Requires a
    /// live GL context.
    ///
    /// First call lazily initializes the glyph atlas and the shader
    /// program; subsequent calls reuse both. If shader compilation
    /// failed (logged via `eprintln!`), this becomes a no-op for the
    /// rest of the session — the lockscreen continues without text
    /// rather than crashing.
    ///
    /// `surface_size` is the framebuffer's `(width, height)` in
    /// physical pixels. The plugin passes its dmabuf's dimensions.
    pub fn render(&mut self, label: &Label, surface_size: (u32, u32)) {
        let atlas = self.atlas.get_or_insert_with(Atlas::new);
        let label_gl = self.label_gl.get_or_insert_with(LabelGl::new);
        label::render_label(
            label,
            label_gl,
            atlas,
            &mut self.font_system,
            &mut self.swash_cache,
            surface_size,
        );
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
