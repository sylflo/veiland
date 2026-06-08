// SPDX-License-Identifier: GPL-3.0-or-later

//! M11 reference plugin — slow upward drift of small dots.
//!
//! Geometry-based (one quad per particle, not instanced rendering),
//! per `docs/m11-plan.md`. Each particle has a fixed seed (its X,
//! and a time offset within the cycle); the Y position is recomputed
//! every frame from `(now - t_offset) mod cycle`. When a particle
//! wraps past the top it reappears at the bottom — the per-particle
//! offsets are staggered so wraps don't synchronise.
//!
//! Cadence: the plugin treats `BufferReleased` as "render next
//! frame," not FrameDone. The host's compositor refresh rate ends up
//! driving us. See `docs/m11-plan.md` Q2 — we accept the CPU/GPU
//! cost for M11 v1; the proper "opt into 60Hz" host capability is
//! M12+.

use serde::Deserialize;
use std::time::Instant;
use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, SyncFence};
use veiland_protocol::Buffer;

const PLUGIN_NAME: &str = "particles";

/// Cycle-length range per particle, in seconds. Each particle picks
/// a fixed cycle uniformly in this range at startup, so the field
/// has a natural speed variance: fast particles overtake slow ones,
/// wraps are fully desynchronised. The mockup's keyframe is 10-18s but
/// over 120vh of travel; ours rises ~100vh, so we use a tighter 6-11s
/// range to land at a comparable (livelier) on-screen speed.
const CYCLE_MIN_SECONDS: f32 = 6.0;
const CYCLE_MAX_SECONDS: f32 = 11.0;

/// Max net horizontal drift over one rise, in logical px. The mockup
/// drifts +50px; we randomise each particle in [-DRIFT, +DRIFT] so the
/// field curves both ways rather than all leaning the same direction.
const DRIFT_MAX_PX: f32 = 50.0;
/// Amplitude of the gentle sideways sin-wobble layered on top of the net
/// drift, in logical px. Small — just enough to feel alive, not jittery.
const WOBBLE_PX: f32 = 8.0;
/// Quad is drawn this much larger than the core radius so the soft glow
/// halo has room to fall off inside the quad (the FS puts the visible
/// core in the inner ~20% of the quad). Larger = wider, dreamier halo.
const GLOW_SCALE: f32 = 3.0;
/// Peak per-particle opacity (mockup tops out at 0.8, not full white).
const PEAK_OPACITY: f32 = 0.8;
/// Fraction of the rise spent fading in / out at each end.
const FADE_FRACTION: f32 = 0.12;

#[derive(Debug, Clone, Deserialize)]
struct Config {
    #[serde(default = "default_count")]
    count: u32,
    #[serde(default = "default_color")]
    color: [f32; 4],
    /// Dot radius in logical pixels at scale=1. Multiplied by
    /// `Configure.scale` at render time so a 3.0 radius shows the
    /// same visual size on 1× and 2× displays.
    #[serde(default = "default_radius_px")]
    radius_px: f32,
}

fn default_count() -> u32 {
    40
}
fn default_color() -> [f32; 4] {
    [1.0, 1.0, 1.0, 0.5]
}
fn default_radius_px() -> f32 {
    // Small, delicate core — the glow halo (GLOW_SCALE + the FS falloff)
    // does most of the visible work, which reads dreamier than a solid
    // dot. 0.4px radius core (near the sub-pixel AA floor), soft halo.
    0.4
}

fn default_config() -> Config {
    Config {
        count: default_count(),
        color: default_color(),
        radius_px: default_radius_px(),
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

/// Per-particle constants: horizontal position, time offset within
/// the cycle, and cycle length itself. All randomised once at
/// startup and fixed for the particle's lifetime. The `cycle` per
/// particle is what gives the field its speed variance — without
/// it, every particle moves in lockstep.
#[derive(Clone, Copy)]
struct Particle {
    x_norm: f32,
    t_offset: f32,
    cycle: f32,
    /// Net horizontal drift over one rise, in logical pixels (scaled at
    /// render time). Randomised per particle and signed, so some curve
    /// left and some right — kills the dead-straight 'snow' look.
    drift_px: f32,
    /// Phase offset for the gentle sideways sin-wobble, so particles
    /// don't all sway in unison.
    wobble_phase: f32,
}

/// Tiny deterministic PRNG. We don't depend on `rand` for a one-shot
/// stagger — a hashed sequence is plenty for "spread these N values
/// over the cycle." xorshift32 from Marsaglia, single-state.
struct Rng(u32);
impl Rng {
    fn next_f32(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x.max(1); // avoid the zero fixed point
        // Map u32 to [0,1) — high 24 bits gives ~7 decimal digits.
        (x >> 8) as f32 / (1u32 << 24) as f32
    }
}

fn seed_particles(count: u32) -> Vec<Particle> {
    let mut rng = Rng(0x9E3779B9); // golden-ratio seed
    (0..count)
        .map(|_| {
            let cycle =
                CYCLE_MIN_SECONDS + rng.next_f32() * (CYCLE_MAX_SECONDS - CYCLE_MIN_SECONDS);
            Particle {
                x_norm: rng.next_f32(),
                t_offset: rng.next_f32() * cycle,
                cycle,
                // Signed drift in [-DRIFT_MAX_PX, +DRIFT_MAX_PX].
                drift_px: (rng.next_f32() * 2.0 - 1.0) * DRIFT_MAX_PX,
                wobble_phase: rng.next_f32() * std::f32::consts::TAU,
            }
        })
        .collect()
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
    vbo: gl::types::GLuint,
    a_pos_loc: gl::types::GLuint,
    a_local_loc: gl::types::GLuint,
    a_fade_loc: gl::types::GLuint,
    u_color_loc: gl::types::GLint,
}

/// Build the shader + the empty VBO we'll fill each frame.
///
/// Each particle is 6 vertices (two triangles). Per vertex we send:
///   `a_pos`   — clip-space position of the corner.
///   `a_local` — local UV in [-1, 1] relative to the particle's
///               centre; the FS uses it for the core+glow falloff.
///   `a_fade`  — the particle's current opacity (0 at the travel
///               ends, peak in the middle), so dots fade in/out.
///
/// Interleaved layout, 5 floats per vertex (px, py, lx, ly, fade).
unsafe fn build_gpu_state() -> GpuState {
    // a_fade is the per-particle opacity (0 at the travel ends, peak in
    // the middle) — see update_vertices. Passed through to the FS so each
    // dot can materialise and dissolve instead of popping at the edges.
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

    // Dreamy dot = small bright core + big soft glow halo, mimicking the
    // mockup's `box-shadow: 0 0 8px`. The quad spans [-1,1] in a_local; the
    // visible core lives in the inner ~20% and a wide, gentle falloff fills
    // out to the edge. d is distance from centre.
    //   core: solid out to 0.10, smoothstep edge to 0.20 (a tiny crisp dot)
    //   glow: gentle falloff from centre to 1.0 (the halo). pow exponent
    //         1.5 (vs 2.0) widens the halo; weight 0.6 (vs 0.35) brightens
    //         it. The glow now dominates the dot — that's the dreamy look.
    // total coverage = core + glow*0.6, capped at 1.
    let fs_src = b"#version 100\n\
        precision mediump float;\n\
        varying vec2 v_local;\n\
        varying float v_fade;\n\
        uniform vec4 u_color;\n\
        void main() {\n\
            float d = length(v_local);\n\
            float core = 1.0 - smoothstep(0.10, 0.20, d);\n\
            float glow = pow(1.0 - clamp(d, 0.0, 1.0), 1.2);\n\
            float cov = clamp(core + glow * 0.8, 0.0, 1.0);\n\
            float a = u_color.a * cov * v_fade;\n\
            if (a <= 0.0) discard;\n\
            // Premultiplied alpha: the core composites this dmabuf with\n\
            // glBlendFunc(ONE, 1-SRC_ALPHA), so emit RGB pre-scaled by\n\
            // the final alpha.\n\
            gl_FragColor = vec4(u_color.rgb * a, a);\n\
        }\n\0";

    unsafe {
        let vs = compile_shader(gl::VERTEX_SHADER, vs_src);
        let fs = compile_shader(gl::FRAGMENT_SHADER, fs_src);
        let program = link_program(vs, fs);
        gl::UseProgram(program);

        let mut vbo: gl::types::GLuint = 0;
        gl::GenBuffers(1, &mut vbo);
        gl::BindBuffer(gl::ARRAY_BUFFER, vbo);

        let a_pos_loc =
            gl::GetAttribLocation(program, b"a_pos\0".as_ptr() as *const _) as gl::types::GLuint;
        let a_local_loc =
            gl::GetAttribLocation(program, b"a_local\0".as_ptr() as *const _) as gl::types::GLuint;
        let a_fade_loc =
            gl::GetAttribLocation(program, b"a_fade\0".as_ptr() as *const _) as gl::types::GLuint;
        let u_color_loc = gl::GetUniformLocation(program, b"u_color\0".as_ptr() as *const _);

        GpuState {
            program,
            vbo,
            a_pos_loc,
            a_local_loc,
            a_fade_loc,
            u_color_loc,
        }
    }
}

struct State {
    config: Config,
    particles: Vec<Particle>,
    /// CPU-side vertex buffer. 6 verts/particle × 4 floats/vert.
    /// Allocated once at startup; overwritten in place each frame.
    cpu_verts: Vec<f32>,
    scale: u32,
    start: Instant,
}

/// Convert pixel-space (origin top-left, Y down) to clip-space for
/// the dmabuf FBO. The host composites the dmabuf with row 0 at the
/// top of the screen (see the wallpaper plugin: no shader Y flip
/// needed), so we map pixel y=0 → clip y=-1 and pixel y=h → clip
/// y=+1 — what looks like "no flip" but is the inverse of the
/// classic GL "Y up" convention.
fn px_to_clip(x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
    let cx = (x / w) * 2.0 - 1.0;
    let cy = (y / h) * 2.0 - 1.0;
    (cx, cy)
}

/// Fill `cpu_verts` with current quad positions for every particle.
/// Called per frame.
fn update_vertices(state: &mut State, surface_w: u32, surface_h: u32) {
    let now = state.start.elapsed().as_secs_f32();
    let w = surface_w as f32;
    let h = surface_h as f32;
    let scale = state.scale as f32;
    // Visible core radius. The quad is GLOW_SCALE bigger so the halo has
    // room; the FS keeps the bright core in the quad's inner ~40%.
    let r_core = state.config.radius_px * scale;
    let r = r_core * GLOW_SCALE;

    // For each particle, compute its current pixel-space centre (with
    // drift + wobble), its fade opacity, and emit two triangles into the
    // pre-allocated vertex buffer.
    for (i, p) in state.particles.iter().enumerate() {
        let phase = ((now + p.t_offset) % p.cycle) / p.cycle;
        // phase 0 → just below the buffer, phase 1 → just above.
        let cy_px = h + r - phase * (h + 2.0 * r);

        // Horizontal: net drift accumulates with the rise, plus a gentle
        // sin wobble so the path curves rather than slides straight. Both
        // scaled to physical px.
        let drift = p.drift_px * scale * phase;
        let wobble = WOBBLE_PX * scale * (phase * std::f32::consts::TAU + p.wobble_phase).sin();
        let cx_px = p.x_norm * w + drift + wobble;

        // Fade: ramp 0→peak over the first FADE_FRACTION of the rise and
        // peak→0 over the last FADE_FRACTION; flat peak in between. So
        // particles materialise and dissolve instead of popping.
        let fade = if phase < FADE_FRACTION {
            phase / FADE_FRACTION
        } else if phase > 1.0 - FADE_FRACTION {
            (1.0 - phase) / FADE_FRACTION
        } else {
            1.0
        };
        let alpha = fade.clamp(0.0, 1.0) * PEAK_OPACITY;

        // Four corners in pixel space (quad is r = core * GLOW_SCALE).
        let (x0, y0) = (cx_px - r, cy_px - r);
        let (x1, y1) = (cx_px + r, cy_px - r);
        let (x2, y2) = (cx_px - r, cy_px + r);
        let (x3, y3) = (cx_px + r, cy_px + r);

        let (cx0, cy0) = px_to_clip(x0, y0, w, h);
        let (cx1, cy1) = px_to_clip(x1, y1, w, h);
        let (cx2, cy2) = px_to_clip(x2, y2, w, h);
        let (cx3, cy3) = px_to_clip(x3, y3, w, h);

        let off = i * 6 * 5;
        let verts = &mut state.cpu_verts[off..off + 30];
        // Each vertex: px, py, lx, ly, fade.
        // tri 1: 0, 1, 2
        verts[0] = cx0;
        verts[1] = cy0;
        verts[2] = -1.0;
        verts[3] = -1.0;
        verts[4] = alpha;
        verts[5] = cx1;
        verts[6] = cy1;
        verts[7] = 1.0;
        verts[8] = -1.0;
        verts[9] = alpha;
        verts[10] = cx2;
        verts[11] = cy2;
        verts[12] = -1.0;
        verts[13] = 1.0;
        verts[14] = alpha;
        // tri 2: 1, 3, 2
        verts[15] = cx1;
        verts[16] = cy1;
        verts[17] = 1.0;
        verts[18] = -1.0;
        verts[19] = alpha;
        verts[20] = cx3;
        verts[21] = cy3;
        verts[22] = 1.0;
        verts[23] = 1.0;
        verts[24] = alpha;
        verts[25] = cx2;
        verts[26] = cy2;
        verts[27] = -1.0;
        verts[28] = 1.0;
        verts[29] = alpha;
    }
}

fn run() -> Result<(), PluginError> {
    eprintln!(
        "veiland-{} (pid {}) starting",
        PLUGIN_NAME,
        std::process::id()
    );

    let config = load_config();
    eprintln!(
        "veiland-{}: config count={} color={:?} radius_px={}",
        PLUGIN_NAME, config.count, config.color, config.radius_px
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
    let gpu = unsafe { build_gpu_state() };

    let particles = seed_particles(config.count);
    let cpu_verts = vec![0.0_f32; particles.len() * 6 * 5];

    let mut state = State {
        config,
        particles,
        cpu_verts,
        scale: first_configure.scale,
        start: Instant::now(),
    };

    // Rebuilt whenever `dma` is reallocated (on a region change), since the
    // buffer carries the fd/stride/modifier the host needs to import it.
    let mut buf_msg = buffer_msg_for(&dma);

    // Self-paced: render on every BufferReleased so the compositor's
    // repaint rate drives the animation, not the host's input-event
    // cadence. See module docs / m11-plan Q2. FramePacer owns the pacing
    // state machine.
    let mut pacer = FramePacer::self_paced();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                render_and_send(
                    &dma, &gbm_egl, &mut conn, &buf_msg, &gpu, &mut state, fast_path,
                )?;
                pacer.submitted();
            }
            Frame::Reconfigure(c) => {
                // Reallocate the dmabuf to the output's true size. The
                // particle math is already resolution-independent (x_norm * w,
                // radius * scale, all read fresh each frame from the buffer
                // size), so a native-size buffer is all that's needed for
                // correctly-sized, crisp dots — no geometry change. Non-fatal
                // on failure.
                match dma.resize_to(&gbm_egl, c.region_w, c.region_h) {
                    Ok(true) => {
                        buf_msg = buffer_msg_for(&dma);
                        eprintln!(
                            "veiland-{}: reallocated to {}x{}, stride={}",
                            PLUGIN_NAME,
                            dma.width(),
                            dma.height(),
                            dma.stride(),
                        );
                    }
                    Ok(false) => {}
                    Err(e) => {
                        eprintln!(
                            "veiland-{}: reallocation to {}x{} failed: {} — \
                             keeping current buffer, particles may scale wrong",
                            PLUGIN_NAME, c.region_w, c.region_h, e
                        );
                    }
                }
                state.scale = c.scale;
            }
            Frame::Shutdown => {
                eprintln!("host requested shutdown");
                return Ok(());
            }
        }
    }
}

/// Build the wire `Buffer` message describing `dma`. Rebuilt after any
/// reallocation, since the fd/stride/modifier move with the GBM bo. `id`
/// stays 0 — v1 is single-buffer.
fn buffer_msg_for(dma: &DmaBuffer) -> Buffer {
    Buffer {
        id: 0,
        width: dma.width(),
        height: dma.height(),
        format: dma.format(),
        modifier: dma.modifier(),
        stride: dma.stride(),
        offset: 0,
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

    update_vertices(state, w, h);

    unsafe {
        gl::Viewport(0, 0, w as i32, h as i32);
        // Transparent — particles sit *on top of* the wallpaper +
        // vignette via the host's z-ordered composite.
        gl::ClearColor(0.0, 0.0, 0.0, 0.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);

        gl::Enable(gl::BLEND);
        // Premultiplied-alpha over operator; FS emits RGB*a. See fs_src.
        gl::BlendFunc(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);

        gl::UseProgram(gpu.program);
        gl::BindBuffer(gl::ARRAY_BUFFER, gpu.vbo);
        // Re-upload the whole vertex buffer. `STREAM_DRAW` because
        // it's overwritten every frame — driver hint for the
        // allocator.
        gl::BufferData(
            gl::ARRAY_BUFFER,
            std::mem::size_of_val(&state.cpu_verts[..]) as isize,
            state.cpu_verts.as_ptr() as *const _,
            gl::STREAM_DRAW,
        );

        // 5 floats/vertex: px, py, lx, ly, fade.
        let stride = (5 * std::mem::size_of::<f32>()) as i32;
        gl::EnableVertexAttribArray(gpu.a_pos_loc);
        gl::VertexAttribPointer(
            gpu.a_pos_loc,
            2,
            gl::FLOAT,
            gl::FALSE,
            stride,
            std::ptr::null(),
        );
        gl::EnableVertexAttribArray(gpu.a_local_loc);
        gl::VertexAttribPointer(
            gpu.a_local_loc,
            2,
            gl::FLOAT,
            gl::FALSE,
            stride,
            (2 * std::mem::size_of::<f32>()) as *const _,
        );
        gl::EnableVertexAttribArray(gpu.a_fade_loc);
        gl::VertexAttribPointer(
            gpu.a_fade_loc,
            1,
            gl::FLOAT,
            gl::FALSE,
            stride,
            (4 * std::mem::size_of::<f32>()) as *const _,
        );

        gl::Uniform4f(
            gpu.u_color_loc,
            state.config.color[0],
            state.config.color[1],
            state.config.color[2],
            state.config.color[3],
        );

        gl::DrawArrays(gl::TRIANGLES, 0, (state.particles.len() * 6) as i32);
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
