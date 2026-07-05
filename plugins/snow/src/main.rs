// SPDX-License-Identifier: GPL-3.0-or-later

//! Reference plugin — gently falling dendritic snow crystals.
//!
//! A sibling of `particles`/`sakura`, but each flake is a *procedurally
//! generated six-fold-symmetric ice crystal* drawn entirely in the
//! fragment shader (no texture): a hexagonal core, six main spines, and
//! a tier of fern-like side-branches whose spacing and length vary with
//! a per-flake seed, so every crystal is unique and crisp at any size.
//! Flakes fall downward (like sakura), drift and sway gently, and slowly
//! tumble as they fall.
//!
//! Geometry: one quad per flake, 6 verts. Per vertex we send the quad
//! corner, the *rotated* local UV (rotation baked in CPU-side so the
//! crystal tumbles), a fade, and the flake's seed. Cadence: self-paced,
//! same as particles.

use serde::Deserialize;
use std::time::Instant;
use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, gl as vgl};

const PLUGIN_NAME: &str = "snow";

/// Cycle-length range per flake, in seconds — how long one crystal takes
/// to fall the full height. Big crystals fall slowly and stately.
/// Per-flake variance within the range desynchronises the field.
const CYCLE_MIN_SECONDS: f32 = 12.0;
const CYCLE_MAX_SECONDS: f32 = 22.0;

/// Max net horizontal drift over one fall, in logical px, signed per flake.
const DRIFT_MAX_PX: f32 = 30.0;
/// Amplitude of the gentle sideways sin-sway, in logical px.
const SWAY_PX: f32 = 8.0;
/// Radians of tumble over one full fall, signed per flake (given here in
/// turns, multiplied by TAU below). Crystals rotate slowly as they
/// descend so the six-fold structure catches the eye.
const SPIN_MAX_TURNS: f32 = 0.6;
/// Fraction of the fall spent fading in / out at each end, so crystals
/// materialise and dissolve at the screen edges instead of popping.
const FADE_FRACTION: f32 = 0.12;
/// Peak per-flake opacity.
const PEAK_OPACITY: f32 = 0.85;

#[derive(Debug, Clone, Deserialize)]
struct Config {
    #[serde(default = "default_count")]
    count: u32,
    #[serde(default = "default_color")]
    color: [f32; 4],
    /// Crystal radius (half-extent of its quad) in logical px at scale=1.
    /// Multiplied by `Configure.scale` at render time. Large — the fern
    /// detail needs size to read.
    #[serde(default = "default_radius_px")]
    radius_px: f32,
}

fn default_count() -> u32 {
    // Few large exquisite crystals, not a dense field — detail needs room.
    12
}
fn default_color() -> [f32; 4] {
    [1.0, 1.0, 1.0, 0.9]
}
fn default_radius_px() -> f32 {
    // The fern detail lives in strokes ~5% of the radius wide, so the
    // crystal must be large for them to span real pixels: at 60px the
    // spine base is ~3px and the rim ~2.7px. Below ~40px the structure
    // collapses back into a dot.
    60.0
}

impl Default for Config {
    fn default() -> Self {
        Self {
            count: default_count(),
            color: default_color(),
            radius_px: default_radius_px(),
        }
    }
}

/// Per-flake constants. Beyond position/timing/drift, `seed` drives the
/// crystal's unique branch pattern in the FS, and `rot0`/`spin` give it a
/// slow tumble over one fall.
#[derive(Clone, Copy)]
struct Flake {
    x_norm: f32,
    t_offset: f32,
    cycle: f32,
    /// Net horizontal drift over one fall, in logical px (scaled at
    /// render time). Signed, so some drift left and some right.
    drift_px: f32,
    /// Phase offset for the gentle sideways sin-sway.
    sway_phase: f32,
    /// Crystal-shape seed in [0,1); scaled in-shader to vary arm length,
    /// spine width, and fern angle/length so every flake differs.
    seed: f32,
    /// Starting tumble angle (rad).
    rot0: f32,
    /// Total tumble over one fall (rad), signed.
    spin: f32,
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

fn seed_flakes(count: u32) -> Vec<Flake> {
    let mut rng = Rng(0x2545_F491); // arbitrary seed, distinct from particles'
    (0..count)
        .map(|_| {
            let cycle =
                CYCLE_MIN_SECONDS + rng.next_f32() * (CYCLE_MAX_SECONDS - CYCLE_MIN_SECONDS);
            Flake {
                x_norm: rng.next_f32(),
                t_offset: rng.next_f32() * cycle,
                cycle,
                drift_px: (rng.next_f32() * 2.0 - 1.0) * DRIFT_MAX_PX,
                sway_phase: rng.next_f32() * std::f32::consts::TAU,
                seed: rng.next_f32(),
                rot0: rng.next_f32() * std::f32::consts::TAU,
                spin: (rng.next_f32() * 2.0 - 1.0) * SPIN_MAX_TURNS * std::f32::consts::TAU,
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
    a_seed_loc: gl::types::GLuint,
    u_color_loc: gl::types::GLint,
}

/// Build the crystal shader + the empty VBO we fill each frame.
///
/// Each flake is 6 vertices (two triangles). Per vertex, interleaved,
/// 6 floats: px, py, lx, ly, fade, seed.
///   `a_pos`   — clip-space corner position.
///   `a_local` — local UV in [-1, 1] from the flake centre, already
///               rotated CPU-side so the crystal tumbles.
///   `a_fade`  — the flake's current opacity (0 at travel ends).
///   `a_seed`  — the flake's shape seed, [0,1).
unsafe fn build_gpu_state() -> Result<GpuState, String> {
    let vs_src = b"#version 100\n\
        attribute vec2 a_pos;\n\
        attribute vec2 a_local;\n\
        attribute float a_fade;\n\
        attribute float a_seed;\n\
        varying vec2 v_local;\n\
        varying float v_fade;\n\
        varying float v_seed;\n\
        void main() {\n\
            v_local = a_local;\n\
            v_fade = a_fade;\n\
            v_seed = a_seed;\n\
            gl_Position = vec4(a_pos, 0.0, 1.0);\n\
        }\n\0";

    // Dendritic crystal, drawn in local space [-1,1] from the flake
    // centre. The shape is built in a single 60 degree wedge and mirrored
    // by six-fold symmetry, so we only describe one arm.
    //
    // Everything is a signed distance: sdSeg() is a *tapered* capsule
    // (half-width w0 at a, w1 at b), the core is a hexagon expressed in
    // wedge space, and the final sd is the min over all elements. From
    // sd we derive coverage (smoothstep edge, fixed AA width -- no
    // fwidth, matches the project GLES2 baseline) plus a two-tone glassy
    // shading: a bright icy rim where sd is near zero and a translucent
    // fill deeper inside. The rim/fill split is what makes the shape
    // read as ice rather than a flat white sticker.
    //
    // Structure per arm (proportions matter -- at quad radius R px, a
    // half-width of 0.05 is 0.05*R px on screen; earlier version used
    // hairline widths that vanished at typical sizes):
    //   core  : hexagonal plate, seed-varied radius
    //   spine : tapered capsule, thick at base, needle at the tip
    //   ferns : four tapered capsules per side at exactly 60 deg (real
    //           dendrite side-branches grow parallel to the crystal
    //           axes), long near the base and shorter toward the tip so
    //           the tips follow the hexagonal envelope.
    let fs_src = b"#version 100\n\
        precision highp float;\n\
        varying vec2 v_local;\n\
        varying float v_fade;\n\
        varying float v_seed;\n\
        uniform vec4 u_color;\n\
        const float PI = 3.14159265;\n\
        const float AA = 0.015;\n\
        float hash(float x) {\n\
            return fract(sin(x * 12.9898) * 43758.5453);\n\
        }\n\
        // Signed distance to a tapered capsule from a to b.\n\
        float sdSeg(vec2 p, vec2 a, vec2 b, float w0, float w1) {\n\
            vec2 pa = p - a; vec2 ba = b - a;\n\
            float h = clamp(dot(pa, ba) / dot(ba, ba), 0.0, 1.0);\n\
            return length(pa - ba * h) - mix(w0, w1, h);\n\
        }\n\
        void main() {\n\
            vec2 p = v_local;\n\
            float r = length(p);\n\
            if (r > 1.0) discard;\n\
            // Fold into one 60 deg wedge (six-fold symmetry). After the\n\
            // fold, ang = 0 is the arm axis; q.x runs along the arm and\n\
            // q.y is the distance across it.\n\
            float ang = atan(p.y, p.x);\n\
            float sector = PI / 3.0;\n\
            ang = mod(ang, sector);\n\
            ang = abs(ang - sector * 0.5);\n\
            vec2 q = vec2(cos(ang), sin(ang)) * r;\n\
            // Seed-driven variation.\n\
            float s = v_seed;\n\
            float armLen = 0.88 + 0.10 * s;\n\
            float coreR = 0.10 + 0.10 * hash(s * 5.7);\n\
            // Spine: thick base tapering to a needle tip.\n\
            float sd = sdSeg(q, vec2(0.0, 0.0), vec2(armLen, 0.0), 0.050, 0.012);\n\
            // Hexagonal core plate. In wedge space the hex edge is the\n\
            // line at distance coreR*cos(30 deg) along the 30 deg normal.\n\
            float sdHex = dot(q, vec2(0.8660254, 0.5)) - coreR * 0.8660254;\n\
            sd = min(sd, sdHex);\n\
            // Ferns: four per side at exactly 60 deg from the spine\n\
            // (classic stellar dendrite). Position and length jittered\n\
            // per fern by the seed; length shrinks toward the tip so\n\
            // fern tips trace the hexagonal envelope. Fixed-count loop\n\
            // for GLES2.\n\
            for (int i = 1; i <= 4; i++) {\n\
                float fi = float(i);\n\
                float at = armLen * (0.30 + 0.16 * (fi - 1.0)\n\
                    + 0.05 * hash(s * 17.0 + fi));\n\
                float fl = (armLen - at)\n\
                    * (0.55 + 0.30 * hash(s * 31.0 + fi));\n\
                vec2 base = vec2(at, 0.0);\n\
                vec2 tip = base + vec2(0.5, 0.8660254) * fl;\n\
                sd = min(sd, sdSeg(q, base, tip, 0.030, 0.008));\n\
            }\n\
            float cov = 1.0 - smoothstep(-AA, AA, sd);\n\
            if (cov <= 0.0) discard;\n\
            // Glassy two-tone: bright rim at the edge, translucent fill\n\
            // deeper inside.\n\
            float inner = 1.0 - smoothstep(-0.045 - AA, -0.045 + AA, sd);\n\
            float shade = mix(1.0, 0.45, inner);\n\
            float a = u_color.a * cov * shade * v_fade;\n\
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
        let a_seed_loc = gl::GetAttribLocation(program, c"a_seed".as_ptr()) as gl::types::GLuint;
        let u_color_loc = gl::GetUniformLocation(program, c"u_color".as_ptr());

        Ok(GpuState {
            program,
            vbo,
            a_pos_loc,
            a_local_loc,
            a_fade_loc,
            a_seed_loc,
            u_color_loc,
        })
    }
}

struct State {
    config: Config,
    flakes: Vec<Flake>,
    /// CPU-side vertex buffer. 6 verts/flake x 6 floats/vert. Allocated
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

/// Fill `cpu_verts` with current quad positions for every flake. Called
/// per frame.
fn update_vertices(state: &mut State, surface_w: u32, surface_h: u32) {
    let now = state.start.elapsed().as_secs_f32();
    let w = surface_w as f32;
    let h = surface_h as f32;
    let scale = state.scale_120 as f32 / 120.0;
    let r = state.config.radius_px * scale;

    for (i, p) in state.flakes.iter().enumerate() {
        let phase = ((now + p.t_offset) % p.cycle) / p.cycle;
        // Downward fall: phase 0 -> just above the top, phase 1 -> just
        // below the bottom.
        let cy_px = -r + phase * (h + 2.0 * r);

        let drift = p.drift_px * scale * phase;
        let sway = SWAY_PX * scale * (phase * std::f32::consts::TAU + p.sway_phase).sin();
        let cx_px = p.x_norm * w + drift + sway;

        let fade = if phase < FADE_FRACTION {
            phase / FADE_FRACTION
        } else if phase > 1.0 - FADE_FRACTION {
            (1.0 - phase) / FADE_FRACTION
        } else {
            1.0
        };
        let alpha = fade.clamp(0.0, 1.0) * PEAK_OPACITY;

        // Tumble. Rotate the local UVs by the flake's current angle so the
        // whole crystal spins as it falls. The pixel-space corners stay an
        // axis-aligned quad (the bounding box); only the *local* coords the
        // shader reads are rotated, which rotates the drawn crystal inside
        // its quad. Cheap, no extra attribute.
        let angle = p.rot0 + p.spin * phase;
        let (sa, ca) = angle.sin_cos();
        let rot = |lx: f32, ly: f32| (lx * ca - ly * sa, lx * sa + ly * ca);
        let (l0x, l0y) = rot(-1.0, -1.0);
        let (l1x, l1y) = rot(1.0, -1.0);
        let (l2x, l2y) = rot(-1.0, 1.0);
        let (l3x, l3y) = rot(1.0, 1.0);

        // Axis-aligned quad corners in pixel space (the crystal's bbox).
        let (cx0, cy0) = px_to_clip(cx_px - r, cy_px - r, w, h);
        let (cx1, cy1) = px_to_clip(cx_px + r, cy_px - r, w, h);
        let (cx2, cy2) = px_to_clip(cx_px - r, cy_px + r, w, h);
        let (cx3, cy3) = px_to_clip(cx_px + r, cy_px + r, w, h);

        let off = i * 6 * 6;
        let v = &mut state.cpu_verts[off..off + 36];
        let sd = p.seed;
        // Each vertex: px, py, lx, ly, fade, seed.
        // tri 1: 0, 1, 2
        v[0] = cx0;
        v[1] = cy0;
        v[2] = l0x;
        v[3] = l0y;
        v[4] = alpha;
        v[5] = sd;
        v[6] = cx1;
        v[7] = cy1;
        v[8] = l1x;
        v[9] = l1y;
        v[10] = alpha;
        v[11] = sd;
        v[12] = cx2;
        v[13] = cy2;
        v[14] = l2x;
        v[15] = l2y;
        v[16] = alpha;
        v[17] = sd;
        // tri 2: 1, 3, 2
        v[18] = cx1;
        v[19] = cy1;
        v[20] = l1x;
        v[21] = l1y;
        v[22] = alpha;
        v[23] = sd;
        v[24] = cx3;
        v[25] = cy3;
        v[26] = l3x;
        v[27] = l3y;
        v[28] = alpha;
        v[29] = sd;
        v[30] = cx2;
        v[31] = cy2;
        v[32] = l2x;
        v[33] = l2y;
        v[34] = alpha;
        v[35] = sd;
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
        "veiland-{}: config count={} color={:?} radius_px={}",
        PLUGIN_NAME, config.count, config.color, config.radius_px
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

    let flakes = seed_flakes(config.count);
    let cpu_verts = vec![0.0_f32; flakes.len() * 6 * 6];

    let mut state = State {
        config,
        flakes,
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
                match dma.resize_to(&gbm_egl, c.region_w, c.region_h) {
                    Ok(true) => {
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
                             keeping current buffer, flakes may scale wrong",
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
        // Transparent — snow sits on top of the wallpaper via the host's
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

        // 6 floats/vertex: px, py, lx, ly, fade, seed.
        let stride = (6 * std::mem::size_of::<f32>()) as i32;
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
        gl::EnableVertexAttribArray(gpu.a_seed_loc);
        gl::VertexAttribPointer(
            gpu.a_seed_loc,
            1,
            gl::FLOAT,
            gl::FALSE,
            stride,
            (5 * std::mem::size_of::<f32>()) as *const _,
        );

        gl::Uniform4f(
            gpu.u_color_loc,
            state.config.color[0],
            state.config.color[1],
            state.config.color[2],
            state.config.color[3],
        );

        gl::DrawArrays(gl::TRIANGLES, 0, (state.flakes.len() * 6) as i32);
    }

    conn.submit_frame(dma, gbm_egl)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{}: {}", env!("CARGO_PKG_NAME"), e);
        std::process::exit(1);
    }
}
