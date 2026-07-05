// SPDX-License-Identifier: GPL-3.0-or-later

//! Reference plugin -- fire particle overlay.
//!
//! Reproduces the classic "fire particles overlay" look: a warm orange glow
//! along the bottom edge of the region, with bright sparks rising from it,
//! curving slightly, and fading out as they climb.
//!
//! Two render passes, one draw call each:
//!
//!   Pass 1 -- bottom glow. A single fullscreen quad with a vertical gradient
//!   that is bright orange-red at y=0 (bottom of region) and fully transparent
//!   by y=GLOW_HEIGHT_FRAC (fraction of region height). Drawn once per frame
//!   with no per-frame CPU work.
//!
//!   Pass 2 -- spark particles. Each spark spawns at a random X near the
//!   bottom, rises quickly, drifts slightly sideways with a gentle curve, and
//!   fades out before leaving the top of the region. Size varies per spark
//!   (small dots to slightly larger embers). Color is bright orange-yellow;
//!   a soft glow halo is added in the FS.
//!
//! Vertex layout:
//!   Glow quad : 4 verts x 3 floats (px, py, gradient_t). Two triangles.
//!   Sparks    : 6 verts/spark x 5 floats (px, py, lx, ly, fade).
//!
//! Cadence: self-paced, same as particles.

use serde::Deserialize;
use std::time::Instant;
use veiland_plugin::{
    Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, Rng, gl as vgl,
    math::px_to_clip,
};

const PLUGIN_NAME: &str = "embers";

// ---------------------------------------------------------------------------
// Spark motion constants
// ---------------------------------------------------------------------------

/// Rise-cycle range per spark, seconds. Fast -- sparks shoot upward urgently.
const CYCLE_MIN_SECONDS: f32 = 1.2;
const CYCLE_MAX_SECONDS: f32 = 3.0;

/// Sparks spawn within this fraction of the region height from the bottom.
/// Keeps them coming from the fire band, not mid-air.
const SPAWN_HEIGHT_FRAC: f32 = 0.08;

/// Max horizontal drift over one full rise, logical px.
const DRIFT_MAX_PX: f32 = 60.0;

/// Gentle sideways wobble amplitude, logical px.
const WOBBLE_PX: f32 = 12.0;

/// Spark radius range in logical px at scale=1. Small range gives natural
/// size variance without any spark dominating.
const RADIUS_MIN_PX: f32 = 1.2;
const RADIUS_MAX_PX: f32 = 4.0;

/// Glow quad drawn GLOW_SCALE bigger than the spark radius so the halo fits.
const GLOW_SCALE: f32 = 3.5;

/// Peak spark opacity. Bright but not fully opaque so they blend nicely.
const PEAK_OPACITY: f32 = 0.95;

/// Fraction of the rise for fade-in at the bottom and fade-out at the top.
const FADE_FRACTION: f32 = 0.15;

// ---------------------------------------------------------------------------
// Bottom glow constants
// ---------------------------------------------------------------------------

/// How tall the bottom glow band is, as a fraction of region height.
/// 0.30 = bottom 30% of the screen glows.
const GLOW_HEIGHT_FRAC: f32 = 0.30;

/// Peak glow alpha at the very bottom edge.
const GLOW_PEAK_ALPHA: f32 = 0.55;

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Deserialize)]
struct Config {
    #[serde(default = "default_count")]
    count: u32,
    /// Spark color (hot core). The glow halo uses the same color at lower
    /// opacity. The bottom gradient uses this color darkened toward the base
    /// color.
    #[serde(default = "default_spark_color")]
    spark_color: [f32; 4],
    /// Bottom glow color. Defaults to a deep red-orange.
    #[serde(default = "default_glow_color")]
    glow_color: [f32; 3],
}

fn default_count() -> u32 {
    80
}
fn default_spark_color() -> [f32; 4] {
    [1.0, 0.65, 0.10, 1.0]
}
fn default_glow_color() -> [f32; 3] {
    [0.80, 0.18, 0.02]
}

impl Default for Config {
    fn default() -> Self {
        Self {
            count: default_count(),
            spark_color: default_spark_color(),
            glow_color: default_glow_color(),
        }
    }
}

// ---------------------------------------------------------------------------
// Per-spark seed data
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Spark {
    x_norm: f32,
    /// Y spawn position as a fraction of region height from the bottom.
    y_spawn_frac: f32,
    t_offset: f32,
    cycle: f32,
    drift_px: f32,
    wobble_phase: f32,
    radius_px: f32,
}

fn seed_sparks(count: u32) -> Vec<Spark> {
    let mut rng = Rng::new(0xB7E15163);
    (0..count)
        .map(|_| {
            let cycle =
                CYCLE_MIN_SECONDS + rng.next_f32() * (CYCLE_MAX_SECONDS - CYCLE_MIN_SECONDS);
            Spark {
                x_norm: rng.next_f32(),
                y_spawn_frac: rng.next_f32() * SPAWN_HEIGHT_FRAC,
                t_offset: rng.next_f32() * cycle,
                cycle,
                drift_px: (rng.next_f32() * 2.0 - 1.0) * DRIFT_MAX_PX,
                wobble_phase: rng.next_f32() * std::f32::consts::TAU,
                radius_px: RADIUS_MIN_PX + rng.next_f32() * (RADIUS_MAX_PX - RADIUS_MIN_PX),
            }
        })
        .collect()
}

// ---------------------------------------------------------------------------
// GPU state -- two programs
// ---------------------------------------------------------------------------

struct GpuState {
    // Pass 1: bottom glow
    glow_program: gl::types::GLuint,
    glow_vbo: gl::types::GLuint,
    glow_a_pos_loc: gl::types::GLuint,
    glow_a_t_loc: gl::types::GLuint,
    glow_u_color_loc: gl::types::GLint,
    glow_u_peak_loc: gl::types::GLint,

    // Pass 2: sparks
    spark_program: gl::types::GLuint,
    spark_vbo: gl::types::GLuint,
    spark_a_pos_loc: gl::types::GLuint,
    spark_a_local_loc: gl::types::GLuint,
    spark_a_fade_loc: gl::types::GLuint,
    spark_u_color_loc: gl::types::GLint,
}

unsafe fn build_gpu_state() -> Result<GpuState, String> {
    // ------------------------------------------------------------------
    // Pass 1: bottom glow quad
    // Per vertex: px, py, gradient_t (0=bottom/opaque, 1=top/transparent).
    // FS: smooth falloff so the glow fades naturally upward.
    // ------------------------------------------------------------------
    let glow_vs = b"#version 100\n\
        attribute vec2 a_pos;\n\
        attribute float a_t;\n\
        varying float v_t;\n\
        void main() {\n\
            v_t = a_t;\n\
            gl_Position = vec4(a_pos, 0.0, 1.0);\n\
        }\n\0";

    // Exponential-ish falloff: bright at the base, smooth fade upward.
    // Premultiplied alpha.
    let glow_fs = b"#version 100\n\
        precision mediump float;\n\
        varying float v_t;\n\
        uniform vec3 u_color;\n\
        uniform float u_peak;\n\
        void main() {\n\
            float a = u_peak * pow(1.0 - v_t, 2.2);\n\
            if (a <= 0.0) discard;\n\
            gl_FragColor = vec4(u_color * a, a);\n\
        }\n\0";

    // ------------------------------------------------------------------
    // Pass 2: spark particles
    // Per vertex: px, py, lx, ly (local [-1,+1]), fade.
    // FS: bright core + wide soft glow halo. Premultiplied alpha.
    // ------------------------------------------------------------------
    let spark_vs = b"#version 100\n\
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

    // Same dreamy dot formula as particles but warmer: core is near-white,
    // halo fades to the configured spark color.
    let spark_fs = b"#version 100\n\
        precision mediump float;\n\
        varying vec2 v_local;\n\
        varying float v_fade;\n\
        uniform vec4 u_color;\n\
        void main() {\n\
            float d = length(v_local);\n\
            float core = 1.0 - smoothstep(0.08, 0.18, d);\n\
            float glow = pow(1.0 - clamp(d, 0.0, 1.0), 1.4);\n\
            float cov = clamp(core + glow * 0.75, 0.0, 1.0);\n\
            float a = u_color.a * cov * v_fade;\n\
            if (a <= 0.0) discard;\n\
            vec3 rgb = mix(u_color.rgb, vec3(1.0, 0.95, 0.6), core * 0.6);\n\
            gl_FragColor = vec4(rgb * a, a);\n\
        }\n\0";

    unsafe {
        // Glow program
        let gvs = vgl::compile_shader(gl::VERTEX_SHADER, glow_vs)?;
        let gfs = vgl::compile_shader(gl::FRAGMENT_SHADER, glow_fs)?;
        let glow_program = vgl::link_program(gvs, gfs)?;

        let mut glow_vbo: gl::types::GLuint = 0;
        gl::GenBuffers(1, &mut glow_vbo);

        let glow_a_pos_loc =
            gl::GetAttribLocation(glow_program, c"a_pos".as_ptr()) as gl::types::GLuint;
        let glow_a_t_loc =
            gl::GetAttribLocation(glow_program, c"a_t".as_ptr()) as gl::types::GLuint;
        let glow_u_color_loc =
            gl::GetUniformLocation(glow_program, c"u_color".as_ptr());
        let glow_u_peak_loc =
            gl::GetUniformLocation(glow_program, c"u_peak".as_ptr());

        // Spark program
        let svs = vgl::compile_shader(gl::VERTEX_SHADER, spark_vs)?;
        let sfs = vgl::compile_shader(gl::FRAGMENT_SHADER, spark_fs)?;
        let spark_program = vgl::link_program(svs, sfs)?;

        let mut spark_vbo: gl::types::GLuint = 0;
        gl::GenBuffers(1, &mut spark_vbo);

        let spark_a_pos_loc =
            gl::GetAttribLocation(spark_program, c"a_pos".as_ptr()) as gl::types::GLuint;
        let spark_a_local_loc =
            gl::GetAttribLocation(spark_program, c"a_local".as_ptr()) as gl::types::GLuint;
        let spark_a_fade_loc =
            gl::GetAttribLocation(spark_program, c"a_fade".as_ptr()) as gl::types::GLuint;
        let spark_u_color_loc =
            gl::GetUniformLocation(spark_program, c"u_color".as_ptr());

        Ok(GpuState {
            glow_program,
            glow_vbo,
            glow_a_pos_loc,
            glow_a_t_loc,
            glow_u_color_loc,
            glow_u_peak_loc,
            spark_program,
            spark_vbo,
            spark_a_pos_loc,
            spark_a_local_loc,
            spark_a_fade_loc,
            spark_u_color_loc,
        })
    }
}

// ---------------------------------------------------------------------------
// State + vertex update
// ---------------------------------------------------------------------------

struct State {
    config: Config,
    sparks: Vec<Spark>,
    /// Glow quad: 4 verts x 3 floats (px, py, t). Static after init.
    glow_verts: [f32; 12],
    /// Spark verts: 6 verts/spark x 5 floats.
    spark_verts: Vec<f32>,
    scale_120: u32,
    start: Instant,
    /// Whether glow_verts needs reuploading (on first frame and Reconfigure).
    glow_dirty: bool,
}

/// Rebuild the glow quad to match current surface dimensions.
/// The quad covers the full width and the bottom GLOW_HEIGHT_FRAC of height.
/// gradient_t=0 at the bottom edge (peak glow), =1 at the top of the band.
fn rebuild_glow_verts(state: &mut State, w: u32, h: u32) {
    let wf = w as f32;
    let hf = h as f32;

    // Bottom edge of region in pixel space: y = h (clip -1).
    // Top of glow band: y = h - h*GLOW_HEIGHT_FRAC = h*(1-GLOW_HEIGHT_FRAC).
    let y_bottom = hf;
    let y_top = hf * (1.0 - GLOW_HEIGHT_FRAC);

    let (x0, _) = px_to_clip(0.0, y_bottom, wf, hf);
    let (x1, _) = px_to_clip(wf, y_bottom, wf, hf);
    let (_, cy_bot) = px_to_clip(0.0, y_bottom, wf, hf);
    let (_, cy_top) = px_to_clip(0.0, y_top, wf, hf);

    // 4 verts: bottom-left, bottom-right, top-left, top-right.
    // t=0 at bottom (opaque), t=1 at top (transparent).
    state.glow_verts = [
        x0, cy_bot, 0.0,
        x1, cy_bot, 0.0,
        x0, cy_top, 1.0,
        x1, cy_top, 1.0,
    ];
    state.glow_dirty = true;
}

/// Fill spark_verts for every spark. Called per frame.
fn update_spark_verts(state: &mut State, surface_w: u32, surface_h: u32) {
    let now = state.start.elapsed().as_secs_f32();
    let w = surface_w as f32;
    let h = surface_h as f32;
    let scale = state.scale_120 as f32 / 120.0;

    for (i, s) in state.sparks.iter().enumerate() {
        let phase = ((now + s.t_offset) % s.cycle) / s.cycle;

        // Spark rises from y_spawn upward. phase=0: at spawn, phase=1: top.
        let y_spawn = h - s.y_spawn_frac * h;
        let travel = y_spawn + s.radius_px * scale * GLOW_SCALE;
        let cy_px = y_spawn - phase * travel;

        // Horizontal drift + wobble.
        let drift = s.drift_px * scale * phase;
        let wobble = WOBBLE_PX * scale
            * (phase * std::f32::consts::TAU * 1.5 + s.wobble_phase).sin();
        let cx_px = s.x_norm * w + drift + wobble;

        // Fade: in over first FADE_FRACTION, out over last FADE_FRACTION.
        let fade = if phase < FADE_FRACTION {
            phase / FADE_FRACTION
        } else if phase > 1.0 - FADE_FRACTION {
            (1.0 - phase) / FADE_FRACTION
        } else {
            1.0
        };
        let alpha = fade.clamp(0.0, 1.0) * PEAK_OPACITY;

        let r = s.radius_px * scale * GLOW_SCALE;
        let (x0, y0) = (cx_px - r, cy_px - r);
        let (x1, y1) = (cx_px + r, cy_px - r);
        let (x2, y2) = (cx_px - r, cy_px + r);
        let (x3, y3) = (cx_px + r, cy_px + r);

        let (cx0, cy0) = px_to_clip(x0, y0, w, h);
        let (cx1, cy1) = px_to_clip(x1, y1, w, h);
        let (cx2, cy2) = px_to_clip(x2, y2, w, h);
        let (cx3, cy3) = px_to_clip(x3, y3, w, h);

        let off = i * 6 * 5;
        let v = &mut state.spark_verts[off..off + 30];

        // 5 floats/vert: px, py, lx, ly, fade.
        v[0]  = cx0; v[1]  = cy0; v[2]  = -1.0; v[3]  = -1.0; v[4]  = alpha;
        v[5]  = cx1; v[6]  = cy1; v[7]  =  1.0; v[8]  = -1.0; v[9]  = alpha;
        v[10] = cx2; v[11] = cy2; v[12] = -1.0; v[13] =  1.0; v[14] = alpha;
        v[15] = cx1; v[16] = cy1; v[17] =  1.0; v[18] = -1.0; v[19] = alpha;
        v[20] = cx3; v[21] = cy3; v[22] =  1.0; v[23] =  1.0; v[24] = alpha;
        v[25] = cx2; v[26] = cy2; v[27] = -1.0; v[28] =  1.0; v[29] = alpha;
    }
}

// ---------------------------------------------------------------------------
// Main loop
// ---------------------------------------------------------------------------

fn run() -> Result<(), PluginError> {
    eprintln!(
        "veiland-{} (pid {}) starting",
        PLUGIN_NAME,
        std::process::id()
    );

    let config = veiland_plugin::load_config::<Config>(PLUGIN_NAME);
    eprintln!(
        "veiland-{}: config count={} spark_color={:?} glow_color={:?}",
        PLUGIN_NAME, config.count, config.spark_color, config.glow_color
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

    let sparks = seed_sparks(config.count);
    let spark_verts = vec![0.0_f32; sparks.len() * 6 * 5];

    let mut state = State {
        config,
        sparks,
        glow_verts: [0.0; 12],
        spark_verts,
        scale_120: first_configure.scale_120,
        start: Instant::now(),
        glow_dirty: true,
    };
    rebuild_glow_verts(&mut state, first_configure.region_w, first_configure.region_h);

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
                rebuild_glow_verts(&mut state, c.region_w, c.region_h);
            }
            Frame::Shutdown => {
                eprintln!("host requested shutdown");
                return Ok(());
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Render
// ---------------------------------------------------------------------------

fn render_and_send(
    dma: &DmaBuffer,
    conn: &mut Connection,
    gbm_egl: &GbmEgl,
    gpu: &GpuState,
    state: &mut State,
) -> Result<(), PluginError> {
    dma.bind_for_rendering()?;
    let (w, h) = (dma.width(), dma.height());

    update_spark_verts(state, w, h);

    unsafe {
        gl::ClearColor(0.0, 0.0, 0.0, 0.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);

        gl::Enable(gl::BLEND);
        gl::BlendFunc(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);

        // ------------------------------------------------------------------
        // Pass 1: bottom glow quad
        // ------------------------------------------------------------------
        gl::UseProgram(gpu.glow_program);
        gl::BindBuffer(gl::ARRAY_BUFFER, gpu.glow_vbo);

        if state.glow_dirty {
            gl::BufferData(
                gl::ARRAY_BUFFER,
                std::mem::size_of_val(&state.glow_verts) as isize,
                state.glow_verts.as_ptr() as *const _,
                gl::STATIC_DRAW,
            );
            state.glow_dirty = false;
        }

        let stride_g = (3 * std::mem::size_of::<f32>()) as i32;
        gl::EnableVertexAttribArray(gpu.glow_a_pos_loc);
        gl::VertexAttribPointer(
            gpu.glow_a_pos_loc, 2, gl::FLOAT, gl::FALSE, stride_g,
            std::ptr::null(),
        );
        gl::EnableVertexAttribArray(gpu.glow_a_t_loc);
        gl::VertexAttribPointer(
            gpu.glow_a_t_loc, 1, gl::FLOAT, gl::FALSE, stride_g,
            (2 * std::mem::size_of::<f32>()) as *const _,
        );
        gl::Uniform3f(
            gpu.glow_u_color_loc,
            state.config.glow_color[0],
            state.config.glow_color[1],
            state.config.glow_color[2],
        );
        gl::Uniform1f(gpu.glow_u_peak_loc, GLOW_PEAK_ALPHA);

        // Glow quad as triangle strip (4 verts).
        gl::DrawArrays(gl::TRIANGLE_STRIP, 0, 4);

        // ------------------------------------------------------------------
        // Pass 2: spark particles
        // ------------------------------------------------------------------
        gl::UseProgram(gpu.spark_program);
        gl::BindBuffer(gl::ARRAY_BUFFER, gpu.spark_vbo);
        gl::BufferData(
            gl::ARRAY_BUFFER,
            std::mem::size_of_val(&state.spark_verts[..]) as isize,
            state.spark_verts.as_ptr() as *const _,
            gl::STREAM_DRAW,
        );

        let stride_s = (5 * std::mem::size_of::<f32>()) as i32;
        gl::EnableVertexAttribArray(gpu.spark_a_pos_loc);
        gl::VertexAttribPointer(
            gpu.spark_a_pos_loc, 2, gl::FLOAT, gl::FALSE, stride_s,
            std::ptr::null(),
        );
        gl::EnableVertexAttribArray(gpu.spark_a_local_loc);
        gl::VertexAttribPointer(
            gpu.spark_a_local_loc, 2, gl::FLOAT, gl::FALSE, stride_s,
            (2 * std::mem::size_of::<f32>()) as *const _,
        );
        gl::EnableVertexAttribArray(gpu.spark_a_fade_loc);
        gl::VertexAttribPointer(
            gpu.spark_a_fade_loc, 1, gl::FLOAT, gl::FALSE, stride_s,
            (4 * std::mem::size_of::<f32>()) as *const _,
        );
        gl::Uniform4f(
            gpu.spark_u_color_loc,
            state.config.spark_color[0],
            state.config.spark_color[1],
            state.config.spark_color[2],
            state.config.spark_color[3],
        );

        gl::DrawArrays(gl::TRIANGLES, 0, (state.sparks.len() * 6) as i32);
    }

    conn.submit_frame(dma, gbm_egl)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{}: {}", env!("CARGO_PKG_NAME"), e);
        std::process::exit(1);
    }
}
