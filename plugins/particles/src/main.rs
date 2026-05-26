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
use veiland_plugin::{Connection, DmaBuffer, GbmEgl, PluginError, SyncFence};
use veiland_protocol::{Buffer, ServerMessage};

const PLUGIN_NAME: &str = "particles";

/// Cycle-length range per particle, in seconds. Each particle picks
/// a fixed cycle uniformly in this range at startup, so the field
/// has a natural speed variance: fast particles overtake slow ones,
/// wraps are fully desynchronised. Mockup uses 10-18s.
const CYCLE_MIN_SECONDS: f32 = 10.0;
const CYCLE_MAX_SECONDS: f32 = 18.0;

/// Particle dot radius, in logical pixels at scale=1. Multiplied by
/// `Configure.scale` when sizing quads.
const PARTICLE_RADIUS_PX: f32 = 1.5;

#[derive(Debug, Clone, Deserialize)]
struct Config {
    #[serde(default = "default_count")]
    count: u32,
    #[serde(default = "default_color")]
    color: [f32; 4],
}

fn default_count() -> u32 {
    40
}
fn default_color() -> [f32; 4] {
    [1.0, 1.0, 1.0, 0.5]
}

fn default_config() -> Config {
    Config {
        count: default_count(),
        color: default_color(),
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
            let cycle = CYCLE_MIN_SECONDS
                + rng.next_f32() * (CYCLE_MAX_SECONDS - CYCLE_MIN_SECONDS);
            Particle {
                x_norm: rng.next_f32(),
                t_offset: rng.next_f32() * cycle,
                cycle,
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
    u_color_loc: gl::types::GLint,
}

/// Build the shader + the empty VBO we'll fill each frame.
///
/// Each particle is 6 vertices (two triangles). Per vertex we send:
///   `a_pos`   — clip-space position of the corner.
///   `a_local` — local UV in [-1, 1] relative to the particle's
///               centre, used by the fragment shader to discard
///               pixels outside the disc.
///
/// Interleaved layout, 4 floats per vertex (px, py, lx, ly).
unsafe fn build_gpu_state() -> GpuState {
    let vs_src = b"#version 100\n\
        attribute vec2 a_pos;\n\
        attribute vec2 a_local;\n\
        varying vec2 v_local;\n\
        void main() {\n\
            v_local = a_local;\n\
            gl_Position = vec4(a_pos, 0.0, 1.0);\n\
        }\n\0";

    // Smoothstep on the disc edge keeps the dot from aliasing at\n\
    // small sizes — without it 3px particles look like little\n\
    // squares with a half-eaten corner.
    let fs_src = b"#version 100\n\
        precision mediump float;\n\
        varying vec2 v_local;\n\
        uniform vec4 u_color;\n\
        void main() {\n\
            float d = length(v_local);\n\
            float a = 1.0 - smoothstep(0.85, 1.0, d);\n\
            if (a <= 0.0) discard;\n\
            gl_FragColor = vec4(u_color.rgb, u_color.a * a);\n\
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
        let u_color_loc = gl::GetUniformLocation(program, b"u_color\0".as_ptr() as *const _);

        GpuState {
            program,
            vbo,
            a_pos_loc,
            a_local_loc,
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
    let r = PARTICLE_RADIUS_PX * state.scale as f32;

    // For each particle, compute its current pixel-space centre and
    // emit two triangles' worth of (a_pos, a_local) data into the
    // pre-allocated vertex buffer.
    for (i, p) in state.particles.iter().enumerate() {
        let phase = ((now + p.t_offset) % p.cycle) / p.cycle;
        // phase 0 → just below the buffer (y = h + r), phase 1 → just
        // above (y = -r). Linear rise, no easing.
        let cy_px = h + r - phase * (h + 2.0 * r);
        let cx_px = p.x_norm * w;

        // Four corners in pixel space.
        let (x0, y0) = (cx_px - r, cy_px - r);
        let (x1, y1) = (cx_px + r, cy_px - r);
        let (x2, y2) = (cx_px - r, cy_px + r);
        let (x3, y3) = (cx_px + r, cy_px + r);

        let (cx0, cy0) = px_to_clip(x0, y0, w, h);
        let (cx1, cy1) = px_to_clip(x1, y1, w, h);
        let (cx2, cy2) = px_to_clip(x2, y2, w, h);
        let (cx3, cy3) = px_to_clip(x3, y3, w, h);

        let off = i * 6 * 4;
        let verts = &mut state.cpu_verts[off..off + 24];
        // tri 1: 0, 1, 2
        verts[0] = cx0;
        verts[1] = cy0;
        verts[2] = -1.0;
        verts[3] = -1.0;
        verts[4] = cx1;
        verts[5] = cy1;
        verts[6] = 1.0;
        verts[7] = -1.0;
        verts[8] = cx2;
        verts[9] = cy2;
        verts[10] = -1.0;
        verts[11] = 1.0;
        // tri 2: 1, 3, 2
        verts[12] = cx1;
        verts[13] = cy1;
        verts[14] = 1.0;
        verts[15] = -1.0;
        verts[16] = cx3;
        verts[17] = cy3;
        verts[18] = 1.0;
        verts[19] = 1.0;
        verts[20] = cx2;
        verts[21] = cy2;
        verts[22] = -1.0;
        verts[23] = 1.0;
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
        "veiland-{}: config count={} color={:?}",
        PLUGIN_NAME, config.count, config.color
    );

    let gbm_egl = GbmEgl::new()?;

    let mut conn = Connection::from_env()?;
    conn.handshake()?;
    conn.send_hello(PLUGIN_NAME, env!("CARGO_PKG_VERSION"))?;
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

    let first_configure = loop {
        match conn.recv_event()? {
            ServerMessage::Configure(c) => break c,
            ServerMessage::Shutdown => {
                eprintln!("veiland-{}: shutdown before first configure", PLUGIN_NAME);
                return Ok(());
            }
            other => {
                eprintln!(
                    "veiland-{}: unexpected pre-configure message {:?}, ignoring",
                    PLUGIN_NAME, other
                );
            }
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

    let particles = seed_particles(config.count);
    let cpu_verts = vec![0.0_f32; particles.len() * 6 * 4];

    let mut state = State {
        config,
        particles,
        cpu_verts,
        scale: first_configure.scale,
        start: Instant::now(),
    };

    let buf_msg = Buffer {
        id: 0,
        width: dma.width(),
        height: dma.height(),
        format: dma.format(),
        modifier: dma.modifier(),
        stride: dma.stride(),
        offset: 0,
    };

    // Particle pacing: render on BufferReleased (not FrameDone) so the
    // animation runs at the compositor's repaint rate, not the host's
    // input-event cadence. See module docs / m11-plan Q2.
    //
    // Same flag pair as label/wallpaper plugins, but used differently:
    // - On FrameDone: render if we can (first frame after spawn does
    //   arrive via FrameDone — the host sends one alongside the first
    //   Configure).
    // - On BufferReleased: render *again* immediately. This is what
    //   keeps the loop turning without waiting for the host to opt us
    //   into another FrameDone.
    let mut buffer_released = true;
    let mut got_first_frame_done = false;

    loop {
        match conn.recv_event()? {
            ServerMessage::Configure(c) => {
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
                state.scale = c.scale;
            }
            ServerMessage::FrameDone => {
                got_first_frame_done = true;
                if !buffer_released {
                    continue;
                }
                render_and_send(&dma, &gbm_egl, &mut conn, &buf_msg, &gpu, &mut state, fast_path)?;
                buffer_released = false;
            }
            ServerMessage::BufferReleased(_) => {
                buffer_released = true;
                // Self-pacing: as soon as the host releases our last
                // buffer, draw a new one. The host's repaint cadence
                // ends up driving us.
                if got_first_frame_done {
                    render_and_send(&dma, &gbm_egl, &mut conn, &buf_msg, &gpu, &mut state, fast_path)?;
                    buffer_released = false;
                }
            }
            ServerMessage::Shutdown => {
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

    update_vertices(state, w, h);

    unsafe {
        gl::Viewport(0, 0, w as i32, h as i32);
        // Transparent — particles sit *on top of* the wallpaper +
        // vignette via the host's z-ordered composite.
        gl::ClearColor(0.0, 0.0, 0.0, 0.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);

        gl::Enable(gl::BLEND);
        gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);

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

        let stride = (4 * std::mem::size_of::<f32>()) as i32;
        gl::EnableVertexAttribArray(gpu.a_pos_loc);
        gl::VertexAttribPointer(gpu.a_pos_loc, 2, gl::FLOAT, gl::FALSE, stride, std::ptr::null());
        gl::EnableVertexAttribArray(gpu.a_local_loc);
        gl::VertexAttribPointer(
            gpu.a_local_loc,
            2,
            gl::FLOAT,
            gl::FALSE,
            stride,
            (2 * std::mem::size_of::<f32>()) as *const _,
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
