// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-side EGL/GL rendering state.
//!
//! `Renderer` owns the host's single EGL context (shared across all
//! plugin dmabuf imports) and the two GL programs the host draws with:
//! the compositor program (samples a plugin's imported texture onto a
//! clip-space rect) and the indicator program (one filled circle per
//! password dot). It is held as one field on `AppData`; the rest of the
//! core reaches GL state through `self.renderer.*`.
//!
//! This is a structural home for state that used to live as a dozen
//! flat fields on `AppData`. The build helpers and `draw_password_
//! indicator` moved here verbatim — no behavior change.

use khronos_egl as egl;

use crate::config;

/// All host-side EGL + GL handles, grouped.
pub struct Renderer {
    pub egl: egl::Instance<egl::Static>,
    pub egl_display: egl::Display,
    pub egl_config: egl::Config,
    pub egl_context: egl::Context,
    pub compositor_program: gl::types::GLuint,
    pub compositor_vbo: gl::types::GLuint,
    pub compositor_sampler_loc: gl::types::GLint,
    pub compositor_rect_loc: gl::types::GLint,
    pub indicator_program: gl::types::GLuint,
    pub indicator_vbo: gl::types::GLuint,
    pub indicator_centre_loc: gl::types::GLint,
    pub indicator_radius_loc: gl::types::GLint,
    pub indicator_color_loc: gl::types::GLint,
    /// Rounded-rect input-box program. One SDF-shaded quad draws the
    /// fill + outline in a single pass; see `build_box_program`.
    pub box_program: gl::types::GLuint,
    pub box_vbo: gl::types::GLuint,
    /// Clip-space placement rect (x, y, w, h) of the box quad.
    pub box_rect_loc: gl::types::GLint,
    /// Box half-extent in surface px (the SDF works in px space).
    pub box_half_loc: gl::types::GLint,
    /// Corner radius in surface px.
    pub box_radius_loc: gl::types::GLint,
    /// Outline thickness in surface px (0 = fill only).
    pub box_outline_loc: gl::types::GLint,
    /// Fill colour (straight RGBA; shader premultiplies).
    pub box_inner_loc: gl::types::GLint,
    /// Outline colour (straight RGBA; shader premultiplies).
    pub box_outer_loc: gl::types::GLint,
}

impl Renderer {
    /// Build both GL programs against an already-current EGL context and
    /// assemble the `Renderer`. The caller is responsible for having
    /// created and made-current `egl_context` (surfaceless) before this
    /// runs — the GL program build issues `gl::*` calls that need a
    /// current context.
    pub fn new(
        egl: egl::Instance<egl::Static>,
        egl_display: egl::Display,
        egl_config: egl::Config,
        egl_context: egl::Context,
    ) -> Self {
        let (compositor_program, compositor_vbo, compositor_sampler_loc, compositor_rect_loc) =
            unsafe { build_compositor_program() };
        eprintln!("built compositor program id={}", compositor_program);

        let (
            indicator_program,
            indicator_vbo,
            indicator_centre_loc,
            indicator_radius_loc,
            indicator_color_loc,
        ) = unsafe { build_indicator_program() };
        eprintln!("built indicator program id={}", indicator_program);

        let box_p = unsafe { build_box_program() };
        eprintln!("built box program id={}", box_p.program);

        Renderer {
            egl,
            egl_display,
            egl_config,
            egl_context,
            compositor_program,
            compositor_vbo,
            compositor_sampler_loc,
            compositor_rect_loc,
            indicator_program,
            indicator_vbo,
            indicator_centre_loc,
            indicator_radius_loc,
            indicator_color_loc,
            box_program: box_p.program,
            box_vbo: box_p.vbo,
            box_rect_loc: box_p.rect_loc,
            box_half_loc: box_p.half_loc,
            box_radius_loc: box_p.radius_loc,
            box_outline_loc: box_p.outline_loc,
            box_inner_loc: box_p.inner_loc,
            box_outer_loc: box_p.outer_loc,
        }
    }

    /// Draw the password field (input box + dots) on the currently-bound
    /// EGL surface.
    ///
    /// `width` and `height` are the surface's pixel dimensions;
    /// `char_count` is the current password length (the caller passes
    /// `auth.char_count()` — the password buffer stays owned by the
    /// core's auth session, never by the renderer). `password` is the
    /// `[password]` config table. The caller is responsible for making
    /// the right EGL context current and clearing the framebuffer; this
    /// method only issues the field draws. Designed to be called *last*
    /// in the per-surface paint sequence so the field appears on top of
    /// any plugins (the soft trust-region — plugins can declare any
    /// region, the field always wins on paint order).
    ///
    /// Paint order within the field: box first (so it sits behind), then
    /// one draw call per dot. The box draws whenever `show_box` is set,
    /// even with zero characters typed, so the user sees where to type.
    pub fn draw_password_field(
        &self,
        password: &config::Password,
        char_count: usize,
        width: i32,
        height: i32,
    ) {
        let pw = password;

        if width <= 0 || height <= 0 {
            return;
        }
        let w = width as f32;
        let h = height as f32;

        // Field centre in surface pixels. `x` default is surface-relative
        // (centred), so it's resolved per-surface here; `y_percent`
        // default of 75 likewise. Both position the box; when the box is
        // shown the dots auto-centre on this same point, so the two never
        // drift apart.
        let centre_x_px = pw.x.map(|v| v as f32).unwrap_or(w / 2.0);
        let y_percent = pw.y_percent.unwrap_or(75) as f32;
        let centre_y_px = h * y_percent / 100.0;

        if pw.show_box {
            self.draw_box(pw, centre_x_px, centre_y_px, w, h);
        }

        // Cap at max_dots (config-driven; clamped at load to [1, 256]).
        // The row freezes at this value — the user keeps typing but the
        // dot count stops growing. Checked *after* the box so an empty
        // box still renders.
        let n = char_count.min(pw.max_dots as usize);
        if n == 0 {
            return;
        }

        let color = pw.dot_color.0;
        let diameter = pw.dot_diameter as f32;
        let spacing = pw.dot_spacing as f32;

        // Leftmost dot centre in surface pixels. total_width is the
        // row's extent edge-to-edge; centring it on centre_x_px puts
        // the leftmost *edge* at centre_x_px - total/2, so the
        // leftmost *centre* is half a diameter further right.
        let total_width = (n as f32 - 1.0) * spacing + diameter;
        let start_x = centre_x_px - total_width / 2.0 + diameter / 2.0;

        // Clip-space radius: surface-px / (surface-px / 2) = 2 * px /
        // surface, per axis. Width and height differ for non-square
        // surfaces, so the dot stays circular on screen.
        let rx = diameter / w;
        let ry = diameter / h;

        unsafe {
            gl::UseProgram(self.indicator_program);

            // Vertex attribute setup — same shape as plugin/state.rs's
            // composite(). Re-binding per call is cheap and keeps this
            // method self-contained (no assumed GL state from the
            // previous program).
            gl::BindBuffer(gl::ARRAY_BUFFER, self.indicator_vbo);
            let a_pos =
                gl::GetAttribLocation(self.indicator_program, b"a_pos\0".as_ptr() as *const _);
            gl::EnableVertexAttribArray(a_pos as u32);
            gl::VertexAttribPointer(a_pos as u32, 2, gl::FLOAT, gl::FALSE, 0, std::ptr::null());

            // Uniforms that don't change between dots.
            gl::Uniform2f(self.indicator_radius_loc, rx, ry);
            gl::Uniform4f(
                self.indicator_color_loc,
                color[0],
                color[1],
                color[2],
                color[3],
            );

            for i in 0..n {
                let centre_x = start_x + i as f32 * spacing;
                // Surface-px → clip space. Y is flipped: surface y=0
                // is top, clip y=+1 is top. (The compositor shader
                // flips at the UV instead; the indicator has no UV,
                // so we flip here.)
                let cx = (centre_x / w) * 2.0 - 1.0;
                let cy = -((centre_y_px / h) * 2.0 - 1.0);
                gl::Uniform2f(self.indicator_centre_loc, cx, cy);
                gl::DrawArrays(gl::TRIANGLES, 0, 6);
            }
        }
    }

    /// Draw the rounded input box, centred on `(centre_x_px, centre_y_px)`
    /// in surface pixels. `w`/`h` are the surface dimensions in pixels.
    /// One SDF-shaded quad; see `build_box_program`.
    fn draw_box(&self, pw: &config::Password, centre_x_px: f32, centre_y_px: f32, w: f32, h: f32) {
        let box_w = pw.box_width as f32;
        let box_h = pw.box_height as f32;
        let half_x = box_w / 2.0;
        let half_y = box_h / 2.0;

        // `rounding == -1` is the full-pill sentinel: radius = half the
        // box height. Otherwise use the (already-clamped) value directly.
        let radius = if pw.rounding < 0 {
            half_y
        } else {
            pw.rounding as f32
        };
        let outline = pw.outline_thickness as f32;

        // Clip-space placement rect (x, y, w, h), matching the compositor's
        // unit-quad remap. The box centre maps to clip space with Y flipped
        // (surface y=0 is top, clip y=+1 is top), and the rect's clip-space
        // size is the box's surface size scaled to the full [-1, 1] range.
        let clip_w = box_w / w * 2.0;
        let clip_h = box_h / h * 2.0;
        let clip_cx = (centre_x_px / w) * 2.0 - 1.0;
        let clip_cy = -((centre_y_px / h) * 2.0 - 1.0);
        // Rect origin is the lower-left corner in clip space.
        let rect = [clip_cx - clip_w / 2.0, clip_cy - clip_h / 2.0, clip_w, clip_h];

        let inner = pw.inner_color.0;
        let outer = pw.outer_color.0;

        unsafe {
            gl::UseProgram(self.box_program);

            gl::BindBuffer(gl::ARRAY_BUFFER, self.box_vbo);
            let a_pos = gl::GetAttribLocation(self.box_program, b"a_pos\0".as_ptr() as *const _);
            gl::EnableVertexAttribArray(a_pos as u32);
            gl::VertexAttribPointer(a_pos as u32, 2, gl::FLOAT, gl::FALSE, 0, std::ptr::null());

            gl::Uniform4f(self.box_rect_loc, rect[0], rect[1], rect[2], rect[3]);
            gl::Uniform2f(self.box_half_loc, half_x, half_y);
            gl::Uniform1f(self.box_radius_loc, radius);
            gl::Uniform1f(self.box_outline_loc, outline);
            gl::Uniform4f(self.box_inner_loc, inner[0], inner[1], inner[2], inner[3]);
            gl::Uniform4f(self.box_outer_loc, outer[0], outer[1], outer[2], outer[3]);

            gl::DrawArrays(gl::TRIANGLES, 0, 6);
        }
    }
}

unsafe fn compile_shader(kind: gl::types::GLenum, src: &[u8]) -> gl::types::GLuint {
    unsafe {
        let shader = gl::CreateShader(kind);
        let src_ptr = src.as_ptr() as *const _;
        gl::ShaderSource(shader, 1, &src_ptr, std::ptr::null());
        gl::CompileShader(shader);
        let mut ok: gl::types::GLint = 0;
        gl::GetShaderiv(shader, gl::COMPILE_STATUS, &mut ok);
        if ok == 0 {
            let mut log = [0u8; 1024];
            let mut len: gl::types::GLsizei = 0;
            gl::GetShaderInfoLog(
                shader,
                log.len() as i32,
                &mut len,
                log.as_mut_ptr() as *mut _,
            );
            panic!(
                "shader compile failed: {}",
                std::str::from_utf8(&log[..len as usize]).unwrap_or("<invalid utf8>")
            );
        }
        shader
    }
}

unsafe fn link_program(vs: gl::types::GLuint, fs: gl::types::GLuint) -> gl::types::GLuint {
    unsafe {
        let program = gl::CreateProgram();
        gl::AttachShader(program, vs);
        gl::AttachShader(program, fs);
        gl::LinkProgram(program);
        let mut ok: gl::types::GLint = 0;
        gl::GetProgramiv(program, gl::LINK_STATUS, &mut ok);
        if ok == 0 {
            let mut log = [0u8; 1024];
            let mut len: gl::types::GLsizei = 0;
            gl::GetProgramInfoLog(
                program,
                log.len() as i32,
                &mut len,
                log.as_mut_ptr() as *mut _,
            );
            panic!(
                "program link failed: {}",
                std::str::from_utf8(&log[..len as usize]).unwrap_or("<invalid utf8>")
            );
        }
        program
    }
}

unsafe fn build_compositor_program() -> (
    gl::types::GLuint,
    gl::types::GLuint,
    gl::types::GLint,
    gl::types::GLint,
) {
    let vs_src = b"#version 100\n\
        attribute vec2 a_pos;\n\
        uniform vec4 u_rect;\n\
        varying vec2 v_uv;\n\
        void main() {\n\
            // a_pos is the unit quad in [-1, 1]\xB2. Remap to [0, 1]\n\
            // (= 'normalised quad'), then place inside the target\n\
            // clip-space rect u_rect = (x, y, w, h).\n\
            vec2 unit01 = a_pos * 0.5 + 0.5;\n\
            vec2 clip = u_rect.xy + unit01 * u_rect.zw;\n\
            gl_Position = vec4(clip.x, clip.y, 0.0, 1.0);\n\
    \n\
            // UV samples the plugin's dmabuf edge-to-edge regardless\n\
            // of where the quad lands on screen. Y is flipped because\n\
            // the dmabuf is top-down but GL samples bottom-up.\n\
            v_uv = vec2(unit01.x, 1.0 - unit01.y);\n\
        }\n\0";

    let fs_src = b"#version 100\n\
        precision mediump float;\n\
        varying vec2 v_uv;\n\
        uniform sampler2D u_tex;\n\
        void main() {\n\
            gl_FragColor = texture2D(u_tex, v_uv);\n\
        }\n\0";

    unsafe {
        let vs = compile_shader(gl::VERTEX_SHADER, vs_src);
        let fs = compile_shader(gl::FRAGMENT_SHADER, fs_src);
        let program = link_program(vs, fs);

        let quad: [f32; 12] = [
            -1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0,
        ];

        let mut vbo: gl::types::GLuint = 0;
        gl::GenBuffers(1, &mut vbo);
        gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
        gl::BufferData(
            gl::ARRAY_BUFFER,
            std::mem::size_of_val(&quad) as isize,
            quad.as_ptr() as *const _,
            gl::STATIC_DRAW,
        );

        let sampler_loc = gl::GetUniformLocation(program, b"u_tex\0".as_ptr() as *const _);
        let rect_loc = gl::GetUniformLocation(program, b"u_rect\0".as_ptr() as *const _);

        (program, vbo, sampler_loc, rect_loc)
    }
}

/// Build the password-indicator GL program.
///
/// One filled circle per draw call. The "circle" is a unit quad whose
/// fragment shader discards anything outside radius 1 from the quad
/// centre — standard procedural-shape trick, no geometry library
/// needed. The caller issues N draws (N = dot count) with `u_centre`
/// updated between each; `u_radius` and `u_color` stay constant
/// across the row.
///
/// `u_centre` and `u_radius` are in clip space (so per-frame the
/// caller converts surface-px → clip-space). Y is flipped at
/// conversion time, not in the shader, because there's no UV here.
unsafe fn build_indicator_program() -> (
    gl::types::GLuint,
    gl::types::GLuint,
    gl::types::GLint,
    gl::types::GLint,
    gl::types::GLint,
) {
    let vs_src = b"#version 100\n\
        attribute vec2 a_pos;\n\
        uniform vec2 u_centre;\n\
        uniform vec2 u_radius;\n\
        varying vec2 v_local;\n\
        void main() {\n\
            v_local = a_pos;\n\
            vec2 clip = u_centre + a_pos * u_radius;\n\
            gl_Position = vec4(clip, 0.0, 1.0);\n\
        }\n\0";

    // highp on the fragment shader: GLES 2 defaults to mediump,
    // which some Mesa drivers honour as fp16 and bands the circle
    // edge visibly at 12-px diameter. NVIDIA defaults to fp32
    // either way. highp is portable and cheap at this scale.
    //
    // smoothstep gives a one-fragment-wide antialias ramp on the
    // edge instead of a hard discard. Without it the dot looks
    // pixelated on both vendors at small sizes.
    let fs_src = b"#version 100\n\
        precision highp float;\n\
        varying vec2 v_local;\n\
        uniform vec4 u_color;\n\
        void main() {\n\
            float d = length(v_local);\n\
            // 1.0 inside, 0.0 outside, smooth across the last\n\
            // ~1.5/radius_px fraction of the radius. fwidth would\n\
            // be more correct but isn't in GLES 2 core.\n\
            float a = 1.0 - smoothstep(0.92, 1.0, d);\n\
            if (a <= 0.0) discard;\n\
            // Premultiplied alpha: the indicator paints after the plugin\n\
            // loop under the same ONE / 1-SRC_ALPHA blend, so emit RGB\n\
            // pre-scaled by the final alpha. Straight alpha here would\n\
            // fade the dots, and the dots are the trusted 'still locked'\n\
            // signal, so they must stay solid.\n\
            float pa = u_color.a * a;\n\
            gl_FragColor = vec4(u_color.rgb * pa, pa);\n\
        }\n\0";

    unsafe {
        let vs = compile_shader(gl::VERTEX_SHADER, vs_src);
        let fs = compile_shader(gl::FRAGMENT_SHADER, fs_src);
        let program = link_program(vs, fs);

        // Same unit quad as the compositor. Allocated separately so
        // the two programs stay independent — no shared-VBO coupling
        // to worry about. 48 bytes is free.
        let quad: [f32; 12] = [
            -1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0,
        ];

        let mut vbo: gl::types::GLuint = 0;
        gl::GenBuffers(1, &mut vbo);
        gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
        gl::BufferData(
            gl::ARRAY_BUFFER,
            std::mem::size_of_val(&quad) as isize,
            quad.as_ptr() as *const _,
            gl::STATIC_DRAW,
        );

        let centre_loc = gl::GetUniformLocation(program, b"u_centre\0".as_ptr() as *const _);
        let radius_loc = gl::GetUniformLocation(program, b"u_radius\0".as_ptr() as *const _);
        let color_loc = gl::GetUniformLocation(program, b"u_color\0".as_ptr() as *const _);

        (program, vbo, centre_loc, radius_loc, color_loc)
    }
}

/// Handles produced by `build_box_program`. The box has more uniforms than
/// fit comfortably in a return tuple, so they get a named struct.
struct BoxProgram {
    program: gl::types::GLuint,
    vbo: gl::types::GLuint,
    rect_loc: gl::types::GLint,
    half_loc: gl::types::GLint,
    radius_loc: gl::types::GLint,
    outline_loc: gl::types::GLint,
    inner_loc: gl::types::GLint,
    outer_loc: gl::types::GLint,
}

/// Build the password input-box GL program: a rounded rectangle drawn with
/// a signed-distance-field fragment shader that paints the inner fill and
/// the outline in one pass.
///
/// The SDF works in **surface-pixel space relative to the box centre** so a
/// radius/outline expressed in pixels stays circular and uniform regardless
/// of the box's aspect ratio. The vertex shader places the unit quad into a
/// clip-space rect (`u_rect`, same remap as the compositor) and hands the
/// fragment shader `v_px = (uv - 0.5) * box_size_px`, ranging
/// `[-half, +half]`. The fragment shader's distance `d` is negative inside
/// the rounded shape, zero on the edge, positive outside.
///
/// Like the indicator, colours arrive **straight** and are emitted
/// premultiplied (`rgb * a`) because the box paints under the same
/// `glBlendFunc(ONE, ONE_MINUS_SRC_ALPHA)`. `precision highp float` for the
/// same Mesa-banding reason the indicator uses it.
unsafe fn build_box_program() -> BoxProgram {
    // u_rect: clip-space (x, y, w, h) placement of the quad.
    // u_half: half the box size in surface px (SDF half-extent).
    let vs_src = b"#version 100\n\
        attribute vec2 a_pos;\n\
        uniform vec4 u_rect;\n\
        uniform vec2 u_half;\n\
        varying vec2 v_px;\n\
        void main() {\n\
            // Unit quad [-1,1]\xB2 -> [0,1], placed inside the clip rect.\n\
            vec2 unit01 = a_pos * 0.5 + 0.5;\n\
            vec2 clip = u_rect.xy + unit01 * u_rect.zw;\n\
            gl_Position = vec4(clip, 0.0, 1.0);\n\
            // Fragment position in box-pixel space, centred: range\n\
            // [-half, +half]. The SDF below is evaluated in these units.\n\
            v_px = (unit01 - 0.5) * (u_half * 2.0);\n\
        }\n\0";

    let fs_src = b"#version 100\n\
        precision highp float;\n\
        varying vec2 v_px;\n\
        uniform vec2 u_half;\n\
        uniform float u_radius;\n\
        uniform float u_outline;\n\
        uniform vec4 u_inner;\n\
        uniform vec4 u_outer;\n\
        void main() {\n\
            // Rounded-box signed distance (Inigo Quilez's formula).\n\
            // q measures how far past the straight edges we are; adding\n\
            // back the corner radius rounds the corners. d < 0 inside.\n\
            vec2 q = abs(v_px) - (u_half - vec2(u_radius));\n\
            float d = length(max(q, 0.0)) + min(max(q.x, q.y), 0.0) - u_radius;\n\
    \n\
            // ~1px antialias band over the edge. GLES 2 core has no\n\
            // fwidth, so use a fixed 1px ramp -- the box is screen-sized,\n\
            // so 1px is the right scale (same smoothstep-AA approach as\n\
            // the indicator dot).\n\
            float aa = 1.0;\n\
            // Coverage inside the outer edge (the whole pill).\n\
            float outer_cov = 1.0 - smoothstep(-aa, 0.0, d);\n\
            // Coverage inside the *inner* edge (fill region only): the\n\
            // shape shrunk inward by the outline thickness.\n\
            float inner_cov = 1.0 - smoothstep(-aa, 0.0, d + u_outline);\n\
            // Outline is the ring between the two. With u_outline = 0 the\n\
            // two coverages coincide and the ring vanishes (fill only).\n\
            float outline_cov = clamp(outer_cov - inner_cov, 0.0, 1.0);\n\
    \n\
            // Composite: fill weighted by its own alpha and the inner\n\
            // coverage, then the outline laid over it by its coverage.\n\
            vec4 fill = vec4(u_inner.rgb, u_inner.a * inner_cov);\n\
            vec4 col = mix(fill, vec4(u_outer.rgb, u_outer.a), outline_cov);\n\
            if (col.a <= 0.0) discard;\n\
            // Premultiplied alpha -- same blend as the indicator.\n\
            gl_FragColor = vec4(col.rgb * col.a, col.a);\n\
        }\n\0";

    unsafe {
        let vs = compile_shader(gl::VERTEX_SHADER, vs_src);
        let fs = compile_shader(gl::FRAGMENT_SHADER, fs_src);
        let program = link_program(vs, fs);

        // Same unit quad as the other two programs. Independent VBO so the
        // programs stay decoupled; 48 bytes is free.
        let quad: [f32; 12] = [
            -1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0,
        ];

        let mut vbo: gl::types::GLuint = 0;
        gl::GenBuffers(1, &mut vbo);
        gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
        gl::BufferData(
            gl::ARRAY_BUFFER,
            std::mem::size_of_val(&quad) as isize,
            quad.as_ptr() as *const _,
            gl::STATIC_DRAW,
        );

        BoxProgram {
            program,
            vbo,
            rect_loc: gl::GetUniformLocation(program, b"u_rect\0".as_ptr() as *const _),
            half_loc: gl::GetUniformLocation(program, b"u_half\0".as_ptr() as *const _),
            radius_loc: gl::GetUniformLocation(program, b"u_radius\0".as_ptr() as *const _),
            outline_loc: gl::GetUniformLocation(program, b"u_outline\0".as_ptr() as *const _),
            inner_loc: gl::GetUniformLocation(program, b"u_inner\0".as_ptr() as *const _),
            outer_loc: gl::GetUniformLocation(program, b"u_outer\0".as_ptr() as *const _),
        }
    }
}
