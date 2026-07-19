// SPDX-License-Identifier: GPL-3.0-or-later

//! Pixel → clip-space region math for the compositor.
//!
//! The compositor shader takes a `vec4 u_rect = (x, y, w, h)` in
//! clip space (`[-1, +1]²`, y-up) and places a unit quad inside it.
//! Plugins declare their regions in pixel coordinates (Wayland's
//! convention: y=0 at top, increases downward, dimensions relative
//! to the lock surface). This module is the conversion.
//!
//! Y math flips because Wayland is top-down and GL is bottom-up.
//! A region's *top* pixel maps to clip space `+1 - 2*y/h`; its
//! *bottom* pixel maps to `+1 - 2*(y+h)/h`. The returned rect uses
//! the bottom-left corner as origin (matching GL conventions),
//! with positive `(w, h)` extending toward top-right.
//!
//! A `None` region (plugin omitted `region` in its config) means
//! "fill the whole lock surface" — returns the unit rect
//! `(-1, -1, 2, 2)` which the vertex shader maps to the full
//! screen. This is also what the math produces for an explicit
//! full-surface region, so the two paths are observationally
//! identical.

use crate::config::Region;

/// Convert a region (in pixel coords, top-down) to the compositor's
/// clip-space rect `(x, y, w, h)` (bottom-up). See module-level
/// docs for the coordinate-system explanation.
///
/// Off-screen or oversized regions are *not* rejected — the math
/// produces clip-space values outside `[-1, +1]` and GL clips them
/// at the rasterizer. The config loader logs a warning for
/// implausible-looking coords (>8192) but otherwise lets them
/// through.
pub fn region_to_clip_rect(region: Option<&Region>, surface_w: i32, surface_h: i32) -> [f32; 4] {
    let Some(r) = region else {
        return [-1.0, -1.0, 2.0, 2.0];
    };
    let sw = surface_w as f32;
    let sh = surface_h as f32;
    let left = (r.x as f32) * 2.0 / sw - 1.0;
    let right = ((r.x + r.w as i32) as f32) * 2.0 / sw - 1.0;
    let top = 1.0 - (r.y as f32) * 2.0 / sh;
    let bottom = 1.0 - ((r.y + r.h as i32) as f32) * 2.0 / sh;
    [left, bottom, right - left, top - bottom]
}

/// The dimensions a plugin's `Configure` should carry: `(x, y, w, h)`.
///
/// This is the other half of the region contract from `region_to_clip_rect`.
/// `region_to_clip_rect` decides *where on screen* a plugin's buffer is
/// composited; this decides *how big a buffer* the plugin allocates by telling
/// it its render size in `Configure`. The two must agree: when a region is
/// declared, the plugin renders a region-sized buffer and the composite is the
/// identity transform (no stretch). When the host lied here (always sending
/// full-surface dims), a region plugin rendered a full-surface texture that the
/// compositor then squashed into the smaller region quad.
///
/// - `None` region → `(0, 0, surface_w, surface_h)`: fill the whole surface.
///   This is byte-identical to what every non-region plugin was sent before
///   this contract existed, so their `Configure` is unchanged.
/// - `Some(region)` → `(region.x, region.y, region.w, region.h)`: render at the
///   region's real position and size. Coords are absolute pixels (see
///   `docs/config.md`), passed through unchanged — they are *not* rescaled on a
///   mode change, which is the resolution-hostility that anchor keywords
///   address separately.
///
/// Both `Configure`-construction sites (the initial spawn in
/// `plugin::host_spawn` and the resize resend in
/// `app::resend_configure_region_for_output`) call this so they cannot drift.
pub fn configure_dims(
    region: Option<&Region>,
    surface_w: u32,
    surface_h: u32,
) -> (i32, i32, u32, u32) {
    match region {
        None => (0, 0, surface_w, surface_h),
        Some(r) => (r.x, r.y, r.w, r.h),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Region;

    /// Compare two `[f32; 4]` arrays with a tolerance. f32 math
    /// from divisions isn't bit-exact across compilers/optimisers,
    /// and exact equality on `[-0.8958333, ...]` is brittle.
    fn assert_rect_eq(got: [f32; 4], want: [f32; 4]) {
        let tol = 1e-5;
        for (i, (g, w)) in got.iter().zip(want.iter()).enumerate() {
            assert!(
                (g - w).abs() < tol,
                "rect[{}]: got {}, want {} (got rect {:?}, want rect {:?})",
                i,
                g,
                w,
                got,
                want
            );
        }
    }

    #[test]
    fn none_region_is_full_screen() {
        // A plugin that omitted `region` in config gets the whole
        // surface. The unit rect (-1, -1, 2, 2) maps to the full
        // [-1, +1]² clip space.
        let got = region_to_clip_rect(None, 1920, 1080);
        assert_rect_eq(got, [-1.0, -1.0, 2.0, 2.0]);
    }

    #[test]
    fn full_surface_region_equals_none_region() {
        // Explicit full-surface region produces the same clip rect
        // as `None`. This is the regression check: a single plugin
        // with `region = { x = 0, y = 0, w = SURFACE_W, h = SURFACE_H }`
        // must render identically to the pre-step-3 full-screen
        // composite.
        let r = Region {
            x: 0,
            y: 0,
            w: 1920,
            h: 1080,
        };
        let got = region_to_clip_rect(Some(&r), 1920, 1080);
        assert_rect_eq(got, [-1.0, -1.0, 2.0, 2.0]);
    }

    #[test]
    fn top_left_quarter() {
        // Quarter-screen rectangle anchored at top-left in pixel
        // coords. The Wayland top-left (0, 0) maps to clip-space
        // upper-left (-1, +1). The quarter occupies clip x in
        // [-1, 0] and clip y in [0, +1].
        let r = Region {
            x: 0,
            y: 0,
            w: 960,
            h: 540,
        };
        let got = region_to_clip_rect(Some(&r), 1920, 1080);
        assert_rect_eq(got, [-1.0, 0.0, 1.0, 1.0]);
    }

    #[test]
    fn bottom_right_quarter() {
        // Quarter-screen rectangle anchored at the bottom-right in
        // pixel coords. The y-flip puts it at clip y in [-1, 0].
        let r = Region {
            x: 960,
            y: 540,
            w: 960,
            h: 540,
        };
        let got = region_to_clip_rect(Some(&r), 1920, 1080);
        assert_rect_eq(got, [0.0, -1.0, 1.0, 1.0]);
    }

    #[test]
    fn small_off_centre_rectangle() {
        // The exhaustive hand-worked example from the plan:
        // 400x100 at (100, 200) on 1920x1080.
        // left   = 100 * 2 / 1920 - 1 ≈ -0.895833
        // right  = 500 * 2 / 1920 - 1 ≈ -0.479167
        // top    = 1 - 200 * 2 / 1080 ≈  0.629630
        // bottom = 1 - 300 * 2 / 1080 ≈  0.444444
        let r = Region {
            x: 100,
            y: 200,
            w: 400,
            h: 100,
        };
        let got = region_to_clip_rect(Some(&r), 1920, 1080);
        let left = -0.895833;
        let bottom = 0.444444;
        let width = 0.416667; // right - left
        let height = 0.185185; // top - bottom
        assert_rect_eq(got, [left, bottom, width, height]);
    }

    #[test]
    fn off_screen_negative_x_is_finite() {
        // GL clips at the rasterizer; the config loader doesn't
        // reject implausible coords. We at least guarantee the
        // math produces finite numbers — no NaN, no infinity —
        // so the uniform set doesn't silently break the shader.
        let r = Region {
            x: -500,
            y: 0,
            w: 200,
            h: 200,
        };
        let got = region_to_clip_rect(Some(&r), 1920, 1080);
        for v in got.iter() {
            assert!(v.is_finite(), "got non-finite value: {:?}", got);
        }
    }

    #[test]
    fn region_larger_than_surface_is_finite() {
        // Same as above for oversized regions. GL clips; we don't.
        let r = Region {
            x: 0,
            y: 0,
            w: 4000,
            h: 4000,
        };
        let got = region_to_clip_rect(Some(&r), 1920, 1080);
        for v in got.iter() {
            assert!(v.is_finite(), "got non-finite value: {:?}", got);
        }
    }

    #[test]
    fn configure_dims_none_is_full_surface() {
        // The backward-compat guarantee: a plugin that omitted `region`
        // gets exactly the dims every non-region plugin was sent before
        // this contract existed — full surface, origin (0, 0). If this
        // ever changes, every shipped reference plugin's Configure changes.
        assert_eq!(configure_dims(None, 1920, 1080), (0, 0, 1920, 1080));
        assert_eq!(configure_dims(None, 3840, 2160), (0, 0, 3840, 2160));
    }

    #[test]
    fn configure_dims_some_reports_region() {
        // A declared region reports its own position and size, NOT the
        // surface — this is the whole fix. The plugin allocates a
        // region-sized buffer and the composite becomes identity (no
        // stretch). Coords pass through as absolute pixels.
        let r = Region {
            x: 760,
            y: 440,
            w: 400,
            h: 200,
        };
        assert_eq!(configure_dims(Some(&r), 1920, 1080), (760, 440, 400, 200));
    }

    #[test]
    fn configure_dims_some_ignores_surface_size() {
        // The region dims are independent of the surface: the same region
        // on a 1080p and a 4K surface reports the same (x, y, w, h). (That
        // absolute-pixel behaviour is exactly what makes explicit regions
        // resolution-hostile and motivates anchor keywords; the contract
        // itself is correct — no stretch on either surface.)
        let r = Region {
            x: 100,
            y: 100,
            w: 300,
            h: 80,
        };
        assert_eq!(
            configure_dims(Some(&r), 1920, 1080),
            configure_dims(Some(&r), 3840, 2160),
            "region dims must not depend on surface size"
        );
    }
}
