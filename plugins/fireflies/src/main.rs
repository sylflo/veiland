// SPDX-License-Identifier: GPL-3.0-or-later

//! Reference plugin -- softly glowing wandering fireflies.
//!
//! Each firefly wanders through the region following a path built from two
//! layered sin waves per axis (different frequencies and phases, seeded per
//! firefly). The result is a slow organic Lissajous-style drift that never
//! strays outside a configurable radius around the firefly's home position.
//!
//! Each firefly blinks independently: brightness = max(0, sin(f*t+p))^3,
//! which spends most of the time near zero (dark) with brief sharp bright
//! peaks -- the classic firefly flash. Period ranges from 2 to 6 seconds.
//!
//! Geometry: one quad per firefly, 6 verts, 5 floats per vertex:
//!   px, py   -- clip-space corner
//!   lx, ly   -- local UV in [-1,+1]; FS uses it for the glow falloff
//!   fade     -- current brightness (blink * config alpha), 0..1
//!
//! The fragment shader is the same dreamy dot used by particles: a small
//! bright core + a wide soft glow halo. The glow dominates at low brightness
//! so fireflies leave a faint ambient haze before their peak flash.
//!
//! Cadence: self-paced, same as particles.

use serde::Deserialize;
use std::time::Instant;
use veiland_plugin::{
    Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, Rng, gl as vgl,
    math::px_to_clip,
};

const PLUGIN_NAME: &str = "fireflies";

/// Wander wave amplitude range, logical px. Each firefly has two waves per
/// axis; the total wander radius is up to 2 * WANDER_AMP_MAX.
const WANDER_AMP_MIN_PX: f32 = 30.0;
const WANDER_AMP_MAX_PX: f32 = 90.0;

/// Wander wave frequency range, Hz. Slow -- fireflies drift, not zip.
const WANDER_FREQ_MIN: f32 = 0.08;
const WANDER_FREQ_MAX: f32 = 0.35;

/// Blink frequency range, Hz (full sin cycle = one blink on + off).
const BLINK_FREQ_MIN: f32 = 0.18;
const BLINK_FREQ_MAX: f32 = 0.50;

/// Glow scale: quad is this many times larger than the core radius so the
/// halo has room to fall off inside it.
const GLOW_SCALE: f32 = 4.0;

#[derive(Debug, Clone, Deserialize)]
struct Config {
    #[serde(default = "default_count")]
    count: u32,
    /// Glow color + peak alpha. Default is classic warm yellow-green.
    #[serde(default = "default_color")]
    color: [f32; 4],
    /// Core radius in logical px at scale=1. The visible glow halo extends
    /// GLOW_SCALE times this from the centre.
    #[serde(default = "default_radius_px")]
    radius_px: f32,
    /// Controls how much of the blink cycle a firefly is visible.
    /// 1 = very brief sharp flash (sin^5), 0 = always softly on (sin^1).
    /// Default 0.4 gives a gentle glow that briefly brightens.
    #[serde(default = "default_flash_sharpness")]
    flash_sharpness: f32,
}

fn default_count() -> u32 {
    25
}
fn default_color() -> [f32; 4] {
    [0.72, 1.0, 0.18, 0.95]
}
fn default_radius_px() -> f32 {
    2.5
}
fn default_flash_sharpness() -> f32 {
    0.4
}

impl Default for Config {
    fn default() -> Self {
        Self {
            count: default_count(),
            color: default_color(),
            radius_px: default_radius_px(),
            flash_sharpness: default_flash_sharpness(),
        }
    }
}

/// Per-firefly constants. Everything is seeded once and fixed for life;
/// position and brightness are recomputed each frame from these seeds + time.
#[derive(Clone, Copy)]
struct Firefly {
    /// Home position as a fraction of surface width/height [0,1].
    x_home: f32,
    y_home: f32,

    /// Two wander waves per axis. Each wave: (amplitude_px, freq_hz, phase).
    wx: [(f32, f32, f32); 2],
    wy: [(f32, f32, f32); 2],

    /// Blink: frequency (Hz) and phase offset.
    blink_freq: f32,
    blink_phase: f32,
}

fn seed_fireflies(count: u32) -> Vec<Firefly> {
    let mut rng = Rng::new(0x6C624955); // arbitrary seed, distinct from siblings'
    (0..count).map(|_| {
        let wave = |rng: &mut Rng| -> (f32, f32, f32) {
            let amp = WANDER_AMP_MIN_PX
                + rng.next_f32() * (WANDER_AMP_MAX_PX - WANDER_AMP_MIN_PX);
            let freq = WANDER_FREQ_MIN
                + rng.next_f32() * (WANDER_FREQ_MAX - WANDER_FREQ_MIN);
            let phase = rng.next_f32() * std::f32::consts::TAU;
            (amp, freq, phase)
        };
        Firefly {
            x_home: rng.next_f32(),
            y_home: rng.next_f32(),
            wx: [wave(&mut rng), wave(&mut rng)],
            wy: [wave(&mut rng), wave(&mut rng)],
            blink_freq: BLINK_FREQ_MIN
                + rng.next_f32() * (BLINK_FREQ_MAX - BLINK_FREQ_MIN),
            blink_phase: rng.next_f32() * std::f32::consts::TAU,
        }
    }).collect()
}

struct GpuState {
    program: gl::types::GLuint,
    vbo: gl::types::GLuint,
    a_pos_loc: gl::types::GLuint,
    a_local_loc: gl::types::GLuint,
    a_fade_loc: gl::types::GLuint,
    u_color_loc: gl::types::GLint,
}

/// Build shader + VBO. Reuses the particles dreamy-dot shader verbatim:
/// bright core (smoothstep), wide glow halo (pow falloff), premultiplied alpha.
unsafe fn build_gpu_state() -> Result<GpuState, String> {
    let vs_src = b"#version 100\n\
        attribute vec2 a_pos;\n\
        attribute vec2 a_local;\n\
        attribute float a_fade;\n\
        varying vec2 v_local;\n\
        varying float v_fade;\n\
        void main() {\n\
            v_local = a_local;\n\
            v_fade = a_fade;\n\
            gl_Position = vec4(a_pos, 0.0, 1.0);\n\
        }\n\0";

    // Dreamy dot: tight bright core + wide pow halo. Fireflies use a wider
    // halo exponent (1.0 vs 1.2 in particles) so the ambient glow is more
    // visible between flashes -- you can see where they are even when dim.
    // Premultiplied alpha: emit rgb*a, host composites with ONE/1-SRC_ALPHA.
    let fs_src = b"#version 100\n\
        precision mediump float;\n\
        varying vec2 v_local;\n\
        varying float v_fade;\n\
        uniform vec4 u_color;\n\
        void main() {\n\
            float d = length(v_local);\n\
            float core = 1.0 - smoothstep(0.08, 0.18, d);\n\
            float glow = pow(1.0 - clamp(d, 0.0, 1.0), 1.0);\n\
            float cov = clamp(core + glow * 0.85, 0.0, 1.0);\n\
            float a = u_color.a * cov * v_fade;\n\
            if (a <= 0.0) discard;\n\
            gl_FragColor = vec4(u_color.rgb * a, a);\n\
        }\n\0";

    unsafe {
        let vs = vgl::compile_shader(gl::VERTEX_SHADER, vs_src)?;
        let fs = vgl::compile_shader(gl::FRAGMENT_SHADER, fs_src)?;
        let program = vgl::link_program(vs, fs)?;
        gl::UseProgram(program);

        let mut vbo: gl::types::GLuint = 0;
        gl::GenBuffers(1, &mut vbo);
        gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

        let a_pos_loc =
            gl::GetAttribLocation(program, c"a_pos".as_ptr()) as gl::types::GLuint;
        let a_local_loc =
            gl::GetAttribLocation(program, c"a_local".as_ptr()) as gl::types::GLuint;
        let a_fade_loc =
            gl::GetAttribLocation(program, c"a_fade".as_ptr()) as gl::types::GLuint;
        let u_color_loc =
            gl::GetUniformLocation(program, c"u_color".as_ptr());

        Ok(GpuState { program, vbo, a_pos_loc, a_local_loc, a_fade_loc, u_color_loc })
    }
}

struct State {
    config: Config,
    fireflies: Vec<Firefly>,
    /// 6 verts/firefly x 5 floats/vert.
    cpu_verts: Vec<f32>,
    scale_120: u32,
    start: Instant,
}

/// Fill cpu_verts for every firefly. Called per frame.
fn update_vertices(state: &mut State, surface_w: u32, surface_h: u32) {
    let now = state.start.elapsed().as_secs_f32();
    let w = surface_w as f32;
    let h = surface_h as f32;
    let scale = state.scale_120 as f32 / 120.0;
    let r = state.config.radius_px * scale * GLOW_SCALE;

    for (i, f) in state.fireflies.iter().enumerate() {
        // Wander: home + sum of two sin waves per axis.
        let wx = f.wx[0].0 * scale * (f.wx[0].1 * now + f.wx[0].2).sin()
               + f.wx[1].0 * scale * (f.wx[1].1 * now + f.wx[1].2).sin();
        let wy = f.wy[0].0 * scale * (f.wy[0].1 * now + f.wy[0].2).sin()
               + f.wy[1].0 * scale * (f.wy[1].1 * now + f.wy[1].2).sin();
        let cx_px = f.x_home * w + wx;
        let cy_px = f.y_home * h + wy;

        // Blink: sin^exp where exp is driven by flash_sharpness.
        // exp=1 (sharpness=0): gentle continuous glow.
        // exp=5 (sharpness=1): very brief sharp flash, mostly dark.
        // max(0,...) keeps brightness non-negative.
        let exp = 1 + (state.config.flash_sharpness.clamp(0.0, 1.0) * 4.0).round() as i32;
        let s = (f.blink_freq * std::f32::consts::TAU * now + f.blink_phase).sin();
        let brightness = s.max(0.0).powi(exp);

        let (x0, y0) = (cx_px - r, cy_px - r);
        let (x1, y1) = (cx_px + r, cy_px - r);
        let (x2, y2) = (cx_px - r, cy_px + r);
        let (x3, y3) = (cx_px + r, cy_px + r);

        let (cx0, cy0) = px_to_clip(x0, y0, w, h);
        let (cx1, cy1) = px_to_clip(x1, y1, w, h);
        let (cx2, cy2) = px_to_clip(x2, y2, w, h);
        let (cx3, cy3) = px_to_clip(x3, y3, w, h);

        let off = i * 6 * 5;
        let v = &mut state.cpu_verts[off..off + 30];

        v[0]  = cx0; v[1]  = cy0; v[2]  = -1.0; v[3]  = -1.0; v[4]  = brightness;
        v[5]  = cx1; v[6]  = cy1; v[7]  =  1.0; v[8]  = -1.0; v[9]  = brightness;
        v[10] = cx2; v[11] = cy2; v[12] = -1.0; v[13] =  1.0; v[14] = brightness;
        v[15] = cx1; v[16] = cy1; v[17] =  1.0; v[18] = -1.0; v[19] = brightness;
        v[20] = cx3; v[21] = cy3; v[22] =  1.0; v[23] =  1.0; v[24] = brightness;
        v[25] = cx2; v[26] = cy2; v[27] = -1.0; v[28] =  1.0; v[29] = brightness;
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
        "veiland-{}: config count={} color={:?} radius_px={} flash_sharpness={}",
        PLUGIN_NAME, config.count, config.color, config.radius_px, config.flash_sharpness
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

    let fireflies = seed_fireflies(config.count);
    let cpu_verts = vec![0.0_f32; fireflies.len() * 6 * 5];

    let mut state = State {
        config,
        fireflies,
        cpu_verts,
        scale_120: first_configure.scale_120,
        start: Instant::now(),
    };

    let mut pacer = FramePacer::self_paced();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                render_and_send(&dma, &mut conn, &gbm_egl, &gpu, &mut state)?;
                pacer.submitted();
            }
            Frame::Reconfigure(c) => {
                dma.resize_or_keep(&gbm_egl, c.region_w, c.region_h, PLUGIN_NAME);
                state.scale_120 = c.scale_120;
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
    state: &mut State,
) -> Result<(), PluginError> {
    dma.bind_for_rendering()?;
    let (w, h) = (dma.width(), dma.height());

    update_vertices(state, w, h);

    unsafe {
        gl::ClearColor(0.0, 0.0, 0.0, 0.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);

        gl::Enable(gl::BLEND);
        gl::BlendFunc(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);

        gl::UseProgram(gpu.program);
        gl::BindBuffer(gl::ARRAY_BUFFER, gpu.vbo);
        gl::BufferData(
            gl::ARRAY_BUFFER,
            std::mem::size_of_val(&state.cpu_verts[..]) as isize,
            state.cpu_verts.as_ptr() as *const _,
            gl::STREAM_DRAW,
        );

        let stride = (5 * std::mem::size_of::<f32>()) as i32;
        let off = |n: usize| (n * std::mem::size_of::<f32>()) as *const _;

        gl::EnableVertexAttribArray(gpu.a_pos_loc);
        gl::VertexAttribPointer(gpu.a_pos_loc, 2, gl::FLOAT, gl::FALSE, stride, off(0));

        gl::EnableVertexAttribArray(gpu.a_local_loc);
        gl::VertexAttribPointer(gpu.a_local_loc, 2, gl::FLOAT, gl::FALSE, stride, off(2));

        gl::EnableVertexAttribArray(gpu.a_fade_loc);
        gl::VertexAttribPointer(gpu.a_fade_loc, 1, gl::FLOAT, gl::FALSE, stride, off(4));

        gl::Uniform4f(
            gpu.u_color_loc,
            state.config.color[0],
            state.config.color[1],
            state.config.color[2],
            state.config.color[3],
        );

        gl::DrawArrays(gl::TRIANGLES, 0, (state.fireflies.len() * 6) as i32);
    }

    conn.submit_frame(dma, gbm_egl)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{}: {}", env!("CARGO_PKG_NAME"), e);
        std::process::exit(1);
    }
}
