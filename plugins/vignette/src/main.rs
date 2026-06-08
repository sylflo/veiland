// SPDX-License-Identifier: GPL-3.0-or-later

//! M11 reference plugin — four-corner radial-gradient vignette.
//!
//! One full-buffer quad. Fragment shader computes per-pixel coverage
//! from four corner radial-gradient terms (one per corner), sums
//! them, clamps to `[0,1]`, and outputs the configured colour with
//! that coverage as alpha. Composited on top of the wallpaper and
//! below text — darkens the corners, leaves the centre clear.
//!
//! No animation. After the first render the buffer is static; the
//! plugin re-renders on every FrameDone for protocol correctness
//! but the content doesn't change unless Configure brings a new
//! scale or region.
//!
//! `precision highp float` everywhere — `mediump` Mesa can band on
//! the smoothstep sum at low gradient values (per m11-plan.md Risks).

use serde::Deserialize;
use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, SyncFence};
use veiland_protocol::Buffer;

const PLUGIN_NAME: &str = "vignette";

#[derive(Debug, Clone, Deserialize)]
struct Config {
    #[serde(default = "default_color")]
    color: [f32; 4],
    #[serde(default = "default_corner_opacity")]
    opacity_top_left: f32,
    #[serde(default = "default_corner_opacity")]
    opacity_top_right: f32,
    #[serde(default = "default_bottom_corner_opacity")]
    opacity_bottom_left: f32,
    #[serde(default = "default_bottom_corner_opacity")]
    opacity_bottom_right: f32,
    /// Fraction of the half-diagonal (after aspect correction) at
    /// which a corner's contribution falls to zero. `0.7` matches
    /// the Shinkai mockup. Larger → vignette reaches further into
    /// the centre. Smaller → only the very corners darken.
    #[serde(default = "default_radius")]
    radius: f32,
    /// Uniform dim applied across the WHOLE image, under the corner
    /// gradient. `0.0` = classic corners-only vignette; small values
    /// (e.g. 0.15-0.3) give a soft, dreamy haze over everything. Clamped
    /// with the corners so it never exceeds full `color` opacity.
    #[serde(default = "default_base_opacity")]
    base_opacity: f32,
}

fn default_color() -> [f32; 4] {
    [0.10, 0.14, 0.20, 1.0]
}
fn default_corner_opacity() -> f32 {
    0.6
}
fn default_bottom_corner_opacity() -> f32 {
    0.7
}
fn default_radius() -> f32 {
    0.7
}
fn default_base_opacity() -> f32 {
    // Off by default — preserves the classic corners-only vignette for
    // configs that don't ask for the haze.
    0.0
}

fn default_config() -> Config {
    Config {
        color: default_color(),
        opacity_top_left: default_corner_opacity(),
        opacity_top_right: default_corner_opacity(),
        opacity_bottom_left: default_bottom_corner_opacity(),
        opacity_bottom_right: default_bottom_corner_opacity(),
        radius: default_radius(),
        base_opacity: default_base_opacity(),
    }
}

fn load_config() -> Config {
    match std::env::var("VEILAND_PLUGIN_CONFIG") {
        Ok(s) => match serde_json::from_str::<Config>(&s) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "veiland-{}: failed to parse VEILAND_PLUGIN_CONFIG as JSON: {} \
                     — falling back to defaults",
                    PLUGIN_NAME, e
                );
                default_config()
            }
        },
        Err(_) => {
            eprintln!(
                "veiland-{}: VEILAND_PLUGIN_CONFIG unset — using defaults",
                PLUGIN_NAME
            );
            default_config()
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

struct GpuState {
    program: gl::types::GLuint,
    u_color_loc: gl::types::GLint,
    u_opacities_loc: gl::types::GLint,
    u_base_loc: gl::types::GLint,
    u_radius_loc: gl::types::GLint,
    u_aspect_loc: gl::types::GLint,
}

unsafe fn build_gpu_state() -> GpuState {
    // Highp throughout — mediump on Mesa can band the sum of four
    // smoothsteps at small gradient values (m11-plan.md Risks).
    let vs_src = b"#version 100\n\
        precision highp float;\n\
        attribute vec2 a_pos;\n\
        varying vec2 v_uv;\n\
        void main() {\n\
            v_uv = a_pos * 0.5 + 0.5;\n\
            gl_Position = vec4(a_pos, 0.0, 1.0);\n\
        }\n\0";

    let fs_src = b"#version 100\n\
        precision highp float;\n\
        varying vec2 v_uv;\n\
        uniform vec4 u_color;\n\
        // Per-corner opacities. xyzw = TL, TR, BL, BR.\n\
        uniform vec4 u_opacities;\n\
        // Uniform whole-image dim added under the corner gradient. 0.0 =\n\
        // corners only (classic vignette); >0 darkens the entire image\n\
        // evenly for a soft, dreamy haze.\n\
        uniform float u_base;\n\
        uniform float u_radius;\n\
        // Buffer aspect ratio (w/h). Applied to U so a `radius` of\n\
        // 0.7 reads as 70% of the half-diagonal in physical pixels,\n\
        // not 70% of the UV unit (which would be elliptical on a\n\
        // 1920x1080 buffer).\n\
        uniform float u_aspect;\n\
        \n\
        // Coverage at the current pixel for a corner at `corner_uv`.\n\
        // 1.0 at the corner, falls to 0.0 at distance `u_radius`.\n\
        float corner_coverage(vec2 corner_uv) {\n\
            // Aspect-correct: stretch U so the metric is isotropic.\n\
            vec2 d = vec2((v_uv.x - corner_uv.x) * u_aspect,\n\
                          (v_uv.y - corner_uv.y));\n\
            float dist = length(d);\n\
            // smoothstep gives a soft falloff; (1.0 - x) inverts so\n\
            // 0 distance -> 1, radius -> 0.\n\
            return 1.0 - smoothstep(0.0, u_radius, dist);\n\
        }\n\
        \n\
        void main() {\n\
            float tl = corner_coverage(vec2(0.0, 0.0)) * u_opacities.x;\n\
            float tr = corner_coverage(vec2(1.0, 0.0)) * u_opacities.y;\n\
            float bl = corner_coverage(vec2(0.0, 1.0)) * u_opacities.z;\n\
            float br = corner_coverage(vec2(1.0, 1.0)) * u_opacities.w;\n\
            // Base dim everywhere, then the corner gradient on top.\n\
            float a = clamp(u_base + tl + tr + bl + br, 0.0, 1.0);\n\
            // Premultiplied alpha: the core composites this dmabuf with\n\
            // glBlendFunc(ONE, 1-SRC_ALPHA), so emit RGB pre-scaled by\n\
            // the final alpha.\n\
            float pa = a * u_color.a;\n\
            gl_FragColor = vec4(u_color.rgb * pa, pa);\n\
        }\n\0";

    unsafe {
        let vs = compile_shader(gl::VERTEX_SHADER, vs_src);
        let fs = compile_shader(gl::FRAGMENT_SHADER, fs_src);
        let program = link_program(vs, fs);
        gl::UseProgram(program);

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

        let a_pos = gl::GetAttribLocation(program, b"a_pos\0".as_ptr() as *const _);
        gl::EnableVertexAttribArray(a_pos as u32);
        gl::VertexAttribPointer(a_pos as u32, 2, gl::FLOAT, gl::FALSE, 0, std::ptr::null());

        let u_color_loc = gl::GetUniformLocation(program, b"u_color\0".as_ptr() as *const _);
        let u_opacities_loc =
            gl::GetUniformLocation(program, b"u_opacities\0".as_ptr() as *const _);
        let u_base_loc = gl::GetUniformLocation(program, b"u_base\0".as_ptr() as *const _);
        let u_radius_loc = gl::GetUniformLocation(program, b"u_radius\0".as_ptr() as *const _);
        let u_aspect_loc = gl::GetUniformLocation(program, b"u_aspect\0".as_ptr() as *const _);

        GpuState {
            program,
            u_color_loc,
            u_opacities_loc,
            u_base_loc,
            u_radius_loc,
            u_aspect_loc,
        }
    }
}

struct State {
    config: Config,
}

fn run() -> Result<(), PluginError> {
    eprintln!(
        "veiland-{} (pid {}) starting",
        PLUGIN_NAME,
        std::process::id()
    );

    let config = load_config();
    eprintln!(
        "veiland-{}: config color={:?} corners=[{},{},{},{}] radius={}",
        PLUGIN_NAME,
        config.color,
        config.opacity_top_left,
        config.opacity_top_right,
        config.opacity_bottom_left,
        config.opacity_bottom_right,
        config.radius,
    );

    let gbm_egl = GbmEgl::new()?;

    // Connect preamble (from_env + handshake + hello) in one call.
    let mut conn = Connection::connect(PLUGIN_NAME, env!("CARGO_PKG_VERSION"))?;
    eprintln!("connected to host, hello sent");

    let fast_path = conn.host_supports_fence_fd() && gbm_egl.supports_fence_fd();
    eprintln!(
        "sync model: {} (host_cap={}, plugin_cap={})",
        if fast_path {
            "fast (fence fd)"
        } else {
            "slow (glFinish)"
        },
        conn.host_supports_fence_fd(),
        gbm_egl.supports_fence_fd(),
    );

    let first_configure = match conn.wait_for_configure()? {
        Some(c) => c,
        None => {
            eprintln!("veiland-{}: shutdown before first configure", PLUGIN_NAME);
            return Ok(());
        }
    };
    eprintln!(
        "veiland-{}: first configure region=({},{}) {}x{} scale={}",
        PLUGIN_NAME,
        first_configure.region_x,
        first_configure.region_y,
        first_configure.region_w,
        first_configure.region_h,
        first_configure.scale,
    );

    let dma = DmaBuffer::new(&gbm_egl, first_configure.region_w, first_configure.region_h)?;
    eprintln!(
        "allocated {}x{} {:?}, modifier=0x{:016x}, stride={}",
        dma.width(),
        dma.height(),
        dma.format(),
        u64::from(dma.modifier()),
        dma.stride(),
    );

    dma.bind_for_rendering()?;
    let gpu = unsafe { build_gpu_state() };

    let mut state = State { config };

    let buf_msg = Buffer {
        id: 0,
        width: dma.width(),
        height: dma.height(),
        format: dma.format(),
        modifier: dma.modifier(),
        stride: dma.stride(),
        offset: 0,
    };

    // On-demand: the vignette is static — it redraws only when the host
    // asks (FrameDone). FramePacer owns the deferral state machine.
    let mut pacer = FramePacer::on_demand();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                render_and_send(
                    &dma, &gbm_egl, &mut conn, &buf_msg, &gpu, &mut state, fast_path,
                )?;
                pacer.submitted();
            }
            Frame::Reconfigure(c) => {
                if c.region_w != dma.width() || c.region_h != dma.height() {
                    eprintln!(
                        "veiland-{}: configure region {}x{} differs from initial {}x{}; \
                         keeping initial buffer size",
                        PLUGIN_NAME,
                        c.region_w,
                        c.region_h,
                        dma.width(),
                        dma.height(),
                    );
                }
            }
            Frame::Shutdown => {
                eprintln!("host requested shutdown");
                return Ok(());
            }
        }
    }
}

fn render_and_send(
    dma: &DmaBuffer,
    gbm_egl: &GbmEgl,
    conn: &mut Connection,
    buf_msg: &Buffer,
    gpu: &GpuState,
    state: &mut State,
    fast_path: bool,
) -> Result<(), PluginError> {
    dma.bind_for_rendering()?;
    let (w, h) = (dma.width(), dma.height());
    let aspect = w as f32 / h as f32;

    unsafe {
        gl::Viewport(0, 0, w as i32, h as i32);
        gl::ClearColor(0.0, 0.0, 0.0, 0.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);

        gl::Enable(gl::BLEND);
        // Premultiplied-alpha over operator; FS emits RGB*a. See fs_src.
        gl::BlendFunc(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);

        gl::UseProgram(gpu.program);
        gl::Uniform4f(
            gpu.u_color_loc,
            state.config.color[0],
            state.config.color[1],
            state.config.color[2],
            state.config.color[3],
        );
        gl::Uniform4f(
            gpu.u_opacities_loc,
            state.config.opacity_top_left,
            state.config.opacity_top_right,
            state.config.opacity_bottom_left,
            state.config.opacity_bottom_right,
        );
        gl::Uniform1f(gpu.u_base_loc, state.config.base_opacity);
        gl::Uniform1f(gpu.u_radius_loc, state.config.radius);
        gl::Uniform1f(gpu.u_aspect_loc, aspect);

        gl::DrawArrays(gl::TRIANGLES, 0, 6);
    }

    if fast_path {
        unsafe {
            gl::Flush();
        }
        let fence = SyncFence::create(gbm_egl)?;
        conn.send_buffer(buf_msg, dma.dmabuf_fd(), Some(fence.as_fd()))?;
    } else {
        dma.finish();
        conn.send_buffer(buf_msg, dma.dmabuf_fd(), None)?;
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{}: {}", env!("CARGO_PKG_NAME"), e);
        std::process::exit(1);
    }
}
