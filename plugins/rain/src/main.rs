// SPDX-License-Identifier: GPL-3.0-or-later

//! Reference plugin — wind-slanted falling rain streaks.
//!
//! A sibling of `particles`/`snow`, tuned to the opposite end of the
//! family's constants: drops cross the screen in ~1 second instead of
//! ~15, so each drop reads as a motion-blur *streak* — a thin, slanted
//! capsule with a dim tail and a brighter head — rather than a shape.
//! All drops share one wind slant (small per-drop jitter), unlike
//! snow's independent sway. Each drop carries a depth value: near
//! drops are longer, faster, wider, and brighter than far ones, which
//! turns the flat sheet into a rain volume.
//!
//! Geometry: one slanted thin quad per drop, built directly in pixel
//! space along the fall direction (no rotation trick needed — the quad
//! itself is oriented). Drops spawn fully above the top edge and exit
//! fully below the bottom, so no fade in/out is needed: the wrap is
//! off-screen and, at ~1s cycles, invisible.
//!
//! Cadence: self-paced, same as particles.

use serde::Deserialize;
use std::time::Instant;
use veiland_plugin::{
    Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, SyncFence, gl as vgl,
};
use veiland_protocol::Buffer;

const PLUGIN_NAME: &str = "rain";

/// Fall duration range across the screen height, in seconds, before
/// depth scaling. Near drops take CYCLE_FAST, far drops CYCLE_SLOW.
/// Physically real rain crosses in well under a second, but that is
/// exhausting to look at on a lock screen — these values are ~2x
/// slower than real for comfort while still reading as rain.
/// (Compare snow's 12-22s.)
const CYCLE_FAST_SECONDS: f32 = 1.8;
const CYCLE_SLOW_SECONDS: f32 = 3.6;
/// Per-drop jitter around the shared wind slant, in degrees. Small —
/// rain falls as a sheet, not as independent wanderers.
const SLANT_JITTER_DEG: f32 = 1.5;
/// Streak half-width range in logical px, far to near.
const WIDTH_MIN_PX: f32 = 0.9;
const WIDTH_MAX_PX: f32 = 1.6;
/// Farthest drops render at this fraction of the configured length and
/// this fraction of the configured alpha; nearest drops at 1.0.
const LEN_DEPTH_MIN: f32 = 0.45;
const ALPHA_DEPTH_MIN: f32 = 0.35;

#[derive(Debug, Clone, Deserialize)]
struct Config {
    #[serde(default = "default_count")]
    count: u32,
    #[serde(default = "default_color")]
    color: [f32; 4],
    /// Full streak length in logical px at scale=1 for the *nearest*
    /// drops; farther drops shrink toward LEN_DEPTH_MIN of this.
    #[serde(default = "default_length_px")]
    length_px: f32,
    /// Shared wind slant in degrees from vertical. Positive leans the
    /// fall rightward. All drops share it (tiny per-drop jitter).
    #[serde(default = "default_slant_deg")]
    slant_deg: f32,
}

fn default_count() -> u32 {
    // Rain is a volume, not a scatter — denser than snow's 12-16.
    90
}
fn default_color() -> [f32; 4] {
    // Cool blue-grey, translucent: rain reads by motion, not brightness.
    [0.72, 0.80, 0.95, 0.65]
}
fn default_length_px() -> f32 {
    36.0
}
fn default_slant_deg() -> f32 {
    10.0
}

fn default_config() -> Config {
    Config {
        count: default_count(),
        color: default_color(),
        length_px: default_length_px(),
        slant_deg: default_slant_deg(),
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

/// Per-drop constants, randomised once at startup. `depth` in [0,1]
/// drives the near/far look: length, speed, width, and alpha all scale
/// with it, so one uniform sheet becomes a layered volume.
#[derive(Clone, Copy)]
struct Drop {
    x_norm: f32,
    t_offset: f32,
    cycle: f32,
    /// 0 = farthest (short, slow, dim), 1 = nearest (long, fast, bright).
    depth: f32,
    /// Per-drop deviation from the shared wind slant, radians.
    slant_jitter: f32,
}

/// Tiny deterministic PRNG (xorshift32, Marsaglia).
struct Rng(u32);
impl Rng {
    fn next_f32(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x.max(1); // avoid the zero fixed point
        (x >> 8) as f32 / (1u32 << 24) as f32
    }
}

fn seed_drops(count: u32) -> Vec<Drop> {
    let mut rng = Rng(0x1F12_3BB5); // arbitrary seed, distinct from siblings'
    (0..count)
        .map(|_| {
            let depth = rng.next_f32();
            // Near drops fall faster; +-10% jitter so equal-depth drops
            // still desynchronise.
            let cycle = (CYCLE_SLOW_SECONDS - depth * (CYCLE_SLOW_SECONDS - CYCLE_FAST_SECONDS))
                * (0.9 + 0.2 * rng.next_f32());
            Drop {
                x_norm: rng.next_f32(),
                t_offset: rng.next_f32() * cycle,
                cycle,
                depth,
                slant_jitter: (rng.next_f32() * 2.0 - 1.0) * SLANT_JITTER_DEG.to_radians(),
            }
        })
        .collect()
}

struct GpuState {
    program: gl::types::GLuint,
    vbo: gl::types::GLuint,
    a_pos_loc: gl::types::GLuint,
    a_local_loc: gl::types::GLuint,
    a_fade_loc: gl::types::GLuint,
    u_color_loc: gl::types::GLint,
}

/// Build the streak shader + the empty VBO we fill each frame.
///
/// Each drop is 6 vertices (two triangles) of a thin quad already
/// oriented along the fall direction. Per vertex, interleaved, 5
/// floats: px, py, lx, ly, fade.
///   `a_pos`   — clip-space corner position.
///   `a_local` — local UV: x in [-1,1] runs *along* the streak (tail at
///               -1, head at +1), y in [-1,1] across it.
///   `a_fade`  — the drop's depth-scaled opacity.
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

    // Motion-blur streak: soft across the width, rounded ends, and a
    // lengthwise gradient from dim tail (x = -1) to bright head
    // (x = +1) — the eye reads the bright end as the drop and the tail
    // as its blur. Fixed-width smoothstep AA, no fwidth (project GLES2
    // baseline).
    let fs_src = b"#version 100\n\
        precision mediump float;\n\
        varying vec2 v_local;\n\
        varying float v_fade;\n\
        uniform vec4 u_color;\n\
        void main() {\n\
            float across = 1.0 - smoothstep(0.45, 1.0, abs(v_local.y));\n\
            float ends = 1.0 - smoothstep(0.80, 1.0, abs(v_local.x));\n\
            float grad = 0.30 + 0.70 * (v_local.x + 1.0) * 0.5;\n\
            float a = u_color.a * across * ends * grad * v_fade;\n\
            if (a <= 0.0) discard;\n\
            // Premultiplied alpha: the core composites this dmabuf with\n\
            // glBlendFunc(ONE, 1-SRC_ALPHA), so emit RGB pre-scaled by\n\
            // the final alpha.\n\
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

        let a_pos_loc = gl::GetAttribLocation(program, c"a_pos".as_ptr()) as gl::types::GLuint;
        let a_local_loc = gl::GetAttribLocation(program, c"a_local".as_ptr()) as gl::types::GLuint;
        let a_fade_loc = gl::GetAttribLocation(program, c"a_fade".as_ptr()) as gl::types::GLuint;
        let u_color_loc = gl::GetUniformLocation(program, c"u_color".as_ptr());

        Ok(GpuState {
            program,
            vbo,
            a_pos_loc,
            a_local_loc,
            a_fade_loc,
            u_color_loc,
        })
    }
}

struct State {
    config: Config,
    drops: Vec<Drop>,
    /// CPU-side vertex buffer. 6 verts/drop x 5 floats/vert. Allocated
    /// once at startup; overwritten in place each frame.
    cpu_verts: Vec<f32>,
    scale_120: u32,
    start: Instant,
}

/// Convert pixel-space (origin top-left, Y down) to clip-space for the
/// dmabuf FBO. The host composites the dmabuf with row 0 at the top, so
/// pixel y=0 -> clip y=-1 and pixel y=h -> clip y=+1.
fn px_to_clip(x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
    let cx = (x / w) * 2.0 - 1.0;
    let cy = (y / h) * 2.0 - 1.0;
    (cx, cy)
}

/// Fill `cpu_verts` with current slanted-quad positions for every drop.
/// Called per frame.
fn update_vertices(state: &mut State, surface_w: u32, surface_h: u32) {
    let now = state.start.elapsed().as_secs_f32();
    let w = surface_w as f32;
    let h = surface_h as f32;
    let scale = state.scale_120 as f32 / 120.0;
    let base_slant = state.config.slant_deg.to_radians();

    for (i, p) in state.drops.iter().enumerate() {
        let phase = ((now + p.t_offset) % p.cycle) / p.cycle;

        // Depth-scaled streak dimensions, in physical px.
        let half_len = 0.5
            * state.config.length_px
            * scale
            * (LEN_DEPTH_MIN + (1.0 - LEN_DEPTH_MIN) * p.depth);
        let half_w = (WIDTH_MIN_PX + (WIDTH_MAX_PX - WIDTH_MIN_PX) * p.depth) * scale;
        let alpha = ALPHA_DEPTH_MIN + (1.0 - ALPHA_DEPTH_MIN) * p.depth;

        // Fall direction: unit vector slanted from vertical by the
        // shared wind angle plus this drop's jitter. Positive slant
        // leans rightward as y increases downward.
        let slant = base_slant + p.slant_jitter;
        let (dir_x, dir_y) = (slant.sin(), slant.cos());

        // Vertical travel spans from fully above the top edge to fully
        // below the bottom, so the wrap happens off-screen. The x start
        // range is widened by the total horizontal travel so coverage
        // over [0, w] stays uniform despite the slant.
        let travel = h + 2.0 * half_len;
        let cy_px = -half_len + phase * travel;
        let dx_total = travel * (dir_x / dir_y);
        let cx0 = p.x_norm * (w + dx_total.abs()) - dx_total.max(0.0);
        let cx_px = cx0 + dx_total * phase;

        // Quad oriented along the fall direction: corners at
        // centre +- dir*half_len +- perp*half_w. local.x runs along the
        // streak (tail -1 at the trailing/top end, head +1 at the
        // leading/bottom end), local.y across it.
        let (px, py) = (dir_y, -dir_x); // perpendicular
        let tail = (cx_px - dir_x * half_len, cy_px - dir_y * half_len);
        let head = (cx_px + dir_x * half_len, cy_px + dir_y * half_len);
        let (x0, y0) = (tail.0 - px * half_w, tail.1 - py * half_w); // tail, left
        let (x1, y1) = (tail.0 + px * half_w, tail.1 + py * half_w); // tail, right
        let (x2, y2) = (head.0 - px * half_w, head.1 - py * half_w); // head, left
        let (x3, y3) = (head.0 + px * half_w, head.1 + py * half_w); // head, right

        let (cx0c, cy0c) = px_to_clip(x0, y0, w, h);
        let (cx1c, cy1c) = px_to_clip(x1, y1, w, h);
        let (cx2c, cy2c) = px_to_clip(x2, y2, w, h);
        let (cx3c, cy3c) = px_to_clip(x3, y3, w, h);

        let off = i * 6 * 5;
        let v = &mut state.cpu_verts[off..off + 30];
        // Each vertex: px, py, lx (along, tail=-1 head=+1), ly (across), fade.
        // tri 1: tail-left, tail-right, head-left
        v[0] = cx0c;
        v[1] = cy0c;
        v[2] = -1.0;
        v[3] = -1.0;
        v[4] = alpha;
        v[5] = cx1c;
        v[6] = cy1c;
        v[7] = -1.0;
        v[8] = 1.0;
        v[9] = alpha;
        v[10] = cx2c;
        v[11] = cy2c;
        v[12] = 1.0;
        v[13] = -1.0;
        v[14] = alpha;
        // tri 2: tail-right, head-right, head-left
        v[15] = cx1c;
        v[16] = cy1c;
        v[17] = -1.0;
        v[18] = 1.0;
        v[19] = alpha;
        v[20] = cx3c;
        v[21] = cy3c;
        v[22] = 1.0;
        v[23] = 1.0;
        v[24] = alpha;
        v[25] = cx2c;
        v[26] = cy2c;
        v[27] = 1.0;
        v[28] = -1.0;
        v[29] = alpha;
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
        "veiland-{}: config count={} color={:?} length_px={} slant_deg={}",
        PLUGIN_NAME, config.count, config.color, config.length_px, config.slant_deg
    );

    let gbm_egl = GbmEgl::new()?;

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

    let drops = seed_drops(config.count);
    let cpu_verts = vec![0.0_f32; drops.len() * 6 * 5];

    let mut state = State {
        config,
        drops,
        cpu_verts,
        scale_120: first_configure.scale_120,
        start: Instant::now(),
    };

    let mut buf_msg = buffer_msg_for(&dma);

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
                // drop math is resolution-independent (x_norm * w,
                // length * scale, read fresh each frame), so no geometry
                // change is needed. Non-fatal on failure.
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
                             keeping current buffer, drops may scale wrong",
                            PLUGIN_NAME, c.region_w, c.region_h, e
                        );
                    }
                }
                state.scale_120 = c.scale_120;
            }
            Frame::Shutdown => {
                eprintln!("host requested shutdown");
                return Ok(());
            }
        }
    }
}

/// Build the wire `Buffer` message describing `dma`. Rebuilt after any
/// reallocation. `id` stays 0 — v1 is single-buffer.
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
        // Transparent — rain sits on top of the wallpaper via the host's
        // z-ordered composite.
        gl::ClearColor(0.0, 0.0, 0.0, 0.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);

        gl::Enable(gl::BLEND);
        // Premultiplied-alpha over operator; FS emits RGB*a.
        gl::BlendFunc(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);

        gl::UseProgram(gpu.program);
        gl::BindBuffer(gl::ARRAY_BUFFER, gpu.vbo);
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

        gl::DrawArrays(gl::TRIANGLES, 0, (state.drops.len() * 6) as i32);
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
