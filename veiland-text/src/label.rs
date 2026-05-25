// SPDX-License-Identifier: GPL-3.0-or-later

//! Public `Label` API and its GL draw path. See `docs/m10-plan.md`
//! step 5a.
//!
//! 5a is "see Latin text on screen for the first time." The public
//! struct has text/font/size/color/alignment/position; the render path
//! shapes via cosmic-text, uploads glyphs into the atlas (step 4) on
//! cache miss, then emits one quad per glyph and issues a single
//! `glDrawArrays`. No rotation, no shadow — 5b adds those.
//!
//! GL dialect matches the rest of veiland: GLES 2 (`#version 100`),
//! `attribute`/`varying`/`uniform`, `gl_FragColor`. See
//! `veiland-core/src/main.rs`'s `build_indicator_program` for the
//! pattern this mirrors.

// gl is FFI; this module needs unsafe just like atlas.rs. Crate-level
// deny stays in lib.rs.
#![allow(unsafe_code)]

use cosmic_text::{
    Attrs, Buffer, Family, FontSystem, Metrics, Shaping, SwashCache, SwashContent, fontdb,
};

use crate::atlas::{Atlas, GlyphKey};

/// Horizontal alignment of the text content rect relative to `position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

/// Vertical alignment of the text content rect relative to `position`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VAlign {
    Top,
    Middle,
    Bottom,
}

/// A single styled text label. Constructed by the plugin from its
/// config; consumed by `FontContext::render_label`. Cheap to build — all
/// the work happens in `render_label`.
///
/// 5a fields. Rotation and shadow land in 5b.
#[derive(Debug, Clone)]
pub struct Label {
    /// The text to display. UTF-8; cosmic-text handles complex scripts
    /// (CJK, RTL, combining marks).
    pub text: String,
    /// CSS-style family name. Falls back to system Sans if not found.
    pub font_family: String,
    /// Logical pixels. The plugin multiplies by `Configure.scale`
    /// before constructing the Label; see `docs/protocol.md` §7.1.
    pub font_size: f32,
    /// Straight-alpha RGBA, each component in [0, 1].
    pub color: [f32; 4],
    /// Which horizontal edge of the content rect sits at `position.x`.
    pub halign: HAlign,
    /// Which vertical edge of the content rect sits at `position.y`.
    pub valign: VAlign,
    /// Anchor point in surface pixels (top-left origin).
    pub position: (f32, f32),
}

impl Label {
    /// Construct with minimal required state; remaining fields take
    /// reasonable defaults. The plugin is expected to fill in fields it
    /// cares about via field assignment after `new`.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            font_family: "Sans".to_string(),
            font_size: 16.0,
            color: [1.0, 1.0, 1.0, 1.0],
            halign: HAlign::Left,
            valign: VAlign::Top,
            position: (0.0, 0.0),
        }
    }
}

/// Per-context GL state for label rendering. One per `FontContext`,
/// reused across every `Label::render_label` call. Mirrors M9's
/// `build_indicator_program`/VBO pattern.
pub(crate) struct LabelGl {
    program: u32,
    vbo: u32,
    a_pos_loc: i32,
    a_uv_loc: i32,
    u_surface_loc: i32,
    u_color_loc: i32,
    u_atlas_loc: i32,
    /// Set to `true` if shader compile/link failed. Subsequent renders
    /// no-op. Lockscreen-grade error handling: don't crash the locker
    /// because a driver hiccupped. Tofu beats a black screen, but a
    /// black screen beats a panic.
    broken: bool,
}

const VS_SRC: &[u8] = b"#version 100\n\
    attribute vec2 a_pos;\n\
    attribute vec2 a_uv;\n\
    uniform vec2 u_surface;\n\
    varying vec2 v_uv;\n\
    void main() {\n\
        // a_pos is in surface pixels, top-left origin. Convert to GL\n\
        // clip space [-1, 1], flipping Y so top-left in pixels maps to\n\
        // top of clip. See docs/m10-plan.md step 5a concept 3.\n\
        vec2 clip;\n\
        clip.x = (a_pos.x / u_surface.x) * 2.0 - 1.0;\n\
        clip.y = 1.0 - (a_pos.y / u_surface.y) * 2.0;\n\
        gl_Position = vec4(clip, 0.0, 1.0);\n\
        v_uv = a_uv;\n\
    }\n\0";

// highp on the fragment shader to match the password indicator's
// rationale: GLES 2 defaults to mediump which some Mesa drivers honour
// as fp16; we want consistent edge quality on both Mesa and NVIDIA.
const FS_SRC: &[u8] = b"#version 100\n\
    precision highp float;\n\
    varying vec2 v_uv;\n\
    uniform sampler2D u_atlas;\n\
    uniform vec4 u_color;\n\
    void main() {\n\
        // Single-channel atlas (R8); the .r channel is coverage 0..1.\n\
        // Colour comes from the uniform, not the texture: same atlas\n\
        // entry can be drawn in any colour. See concept 1.\n\
        float coverage = texture2D(u_atlas, v_uv).r;\n\
        gl_FragColor = vec4(u_color.rgb, u_color.a * coverage);\n\
    }\n\0";

impl LabelGl {
    /// Compile shaders + allocate the dynamic VBO. Requires a live GL
    /// context. Returns a struct flagged `broken = true` if compilation
    /// failed — the caller checks and skips rendering rather than
    /// crashing the locker.
    pub(crate) fn new() -> Self {
        // SAFETY: gl is FFI; caller (FontContext, called from
        // render_label) guarantees a current GL context.
        unsafe {
            let vs = match compile(gl::VERTEX_SHADER, VS_SRC) {
                Some(s) => s,
                None => return Self::broken(),
            };
            let fs = match compile(gl::FRAGMENT_SHADER, FS_SRC) {
                Some(s) => s,
                None => {
                    gl::DeleteShader(vs);
                    return Self::broken();
                }
            };
            let program = gl::CreateProgram();
            gl::AttachShader(program, vs);
            gl::AttachShader(program, fs);
            gl::LinkProgram(program);
            gl::DeleteShader(vs);
            gl::DeleteShader(fs);
            let mut ok: i32 = 0;
            gl::GetProgramiv(program, gl::LINK_STATUS, &mut ok);
            if ok == 0 {
                let mut log = [0u8; 1024];
                let mut len: i32 = 0;
                gl::GetProgramInfoLog(
                    program,
                    log.len() as i32,
                    &mut len,
                    log.as_mut_ptr() as *mut _,
                );
                eprintln!(
                    "veiland-text: label program link failed: {}",
                    std::str::from_utf8(&log[..len as usize]).unwrap_or("<invalid utf8>"),
                );
                gl::DeleteProgram(program);
                return Self::broken();
            }

            let a_pos_loc = gl::GetAttribLocation(program, b"a_pos\0".as_ptr() as *const _);
            let a_uv_loc = gl::GetAttribLocation(program, b"a_uv\0".as_ptr() as *const _);
            let u_surface_loc =
                gl::GetUniformLocation(program, b"u_surface\0".as_ptr() as *const _);
            let u_color_loc = gl::GetUniformLocation(program, b"u_color\0".as_ptr() as *const _);
            let u_atlas_loc = gl::GetUniformLocation(program, b"u_atlas\0".as_ptr() as *const _);

            let mut vbo: u32 = 0;
            gl::GenBuffers(1, &mut vbo);
            // Allocation is deferred until the first render — we don't
            // know the upper bound on vertex count yet.

            Self {
                program,
                vbo,
                a_pos_loc,
                a_uv_loc,
                u_surface_loc,
                u_color_loc,
                u_atlas_loc,
                broken: false,
            }
        }
    }

    fn broken() -> Self {
        Self {
            program: 0,
            vbo: 0,
            a_pos_loc: -1,
            a_uv_loc: -1,
            u_surface_loc: -1,
            u_color_loc: -1,
            u_atlas_loc: -1,
            broken: true,
        }
    }
}

impl Drop for LabelGl {
    fn drop(&mut self) {
        // Either we failed to compile shaders (broken == true, program
        // and vbo are 0) or we never had GL function pointers loaded
        // (unit tests). Both want the same skip.
        if self.broken || self.program == 0 {
            return;
        }
        // SAFETY: gl is FFI; best-effort cleanup. If the GL context is
        // already gone the no-op-on-invalid-name rule applies.
        unsafe {
            gl::DeleteProgram(self.program);
            gl::DeleteBuffers(1, &self.vbo);
        }
    }
}

unsafe fn compile(kind: u32, src: &[u8]) -> Option<u32> {
    unsafe {
        let shader = gl::CreateShader(kind);
        let src_ptr = src.as_ptr() as *const _;
        gl::ShaderSource(shader, 1, &src_ptr, std::ptr::null());
        gl::CompileShader(shader);
        let mut ok: i32 = 0;
        gl::GetShaderiv(shader, gl::COMPILE_STATUS, &mut ok);
        if ok == 0 {
            let mut log = [0u8; 1024];
            let mut len: i32 = 0;
            gl::GetShaderInfoLog(
                shader,
                log.len() as i32,
                &mut len,
                log.as_mut_ptr() as *mut _,
            );
            eprintln!(
                "veiland-text: label shader compile failed ({}): {}",
                if kind == gl::VERTEX_SHADER { "vertex" } else { "fragment" },
                std::str::from_utf8(&log[..len as usize]).unwrap_or("<invalid utf8>"),
            );
            gl::DeleteShader(shader);
            return None;
        }
        Some(shader)
    }
}

/// One vertex of a glyph quad: (x, y) in surface pixels, (u, v) in
/// atlas coords. 16 bytes; six vertices per glyph.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
struct Vertex {
    x: f32,
    y: f32,
    u: f32,
    v: f32,
}

/// The whole render flow lives here. Called by `FontContext::render_label`
/// once `LabelGl` and `Atlas` are both materialized.
///
/// Steps:
///   1. Shape the text via cosmic-text → list of `PhysicalGlyph` with
///      cache keys and integer-snapped positions.
///   2. For each glyph: get the swash image (cached or fresh), look it
///      up in the atlas, upload on miss.
///   3. Compute the content rect's `(w, h)` from the shaped glyphs, use
///      it with `halign`/`valign` to derive the offset to apply to
///      every glyph's position.
///   4. Emit six vertices per glyph into one big buffer.
///   5. One `glDrawArrays`.
pub(crate) fn render_label(
    label: &Label,
    label_gl: &mut LabelGl,
    atlas: &mut Atlas,
    font_system: &mut FontSystem,
    swash_cache: &mut SwashCache,
    surface_size: (u32, u32),
) {
    if label_gl.broken {
        return;
    }
    if label.text.is_empty() {
        return;
    }

    // 1. Shape. Metrics: line_height = font_size * 1.2 is the standard
    //    default; cosmic-text needs it but for single-line labels the
    //    exact value only affects content-rect height calculation.
    let metrics = Metrics::new(label.font_size, label.font_size * 1.2);
    let mut buffer = Buffer::new(font_system, metrics);
    // Unbounded width: don't wrap. Plain Font::SansSerif if name doesn't
    // resolve — fontdb's own fallback handles that. For "Sans" specifically
    // cosmic-text already has Family::SansSerif; we use Name(...) so users
    // can specify any system family they like.
    let family = match label.font_family.as_str() {
        "Sans" | "sans" | "sans-serif" => Family::SansSerif,
        "Serif" | "serif" => Family::Serif,
        "Monospace" | "monospace" => Family::Monospace,
        other => Family::Name(other),
    };
    let attrs = Attrs::new().family(family);
    buffer.set_size(Some(f32::MAX), Some(f32::MAX));
    buffer.set_text(&label.text, &attrs, Shaping::Advanced, None);
    buffer.shape_until_scroll(font_system, false);

    // 2 + 3. Walk layout runs, collect PhysicalGlyph + bitmap info,
    //        compute content rect bounds as we go.
    struct PreparedGlyph {
        screen_x: f32,
        screen_y: f32,
        w: f32,
        h: f32,
        u_min: f32,
        v_min: f32,
        u_max: f32,
        v_max: f32,
    }
    let mut prepared: Vec<PreparedGlyph> = Vec::new();
    let mut min_x = f32::INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut max_y = f32::NEG_INFINITY;

    for run in buffer.layout_runs() {
        let baseline_y = run.line_y;
        for glyph in run.glyphs {
            // physical((0, 0), 1.0) snaps to integer pixel — scale=1
            // because we already baked HiDPI scale into font_size at
            // the call site (the plugin multiplied by Configure.scale).
            let physical = glyph.physical((0.0, 0.0), 1.0);

            // Discretize for our atlas cache key. size_px collapses
            // fractional font_size jitter; subpixel_bin = 0 per the
            // M10 "snap to grid" decision.
            let size_px = (label.font_size.round() as i32).clamp(1, u16::MAX as i32) as u16;
            let key = GlyphKey {
                font_id: hash_font_id(physical.cache_key.font_id),
                glyph_id: physical.cache_key.glyph_id,
                size_px,
                subpixel_bin: 0,
            };

            let entry = if let Some(e) = atlas.lookup(key) {
                e
            } else {
                // Miss: rasterize via swash and upload to the atlas.
                // swash_cache.get_image returns &Option<SwashImage>; we
                // clone the placement and bitmap data because the atlas
                // upload borrows mutably and we can't hold the &Option
                // across that.
                let image = match swash_cache.get_image(font_system, physical.cache_key) {
                    Some(img) => img,
                    None => continue, // font/glyph not available; skip
                };
                // Skip colour bitmaps (emoji); M10 mask-only path.
                if image.content != SwashContent::Mask {
                    continue;
                }
                let placement = image.placement;
                let data = image.data.clone();
                if placement.width == 0 || placement.height == 0 {
                    // Whitespace: no bitmap to upload but the advance
                    // still matters. The atlas inserts a zero-area
                    // entry so the next lookup is a hit.
                    atlas.insert_bitmap(key, 0, 0, &[])
                } else {
                    atlas.insert_bitmap(key, placement.width, placement.height, &data)
                }
            };

            // Where in the surface does this glyph sit? swash's
            // placement is relative to the glyph's pen position with
            // Y-up (top is positive). Our screen space is Y-down.
            //
            // We need the placement to compute the quad — re-fetch
            // from swash. The atlas doesn't store metrics; only UVs.
            let placement = match swash_cache.get_image(font_system, physical.cache_key) {
                Some(img) if img.content == SwashContent::Mask => img.placement,
                _ => continue,
            };

            let w = placement.width as f32;
            let h = placement.height as f32;
            let screen_x = physical.x as f32 + placement.left as f32;
            let screen_y = baseline_y + physical.y as f32 - placement.top as f32;

            if w > 0.0 && h > 0.0 {
                min_x = min_x.min(screen_x);
                min_y = min_y.min(screen_y);
                max_x = max_x.max(screen_x + w);
                max_y = max_y.max(screen_y + h);
            }

            prepared.push(PreparedGlyph {
                screen_x,
                screen_y,
                w,
                h,
                u_min: entry.u_min,
                v_min: entry.v_min,
                u_max: entry.u_max,
                v_max: entry.v_max,
            });
        }
    }

    if prepared.is_empty() || !min_x.is_finite() {
        return;
    }

    // 3 (continued). Apply halign/valign offset.
    let content_w = max_x - min_x;
    let content_h = max_y - min_y;
    let (dx, dy) = alignment_offset(label.halign, label.valign, content_w, content_h);
    // The content rect's top-left is at (min_x, min_y) in shaped-glyph
    // coords. We want it to end up at (position.x + dx, position.y + dy)
    // in surface coords.
    let ox = label.position.0 + dx - min_x;
    let oy = label.position.1 + dy - min_y;

    // 4. Build the vertex buffer.
    let mut vertices: Vec<Vertex> = Vec::with_capacity(prepared.len() * 6);
    for g in &prepared {
        if g.w == 0.0 || g.h == 0.0 {
            continue;
        }
        let x0 = g.screen_x + ox;
        let y0 = g.screen_y + oy;
        let x1 = x0 + g.w;
        let y1 = y0 + g.h;
        let tl = Vertex { x: x0, y: y0, u: g.u_min, v: g.v_min };
        let tr = Vertex { x: x1, y: y0, u: g.u_max, v: g.v_min };
        let bl = Vertex { x: x0, y: y1, u: g.u_min, v: g.v_max };
        let br = Vertex { x: x1, y: y1, u: g.u_max, v: g.v_max };
        vertices.extend_from_slice(&[tl, bl, tr, tr, bl, br]);
    }

    if vertices.is_empty() {
        return;
    }

    // 5. Issue the draw.
    // SAFETY: gl FFI; LabelGl invariants checked above (broken=false),
    // Atlas owns a valid texture from its own construction. Surface
    // size from caller; vertices owned by us.
    unsafe {
        gl::UseProgram(label_gl.program);
        gl::BindBuffer(gl::ARRAY_BUFFER, label_gl.vbo);
        gl::BufferData(
            gl::ARRAY_BUFFER,
            (vertices.len() * std::mem::size_of::<Vertex>()) as isize,
            vertices.as_ptr() as *const _,
            gl::DYNAMIC_DRAW,
        );

        gl::EnableVertexAttribArray(label_gl.a_pos_loc as u32);
        gl::VertexAttribPointer(
            label_gl.a_pos_loc as u32,
            2,
            gl::FLOAT,
            gl::FALSE,
            std::mem::size_of::<Vertex>() as i32,
            std::ptr::null(),
        );
        gl::EnableVertexAttribArray(label_gl.a_uv_loc as u32);
        gl::VertexAttribPointer(
            label_gl.a_uv_loc as u32,
            2,
            gl::FLOAT,
            gl::FALSE,
            std::mem::size_of::<Vertex>() as i32,
            (2 * std::mem::size_of::<f32>()) as *const _,
        );

        gl::Uniform2f(label_gl.u_surface_loc, surface_size.0 as f32, surface_size.1 as f32);
        gl::Uniform4f(
            label_gl.u_color_loc,
            label.color[0],
            label.color[1],
            label.color[2],
            label.color[3],
        );
        gl::ActiveTexture(gl::TEXTURE0);
        gl::BindTexture(gl::TEXTURE_2D, atlas.texture());
        gl::Uniform1i(label_gl.u_atlas_loc, 0);

        gl::Enable(gl::BLEND);
        gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);

        gl::DrawArrays(gl::TRIANGLES, 0, vertices.len() as i32);

        gl::DisableVertexAttribArray(label_gl.a_pos_loc as u32);
        gl::DisableVertexAttribArray(label_gl.a_uv_loc as u32);
    }
}

/// fontdb::ID is opaque; we hash it down to u64 to fit our GlyphKey.
/// Two different IDs colliding in the lower 64 bits would only cause a
/// visual glitch (wrong glyph), not a crash, but fontdb IDs are u32
/// internally so collisions are not expected.
fn hash_font_id(id: fontdb::ID) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut h = DefaultHasher::new();
    id.hash(&mut h);
    h.finish()
}

fn alignment_offset(h: HAlign, v: VAlign, w: f32, ht: f32) -> (f32, f32) {
    let dx = match h {
        HAlign::Left => 0.0,
        HAlign::Center => -w / 2.0,
        HAlign::Right => -w,
    };
    let dy = match v {
        VAlign::Top => 0.0,
        VAlign::Middle => -ht / 2.0,
        VAlign::Bottom => -ht,
    };
    (dx, dy)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pure CPU test of halign/valign math. The GL parts are exercised
    // by step 6's demo plugin.

    #[test]
    fn alignment_left_top_is_origin() {
        assert_eq!(alignment_offset(HAlign::Left, VAlign::Top, 100.0, 50.0), (0.0, 0.0));
    }

    #[test]
    fn alignment_center_middle_halves() {
        let (dx, dy) = alignment_offset(HAlign::Center, VAlign::Middle, 100.0, 50.0);
        assert_eq!(dx, -50.0);
        assert_eq!(dy, -25.0);
    }

    #[test]
    fn alignment_right_bottom_full_offset() {
        let (dx, dy) = alignment_offset(HAlign::Right, VAlign::Bottom, 100.0, 50.0);
        assert_eq!(dx, -100.0);
        assert_eq!(dy, -50.0);
    }
}
