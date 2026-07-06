// SPDX-License-Identifier: GPL-3.0-or-later

//! Reference plugin — large soft colored blobs drifting slowly.
//!
//! One full-buffer quad over a metaball field: the CPU drives up to
//! eight blob centers on glacial Lissajous paths (two layered sin
//! waves per axis, like fireflies but far slower) and the fragment
//! shader computes a field-weighted color average per pixel. Where
//! blob fields overlap, their colors melt into each other — the
//! ambient-blob / lava-lamp look.
//!
//! All motion math runs CPU-side in f64, so the shader's f32 uniforms
//! only ever see bounded values (positions in aspect-corrected UV,
//! radii in screen-height units) no matter how long the session runs.
//!
//! Fully opaque: emits `vec4(rgb, 1.0)`, no blending. Output is
//! dithered by half an 8-bit step — big soft falloffs band on
//! ARGB8888 just like slow gradients do.

use std::f64::consts::TAU;
use std::time::Instant;

use serde::Deserialize;
use veiland_plugin::{
    Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, Rng, gl as vgl,
};

const PLUGIN_NAME: &str = "blobs";

/// The shader's fixed uniform-array size. Unused slots keep radius 0
/// and contribute nothing to the field.
const MAX_BLOBS: usize = 8;

#[derive(Debug, Clone, Deserialize)]
struct Config {
    /// Blob palette, 1-8 colors, cycled across blobs.
    #[serde(default = "default_colors")]
    colors: Vec<[f32; 3]>,
    /// Background the blobs float over.
    #[serde(default = "default_background")]
    background: [f32; 3],
    /// Number of blobs, 1-8.
    #[serde(default = "default_count")]
    count: u32,
    /// Blob radius as a fraction of screen height. Each blob varies
    /// +/-30% around this. Past ~0.35 the fields saturate the whole
    /// screen and the look collapses into one big gradient.
    #[serde(default = "default_size")]
    size: f32,
    /// Drift speed multiplier. 1.0 = one slow orbit over a couple of
    /// minutes; 0 freezes the field.
    #[serde(default = "default_speed")]
    speed: f32,
    /// Falloff softness. Lower = tighter cores and darker gaps
    /// between blobs; higher = hazier until everything merges into
    /// a wash.
    #[serde(default = "default_softness")]
    softness: f32,
    /// Layout and motion seed.
    #[serde(default = "default_seed")]
    seed: u32,
}

fn default_colors() -> Vec<[f32; 3]> {
    vec![
        [0.12, 0.20, 0.55], // rich blue
        [0.45, 0.15, 0.50], // magenta purple
        [0.05, 0.42, 0.45], // bright teal
        [0.50, 0.28, 0.12], // warm amber
    ]
}
fn default_background() -> [f32; 3] {
    [0.02, 0.03, 0.08]
}
fn default_count() -> u32 {
    6
}
fn default_size() -> f32 {
    0.25
}
fn default_speed() -> f32 {
    1.0
}
fn default_softness() -> f32 {
    0.6
}
fn default_seed() -> u32 {
    0x9E37_79B9
}

impl Default for Config {
    fn default() -> Self {
        Self {
            colors: default_colors(),
            background: default_background(),
            count: default_count(),
            size: default_size(),
            speed: default_speed(),
            softness: default_softness(),
            seed: default_seed(),
        }
    }
}

/// Clamp a config float to a sane range. The config travels through
/// the host as JSON, so non-finite values are possible; they fall
/// back to the default instead of poisoning the motion math.
fn sane(x: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x.clamp(lo, hi)
    } else {
        fallback
    }
}

/// One sin component of a Lissajous wander: amp * sin(TAU*freq*t + phase).
/// Frequencies are in Hz with the speed multiplier folded in.
type Wave = (f64, f64, f64);

/// A blob's seeded motion parameters. Home and amplitudes are in
/// screen-height units; x is mapped into aspect-corrected space at
/// render time so motion stays isotropic on any monitor shape.
struct Blob {
    home: (f64, f64),
    x_waves: [Wave; 2],
    y_waves: [Wave; 2],
    radius: f32,
    color: [f32; 3],
}

impl Blob {
    fn position(&self, t: f64, aspect: f64) -> (f32, f32) {
        let sum = |waves: &[Wave; 2]| {
            waves
                .iter()
                .map(|(amp, freq, phase)| amp * (TAU * freq * t + phase).sin())
                .sum::<f64>()
        };
        (
            (self.home.0 * aspect + sum(&self.x_waves)) as f32,
            (self.home.1 + sum(&self.y_waves)) as f32,
        )
    }
}

/// Sanitised parameters plus the start-of-life clock.
struct State {
    blobs: Vec<Blob>,
    background: [f32; 3],
    /// Falloff exponent for the shader (2.0 / softness).
    sharp: f32,
    start: Instant,
}

impl State {
    fn new(config: &Config) -> Self {
        let mut palette: Vec<[f32; 3]> = config
            .colors
            .iter()
            .take(MAX_BLOBS)
            .map(|c| {
                [
                    sane(c[0], 0.0, 1.0, 0.0),
                    sane(c[1], 0.0, 1.0, 0.0),
                    sane(c[2], 0.0, 1.0, 0.0),
                ]
            })
            .collect();
        if palette.is_empty() {
            palette = default_colors();
        }

        let count = config.count.clamp(1, MAX_BLOBS as u32) as usize;
        let size = sane(config.size, 0.05, 1.0, default_size());
        let speed = f64::from(sane(config.speed, 0.0, 10.0, default_speed()));

        let mut rng = Rng::new(config.seed);
        let mut rand = move |lo: f64, hi: f64| lo + f64::from(rng.next_f32()) * (hi - lo);

        let blobs = (0..count)
            .map(|i| {
                // Drawn before `wave` below: that closure holds a
                // mutable borrow of `rand` for its whole lifetime, so
                // direct `rand` calls can't interleave with it.
                let home = (rand(0.15, 0.85), rand(0.15, 0.85));
                let radius = (f64::from(size) * rand(0.7, 1.3)) as f32;
                // Two incommensurate frequencies per axis give an
                // organic non-repeating wander. Periods are 100-330s
                // at speed 1.0 — glacial by design.
                let mut wave = || (rand(0.08, 0.20), rand(0.003, 0.010) * speed, rand(0.0, TAU));
                Blob {
                    home,
                    x_waves: [wave(), wave()],
                    y_waves: [wave(), wave()],
                    radius,
                    color: palette[i % palette.len()],
                }
            })
            .collect();

        Self {
            blobs,
            background: [
                sane(config.background[0], 0.0, 1.0, 0.0),
                sane(config.background[1], 0.0, 1.0, 0.0),
                sane(config.background[2], 0.0, 1.0, 0.0),
            ],
            // Inverse mapping: softer config = lower exponent = wider
            // falloff. Bounds keep pow()'s exponent in 0.5..=8.
            sharp: 2.0 / sane(config.softness, 0.25, 4.0, default_softness()),
            start: Instant::now(),
        }
    }
}

struct GpuState {
    program: gl::types::GLuint,
    u_aspect_loc: gl::types::GLint,
    u_bg_loc: gl::types::GLint,
    u_sharp_loc: gl::types::GLint,
    u_pos_loc: gl::types::GLint,
    u_radius_loc: gl::types::GLint,
    u_color_loc: gl::types::GLint,
}

unsafe fn build_gpu_state() -> Result<GpuState, String> {
    let vs_src = b"#version 100\n\
        precision highp float;\n\
        attribute vec2 a_pos;\n\
        varying vec2 v_uv;\n\
        void main() {\n\
            v_uv = a_pos * 0.5 + 0.5;\n\
            gl_Position = vec4(a_pos, 0.0, 1.0);\n\
        }\n\0";

    // Highp: the field sum is a stack of slow falloffs, which bands
    // at lower precision just like the vignette's smoothstep sum.
    let fs_src = b"#version 100\n\
        precision highp float;\n\
        varying vec2 v_uv;\n\
        // Buffer aspect (w/h). Positions and the pixel are compared\n\
        // in aspect-corrected UV so blobs stay round on any monitor.\n\
        uniform float u_aspect;\n\
        // Background the blobs float over.\n\
        uniform vec3 u_bg;\n\
        // Falloff exponent; higher = tighter blob cores.\n\
        uniform float u_sharp;\n\
        // Blob centers (aspect-corrected UV), radii (screen-height\n\
        // units), colors. Unused slots have radius 0 and contribute\n\
        // nothing. Indexing by the loop counter is the one dynamic\n\
        // uniform-array access GLSL ES 1.0 allows in fragment shaders.\n\
        uniform vec2 u_pos[8];\n\
        uniform float u_radius[8];\n\
        uniform vec3 u_color[8];\n\
        \n\
        void main() {\n\
            vec2 p = vec2(v_uv.x * u_aspect, v_uv.y);\n\
            float w = 0.0;\n\
            vec3 col = vec3(0.0);\n\
            for (int i = 0; i < 8; i++) {\n\
                vec2 d = p - u_pos[i];\n\
                float r2 = u_radius[i] * u_radius[i];\n\
                // Rational falloff: 1 at the center, 0.5 at distance\n\
                // r, soft tail beyond. The epsilon keeps empty slots\n\
                // (r2 = 0) at exactly zero instead of 0/0.\n\
                float wi = pow(r2 / (dot(d, d) + r2 + 1e-6), u_sharp);\n\
                w += wi;\n\
                col += wi * u_color[i];\n\
            }\n\
            // Field-weighted color average: where fields overlap the\n\
            // blob colors melt into each other.\n\
            col /= max(w, 1e-4);\n\
            // Total field strength decides how much blob color shows\n\
            // over the background.\n\
            float lum = smoothstep(0.15, 0.9, w);\n\
            vec3 rgb = mix(u_bg, col, lum);\n\
            // Hash dither, +/- half an 8-bit step, against banding on\n\
            // the wide soft falloffs.\n\
            float n = fract(sin(dot(gl_FragCoord.xy, vec2(12.9898, 78.233))) * 43758.5453);\n\
            rgb += (n - 0.5) / 255.0;\n\
            gl_FragColor = vec4(rgb, 1.0);\n\
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

        let u_aspect_loc = gl::GetUniformLocation(program, c"u_aspect".as_ptr());
        let u_bg_loc = gl::GetUniformLocation(program, c"u_bg".as_ptr());
        let u_sharp_loc = gl::GetUniformLocation(program, c"u_sharp".as_ptr());
        let u_pos_loc = gl::GetUniformLocation(program, c"u_pos".as_ptr());
        let u_radius_loc = gl::GetUniformLocation(program, c"u_radius".as_ptr());
        let u_color_loc = gl::GetUniformLocation(program, c"u_color".as_ptr());

        Ok(GpuState {
            program,
            u_aspect_loc,
            u_bg_loc,
            u_sharp_loc,
            u_pos_loc,
            u_radius_loc,
            u_color_loc,
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
        "veiland-{}: config palette={} count={} size={} speed={} softness={}",
        PLUGIN_NAME,
        config.colors.len(),
        config.count,
        config.size,
        config.speed,
        config.softness,
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

    // Self-paced: the field drifts continuously, so render again on
    // every BufferReleased at the compositor's repaint rate.
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
    let aspect = f64::from(dma.width()) / f64::from(dma.height());

    // Blob positions come out of f64 sin() on the CPU, so the f32
    // uniforms only ever carry bounded aspect-corrected UV values.
    let t = state.start.elapsed().as_secs_f64();
    let mut pos = [0.0f32; MAX_BLOBS * 2];
    let mut radius = [0.0f32; MAX_BLOBS];
    let mut color = [0.0f32; MAX_BLOBS * 3];
    for (i, blob) in state.blobs.iter().enumerate() {
        let (x, y) = blob.position(t, aspect);
        pos[i * 2] = x;
        pos[i * 2 + 1] = y;
        radius[i] = blob.radius;
        color[i * 3..i * 3 + 3].copy_from_slice(&blob.color);
    }

    unsafe {
        gl::UseProgram(gpu.program);
        gl::Uniform1f(gpu.u_aspect_loc, aspect as f32);
        gl::Uniform3f(
            gpu.u_bg_loc,
            state.background[0],
            state.background[1],
            state.background[2],
        );
        gl::Uniform1f(gpu.u_sharp_loc, state.sharp);
        gl::Uniform2fv(gpu.u_pos_loc, MAX_BLOBS as i32, pos.as_ptr());
        gl::Uniform1fv(gpu.u_radius_loc, MAX_BLOBS as i32, radius.as_ptr());
        gl::Uniform3fv(gpu.u_color_loc, MAX_BLOBS as i32, color.as_ptr());

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
