// SPDX-License-Identifier: GPL-3.0-or-later

//! Reference plugin — layered drifting bokeh circles with parallax depth.
//!
//! One full-buffer quad; the fragment shader does all the work. Three
//! unrolled layers of soft-edged circles drift in a shared direction at
//! different speeds — the classic parallax depth cue. Depth is encoded
//! in fixed per-layer ratios (near/mid/far): size 1.0/0.65/0.40,
//! speed 1.0/0.55/0.30, opacity 1.0/0.70/0.45.
//!
//! Each layer tiles pixel space into a grid; a per-cell hash decides
//! presence, radius, and center jitter. Circles never cross their cell
//! border, so coverage needs a single cell lookup (no neighbor search).
//! The hash lattice is wrapped mod 64 cells, making the pattern
//! tileable — the CPU wraps each layer's drift offset at the 64-cell
//! period in f64, so shader inputs stay bounded forever with no seam.
//!
//! Transparent overlay: designed to sit above a wallpaper or gradient.
//! Emits premultiplied alpha (`vec4(rgb * a, a)`).

use std::time::Instant;

use serde::Deserialize;
use veiland_plugin::{
    Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, Rng, gl as vgl,
};

const PLUGIN_NAME: &str = "parallax";

/// Per-layer depth ratios, near to far.
const SIZE_RATIO: [f64; 3] = [1.0, 0.65, 0.40];
const SPEED_RATIO: [f64; 3] = [1.0, 0.55, 0.30];
/// Grid cells are 4x the layer's max radius: room for jitter while
/// keeping every circle inside its own cell.
const CELL_FACTOR: f64 = 4.0;
/// The shader hashes cell ids mod 64 — the pattern's tile period.
const TILE_CELLS: f64 = 64.0;

#[derive(Debug, Clone, Deserialize)]
struct Config {
    /// Circle color; alpha is the master opacity for the whole effect.
    #[serde(default = "default_color")]
    color: [f32; 4],
    /// Near-layer max circle radius in px. Mid and far layers scale
    /// down from this.
    #[serde(default = "default_size_px")]
    size_px: f32,
    /// Fraction of grid cells that hold a circle, 0-1.
    #[serde(default = "default_density")]
    density: f32,
    /// Near-layer drift speed in px/s. Deeper layers move slower.
    #[serde(default = "default_speed")]
    speed: f32,
    /// Drift direction in degrees, math convention: 0 = rightward,
    /// 90 = upward.
    #[serde(default = "default_angle_deg")]
    angle_deg: f32,
    /// Edge feather as a fraction of each circle's radius. 1.0 = fully
    /// soft bokeh, small values = crisp dots.
    #[serde(default = "default_softness")]
    softness: f32,
    /// Layout seed: shifts each layer's pattern to a different spot.
    #[serde(default = "default_seed")]
    seed: u32,
}

fn default_color() -> [f32; 4] {
    [1.0, 1.0, 1.0, 0.2]
}
fn default_size_px() -> f32 {
    80.0
}
fn default_density() -> f32 {
    0.5
}
fn default_speed() -> f32 {
    8.0
}
fn default_angle_deg() -> f32 {
    30.0
}
fn default_softness() -> f32 {
    0.5
}
fn default_seed() -> u32 {
    0x9E37_79B9
}

impl Default for Config {
    fn default() -> Self {
        Self {
            color: default_color(),
            size_px: default_size_px(),
            density: default_density(),
            speed: default_speed(),
            angle_deg: default_angle_deg(),
            softness: default_softness(),
            seed: default_seed(),
        }
    }
}

/// Clamp a config float to a sane range. The config travels through
/// the host as JSON, so non-finite values are possible; they fall
/// back to the default instead of poisoning the offset math.
fn sane(x: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x.clamp(lo, hi)
    } else {
        fallback
    }
}

/// Sanitised parameters plus the start-of-life clock.
struct State {
    color: [f32; 4],
    size_px: f32,
    density: f32,
    softness: f32,
    speed_px_per_sec: f64,
    /// Screen-space drift direction (y down), unit length.
    dir: (f64, f64),
    /// Per-layer random starting offsets so the seed changes the
    /// layout even though the hash lives in the shader.
    init_off: [(f64, f64); 3],
    start: Instant,
}

impl State {
    fn new(config: &Config) -> Self {
        let size_px = sane(config.size_px, 4.0, 512.0, default_size_px());

        // Config angle is math convention (y up); the buffer's y axis
        // points down, so negate the sine.
        let angle = if config.angle_deg.is_finite() {
            f64::from(config.angle_deg).to_radians()
        } else {
            f64::from(default_angle_deg()).to_radians()
        };
        let dir = (angle.cos(), -angle.sin());

        let mut rng = Rng::new(config.seed);
        let init_off = std::array::from_fn(|i| {
            let period = TILE_CELLS * CELL_FACTOR * f64::from(size_px) * SIZE_RATIO[i];
            (
                f64::from(rng.next_f32()) * period,
                f64::from(rng.next_f32()) * period,
            )
        });

        Self {
            color: [
                sane(config.color[0], 0.0, 1.0, 1.0),
                sane(config.color[1], 0.0, 1.0, 1.0),
                sane(config.color[2], 0.0, 1.0, 1.0),
                sane(config.color[3], 0.0, 1.0, default_color()[3]),
            ],
            size_px,
            density: sane(config.density, 0.0, 1.0, default_density()),
            // Lower bound keeps smoothstep's edges strictly ordered.
            softness: sane(config.softness, 0.02, 1.0, default_softness()),
            speed_px_per_sec: f64::from(sane(config.speed, 0.0, 200.0, default_speed())),
            dir,
            init_off,
            start: Instant::now(),
        }
    }

    /// Wrapped pattern offset for one layer, in px. Computed in f64
    /// and wrapped to the layer's tile period, so the f32 the shader
    /// sees stays small for arbitrarily long lock sessions.
    fn layer_offset(&self, layer: usize, elapsed: f64) -> (f32, f32) {
        let period = TILE_CELLS * CELL_FACTOR * f64::from(self.size_px) * SIZE_RATIO[layer];
        let dist = elapsed * self.speed_px_per_sec * SPEED_RATIO[layer];
        let (ix, iy) = self.init_off[layer];
        (
            (ix + dist * self.dir.0).rem_euclid(period) as f32,
            (iy + dist * self.dir.1).rem_euclid(period) as f32,
        )
    }
}

struct GpuState {
    program: gl::types::GLuint,
    u_res_loc: gl::types::GLint,
    u_size_loc: gl::types::GLint,
    u_density_loc: gl::types::GLint,
    u_softness_loc: gl::types::GLint,
    u_color_loc: gl::types::GLint,
    u_off_locs: [gl::types::GLint; 3],
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

    // Highp: the sin-based hash falls apart at lower precision.
    let fs_src = b"#version 100\n\
        precision highp float;\n\
        varying vec2 v_uv;\n\
        // Buffer size in px; v_uv * u_res is the pixel position.\n\
        uniform vec2 u_res;\n\
        // Near-layer max circle radius in px.\n\
        uniform float u_size;\n\
        // Fraction of grid cells holding a circle.\n\
        uniform float u_density;\n\
        // Edge feather as a fraction of the circle radius.\n\
        uniform float u_softness;\n\
        // Circle color; alpha is the master opacity.\n\
        uniform vec4 u_color;\n\
        // Per-layer pattern translation in px, pre-wrapped on the CPU\n\
        // to the layer's 64-cell tile period so values stay bounded.\n\
        uniform vec2 u_off0;\n\
        uniform vec2 u_off1;\n\
        uniform vec2 u_off2;\n\
        \n\
        // Cell ids arrive wrapped to [0,64), so the sin argument\n\
        // never grows into precision-loss territory.\n\
        float hash21(vec2 p) {\n\
            return fract(sin(dot(p, vec2(127.1, 311.7))) * 43758.5453);\n\
        }\n\
        \n\
        // Coverage of one layer's circle in this pixel's grid cell.\n\
        // Jitter and radius are constrained so the circle never\n\
        // crosses the cell border: one cell lookup, no neighbors.\n\
        float layer_cov(vec2 frag_px, vec2 off_px, float rmax) {\n\
            float cell_sz = 4.0 * rmax;\n\
            vec2 g = (frag_px - off_px) / cell_sz;\n\
            // mod 64 makes the pattern tileable; GLSL mod() is\n\
            // non-negative for a positive modulus.\n\
            vec2 cell = mod(floor(g), 64.0);\n\
            vec2 f = (fract(g) - 0.5) * cell_sz;\n\
            float h_on = hash21(cell);\n\
            float h_r = hash21(cell + vec2(17.0, 31.0));\n\
            vec2 h_j = vec2(hash21(cell + vec2(43.0, 7.0)),\n\
                            hash21(cell + vec2(5.0, 59.0)));\n\
            float r = mix(0.5, 1.0, h_r) * rmax;\n\
            // Feather eats inward from r, so r is the outer extent;\n\
            // margin = half cell - r keeps circle + jitter inside.\n\
            float margin = 0.5 * cell_sz - r;\n\
            vec2 center = (h_j * 2.0 - 1.0) * margin;\n\
            float d = length(f - center);\n\
            float cov = 1.0 - smoothstep(r * (1.0 - u_softness), r, d);\n\
            // Cells whose presence hash misses the density cut are\n\
            // empty.\n\
            return cov * step(1.0 - u_density, h_on);\n\
        }\n\
        \n\
        void main() {\n\
            vec2 frag_px = v_uv * u_res;\n\
            // Depth ratios near/mid/far: size 1.0/0.65/0.40, opacity\n\
            // 1.0/0.70/0.45. Speed ratios live in the CPU offsets.\n\
            float c_near = layer_cov(frag_px, u_off0, u_size);\n\
            float c_mid = layer_cov(frag_px, u_off1, u_size * 0.65);\n\
            float c_far = layer_cov(frag_px, u_off2, u_size * 0.40);\n\
            // Screen-composite the three coverages into one alpha.\n\
            float a = 1.0 - (1.0 - c_near)\n\
                          * (1.0 - 0.70 * c_mid)\n\
                          * (1.0 - 0.45 * c_far);\n\
            a *= u_color.a;\n\
            // Premultiplied alpha: the host blends ONE/1-SRC_ALPHA.\n\
            gl_FragColor = vec4(u_color.rgb * a, a);\n\
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

        let u_res_loc = gl::GetUniformLocation(program, c"u_res".as_ptr());
        let u_size_loc = gl::GetUniformLocation(program, c"u_size".as_ptr());
        let u_density_loc = gl::GetUniformLocation(program, c"u_density".as_ptr());
        let u_softness_loc = gl::GetUniformLocation(program, c"u_softness".as_ptr());
        let u_color_loc = gl::GetUniformLocation(program, c"u_color".as_ptr());
        let u_off_locs = [c"u_off0", c"u_off1", c"u_off2"]
            .map(|name| gl::GetUniformLocation(program, name.as_ptr()));

        Ok(GpuState {
            program,
            u_res_loc,
            u_size_loc,
            u_density_loc,
            u_softness_loc,
            u_color_loc,
            u_off_locs,
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
        "veiland-{}: config color={:?} size_px={} density={} speed={} angle={} softness={}",
        PLUGIN_NAME,
        config.color,
        config.size_px,
        config.density,
        config.speed,
        config.angle_deg,
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

    let elapsed = state.start.elapsed().as_secs_f64();

    unsafe {
        gl::UseProgram(gpu.program);
        gl::Uniform2f(gpu.u_res_loc, dma.width() as f32, dma.height() as f32);
        gl::Uniform1f(gpu.u_size_loc, state.size_px);
        gl::Uniform1f(gpu.u_density_loc, state.density);
        gl::Uniform1f(gpu.u_softness_loc, state.softness);
        gl::Uniform4f(
            gpu.u_color_loc,
            state.color[0],
            state.color[1],
            state.color[2],
            state.color[3],
        );
        for (layer, loc) in gpu.u_off_locs.iter().enumerate() {
            let (ox, oy) = state.layer_offset(layer, elapsed);
            gl::Uniform2f(*loc, ox, oy);
        }

        // The quad covers the whole buffer and the shader writes a
        // valid premultiplied value (including a = 0) to every pixel:
        // no clear and no blending needed inside the plugin.
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
