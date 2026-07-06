// SPDX-License-Identifier: GPL-3.0-or-later

//! Reference plugin — slow-flowing looping color gradient.
//!
//! One full-buffer quad. The fragment shader projects each pixel onto
//! a gradient axis and maps the projection through a looping ramp of
//! four color stops. Two motions, both computed CPU-side in f64 and
//! uploaded as wrapped phases so the shader's f32 uniforms never see
//! unbounded time:
//!
//! - flow: the ramp slides along the axis (`u_phase` in `[0,1)`)
//! - drift (optional): the axis itself rotates slowly
//!
//! Fully opaque: emits `vec4(rgb, 1.0)`, no blending. The output is
//! dithered by half an 8-bit step — a slow near-flat ramp is the
//! worst-case banding scenario on ARGB8888.

use std::time::Instant;

use serde::Deserialize;
use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, gl as vgl};

const PLUGIN_NAME: &str = "gradient";

#[derive(Debug, Clone, Deserialize)]
struct Config {
    /// Ramp stops, 2-4 used. Fewer than 2 falls back to the default
    /// palette; stops past the fourth are ignored.
    #[serde(default = "default_colors")]
    colors: Vec<[f32; 3]>,
    /// Gradient axis direction in degrees. 0 = left-to-right. Screen
    /// y points down, so positive angles rotate clockwise on screen.
    #[serde(default = "default_angle_deg")]
    angle_deg: f32,
    /// Ramp cycles per minute. 0.25 = one full color loop every four
    /// minutes. 0 freezes the flow.
    #[serde(default = "default_speed")]
    speed: f32,
    /// Axis rotation in degrees per minute. 0 = fixed axis.
    #[serde(default)]
    rotate_deg_per_min: f32,
    /// Ramp lengths per screen-height of projected distance. 0.75
    /// shows about three quarters of the ramp across the short axis;
    /// smaller = broader, softer bands.
    #[serde(default = "default_scale")]
    scale: f32,
}

fn default_colors() -> Vec<[f32; 3]> {
    vec![
        [0.10, 0.16, 0.42], // rich indigo
        [0.38, 0.12, 0.48], // vivid purple
        [0.05, 0.36, 0.44], // bright teal
    ]
}
fn default_angle_deg() -> f32 {
    45.0
}
fn default_speed() -> f32 {
    0.25
}
fn default_scale() -> f32 {
    0.75
}

impl Default for Config {
    fn default() -> Self {
        Self {
            colors: default_colors(),
            angle_deg: default_angle_deg(),
            speed: default_speed(),
            rotate_deg_per_min: 0.0,
            scale: default_scale(),
        }
    }
}

/// Clamp a config float to a sane range. The config travels through
/// the host as JSON, so non-finite values are possible; they fall
/// back to the default instead of poisoning the phase math.
fn sane(x: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x.clamp(lo, hi)
    } else {
        fallback
    }
}

/// Sanitised animation parameters plus the start-of-life clock.
struct State {
    ramp: [[f32; 3]; 4],
    angle_deg: f64,
    rotate_deg_per_sec: f64,
    cycles_per_sec: f64,
    scale: f32,
    start: Instant,
}

impl State {
    fn new(config: &Config) -> Self {
        // Normalise the stop list to exactly four uniforms. Cycling
        // (i % len) keeps every padding smooth: 2 stops become
        // c0,c1,c0,c1 (a back-and-forth), 3 become c0,c1,c2,c0 with a
        // flat hold on the last segment (which blends back to c0).
        let mut stops: Vec<[f32; 3]> = config
            .colors
            .iter()
            .take(4)
            .map(|c| {
                [
                    sane(c[0], 0.0, 1.0, 0.0),
                    sane(c[1], 0.0, 1.0, 0.0),
                    sane(c[2], 0.0, 1.0, 0.0),
                ]
            })
            .collect();
        if stops.len() < 2 {
            stops = default_colors();
        }
        let ramp = std::array::from_fn(|i| stops[i % stops.len()]);

        let angle_deg = if config.angle_deg.is_finite() {
            f64::from(config.angle_deg).rem_euclid(360.0)
        } else {
            f64::from(default_angle_deg())
        };

        Self {
            ramp,
            angle_deg,
            rotate_deg_per_sec: f64::from(sane(config.rotate_deg_per_min, -360.0, 360.0, 0.0))
                / 60.0,
            cycles_per_sec: f64::from(sane(config.speed, 0.0, 30.0, default_speed())) / 60.0,
            scale: sane(config.scale, 0.05, 10.0, default_scale()),
            start: Instant::now(),
        }
    }
}

struct GpuState {
    program: gl::types::GLuint,
    u_c_locs: [gl::types::GLint; 4],
    u_dir_loc: gl::types::GLint,
    u_scale_loc: gl::types::GLint,
    u_phase_loc: gl::types::GLint,
    u_aspect_loc: gl::types::GLint,
}

unsafe fn build_gpu_state() -> Result<GpuState, String> {
    // Highp throughout — a mediump ramp this flat bands on Mesa.
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
        // Ramp stops. The ramp loops: the last segment blends back\n\
        // to u_c0, so fract() of the phase never pops.\n\
        uniform vec3 u_c0;\n\
        uniform vec3 u_c1;\n\
        uniform vec3 u_c2;\n\
        uniform vec3 u_c3;\n\
        // Gradient axis (unit vector). Screen y is down, so positive\n\
        // angles read as clockwise.\n\
        uniform vec2 u_dir;\n\
        // Ramp lengths per screen-height of projected distance.\n\
        uniform float u_scale;\n\
        // CPU-computed flow phase, already wrapped to [0,1). Only\n\
        // bounded values reach f32 here; raw elapsed seconds would\n\
        // lose fract() precision after long lock sessions.\n\
        uniform float u_phase;\n\
        // Buffer aspect (w/h). Applied to U so the axis angle is\n\
        // measured in physical pixels, not UV units.\n\
        uniform float u_aspect;\n\
        \n\
        void main() {\n\
            vec2 p = vec2(v_uv.x * u_aspect, v_uv.y);\n\
            float t = fract(dot(p, u_dir) * u_scale + u_phase);\n\
            // Unrolled 4-stop looping mix chain: GLSL ES 1.0 does not\n\
            // allow dynamic indexing of uniform arrays in the fragment\n\
            // shader. smoothstep clamps its input, so each segment is\n\
            // inert outside its [k, k+1) span of s.\n\
            float s = t * 4.0;\n\
            vec3 c = mix(u_c0, u_c1, smoothstep(0.0, 1.0, s));\n\
            c = mix(c, u_c2, smoothstep(1.0, 2.0, s));\n\
            c = mix(c, u_c3, smoothstep(2.0, 3.0, s));\n\
            c = mix(c, u_c0, smoothstep(3.0, 4.0, s));\n\
            // Hash dither, +/- half an 8-bit step: breaks up banding\n\
            // that an undithered slow ramp shows on ARGB8888.\n\
            float n = fract(sin(dot(gl_FragCoord.xy, vec2(12.9898, 78.233))) * 43758.5453);\n\
            c += (n - 0.5) / 255.0;\n\
            gl_FragColor = vec4(c, 1.0);\n\
        }\n\0";

    unsafe {
        let vs = vgl::compile_shader(gl::VERTEX_SHADER, vs_src)?;
        let fs = vgl::compile_shader(gl::FRAGMENT_SHADER, fs_src)?;
        let program = vgl::link_program(vs, fs)?;
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

        let a_pos = gl::GetAttribLocation(program, c"a_pos".as_ptr());
        gl::EnableVertexAttribArray(a_pos as u32);
        gl::VertexAttribPointer(a_pos as u32, 2, gl::FLOAT, gl::FALSE, 0, std::ptr::null());

        let u_c_locs = [c"u_c0", c"u_c1", c"u_c2", c"u_c3"]
            .map(|name| gl::GetUniformLocation(program, name.as_ptr()));
        let u_dir_loc = gl::GetUniformLocation(program, c"u_dir".as_ptr());
        let u_scale_loc = gl::GetUniformLocation(program, c"u_scale".as_ptr());
        let u_phase_loc = gl::GetUniformLocation(program, c"u_phase".as_ptr());
        let u_aspect_loc = gl::GetUniformLocation(program, c"u_aspect".as_ptr());

        Ok(GpuState {
            program,
            u_c_locs,
            u_dir_loc,
            u_scale_loc,
            u_phase_loc,
            u_aspect_loc,
        })
    }
}

fn run() -> Result<(), PluginError> {
    eprintln!(
        "veiland-{} (pid {}) starting",
        PLUGIN_NAME,
        std::process::id()
    );

    let config = veiland_plugin::load_config::<Config>(PLUGIN_NAME);
    eprintln!(
        "veiland-{}: config stops={} angle={} speed={}/min rotate={}/min scale={}",
        PLUGIN_NAME,
        config.colors.len(),
        config.angle_deg,
        config.speed,
        config.rotate_deg_per_min,
        config.scale,
    );

    let gbm_egl = GbmEgl::new()?;

    let mut conn = Connection::connect(PLUGIN_NAME, env!("CARGO_PKG_VERSION"))?;
    eprintln!("connected to host, hello sent");

    eprintln!(
        "sync model: {} (host_cap={}, plugin_cap={})",
        if conn.host_supports_fence_fd() && gbm_egl.supports_fence_fd() {
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
        "veiland-{}: first configure region=({},{}) {}x{} scale_120={}",
        PLUGIN_NAME,
        first_configure.region_x,
        first_configure.region_y,
        first_configure.region_w,
        first_configure.region_h,
        first_configure.scale_120,
    );

    let mut dma = DmaBuffer::new(&gbm_egl, first_configure.region_w, first_configure.region_h)?;
    eprintln!(
        "allocated {}x{} {:?}, modifier=0x{:016x}, stride={}",
        dma.width(),
        dma.height(),
        dma.format(),
        u64::from(dma.modifier()),
        dma.stride(),
    );

    dma.bind_for_rendering()?;
    let gpu = unsafe { build_gpu_state() }.map_err(|e| {
        eprintln!("veiland-{PLUGIN_NAME}: {e}");
        PluginError::Render("shader build failed")
    })?;

    let state = State::new(&config);

    // Self-paced: the gradient animates, so render again on every
    // BufferReleased and let the compositor's repaint rate drive it.
    let mut pacer = FramePacer::self_paced();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                render_and_send(&dma, &mut conn, &gbm_egl, &gpu, &state)?;
                pacer.submitted();
            }
            Frame::Reconfigure(c) => {
                dma.resize_or_keep(&gbm_egl, c.region_w, c.region_h, PLUGIN_NAME);
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
    conn: &mut Connection,
    gbm_egl: &GbmEgl,
    gpu: &GpuState,
    state: &State,
) -> Result<(), PluginError> {
    dma.bind_for_rendering()?;
    let aspect = dma.width() as f32 / dma.height() as f32;

    // Both phases wrapped in f64 before the f32 cast, so precision
    // holds for arbitrarily long lock sessions.
    let elapsed = state.start.elapsed().as_secs_f64();
    let phase = (elapsed * state.cycles_per_sec).fract() as f32;
    let theta = (state.angle_deg + elapsed * state.rotate_deg_per_sec)
        .rem_euclid(360.0)
        .to_radians();

    unsafe {
        gl::UseProgram(gpu.program);
        for (loc, c) in gpu.u_c_locs.iter().zip(state.ramp.iter()) {
            gl::Uniform3f(*loc, c[0], c[1], c[2]);
        }
        gl::Uniform2f(gpu.u_dir_loc, theta.cos() as f32, theta.sin() as f32);
        gl::Uniform1f(gpu.u_scale_loc, state.scale);
        gl::Uniform1f(gpu.u_phase_loc, phase);
        gl::Uniform1f(gpu.u_aspect_loc, aspect);

        // Opaque quad covering the whole buffer with alpha 1.0: no
        // clear and no blending needed.
        gl::DrawArrays(gl::TRIANGLES, 0, 6);
    }

    conn.submit_frame(dma, gbm_egl)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{}: {}", env!("CARGO_PKG_NAME"), e);
        std::process::exit(1);
    }
}
