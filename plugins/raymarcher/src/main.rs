// SPDX-License-Identifier: GPL-3.0-or-later

//! Reference plugin — slow raymarched drift through gyroid tunnels.
//!
//! One full-buffer quad; the fragment shader is the whole renderer.
//! Each pixel shoots a ray into a scene defined by a signed distance
//! function (a gyroid shell with a corridor carved along the z axis)
//! and sphere-traces to the first hit. Shading comes from the same
//! function: its gradient is the surface normal, the step count
//! approximates ambient occlusion, and exponential fog hides the
//! march's far boundary.
//!
//! All motion is computed CPU-side in f64 and uploaded as wrapped
//! values so the shader's f32 uniforms never see unbounded time. The
//! camera's forward position wraps modulo the gyroid's 2*pi period,
//! which is seamless because the scene repeats exactly.
//!
//! Thermal knobs, on by default: `render_scale` allocates a smaller
//! buffer (the host's bilinear sampler stretches it to the region)
//! and `max_fps` throttles submissions (the host keeps compositing
//! the last submitted buffer between submits).
//!
//! Fully opaque: emits `vec4(rgb, 1.0)`, no blending. The output is
//! dithered by half an 8-bit step — slow fog ramps are worst-case
//! banding material on ARGB8888.

use std::time::{Duration, Instant};

use serde::Deserialize;
use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, gl as vgl};

const PLUGIN_NAME: &str = "raymarcher";

#[derive(Debug, Clone, Deserialize)]
struct Config {
    /// Drift speed multiplier. 1.0 crosses one gyroid cell about
    /// every 18 seconds; 0 freezes the camera.
    #[serde(default = "default_speed")]
    speed: f32,
    /// Vertical field of view in degrees.
    #[serde(default = "default_fov_deg")]
    fov_deg: f32,
    /// Palette stops, 2-4 used. Fewer than 2 falls back to the
    /// default palette; stops past the fourth are ignored. The
    /// first stop also tints the fog.
    #[serde(default = "default_colors")]
    colors: Vec<[f32; 3]>,
    /// Fog density multiplier. Fog also hides the march's far
    /// boundary, so 0 is possible but not recommended.
    #[serde(default = "default_fog")]
    fog: f32,
    /// Buffer resolution as a fraction of the region. The host
    /// upsamples bilinearly; 0.5 costs a quarter of the rays.
    #[serde(default = "default_render_scale")]
    render_scale: f32,
    /// Cap on submitted frames per second. 0 = compositor rate.
    #[serde(default = "default_max_fps")]
    max_fps: f32,
}

fn default_speed() -> f32 {
    1.0
}
fn default_fov_deg() -> f32 {
    70.0
}
fn default_colors() -> Vec<[f32; 3]> {
    vec![
        [0.08, 0.10, 0.18], // deep indigo (also the fog tint)
        [0.55, 0.30, 0.15], // warm amber
        [0.20, 0.35, 0.40], // slate teal
    ]
}
fn default_fog() -> f32 {
    1.0
}
fn default_render_scale() -> f32 {
    0.5
}
fn default_max_fps() -> f32 {
    30.0
}

impl Default for Config {
    fn default() -> Self {
        Self {
            speed: default_speed(),
            fov_deg: default_fov_deg(),
            colors: default_colors(),
            fog: default_fog(),
            render_scale: default_render_scale(),
            max_fps: default_max_fps(),
        }
    }
}

/// Clamp a config float to a sane range. The config travels through
/// the host as JSON, so non-finite values are possible; they fall
/// back to the default instead of poisoning the camera math.
fn sane(x: f32, lo: f32, hi: f32, fallback: f32) -> f32 {
    if x.is_finite() {
        x.clamp(lo, hi)
    } else {
        fallback
    }
}

// Camera motion. Periods are in seconds at speed = 1.0; sway and
// look periods are deliberately non-multiples so the combined drift
// never visibly repeats. Sway magnitude (sqrt of the two amplitudes
// squared, ~0.33) must stay well inside the shader's CLEARANCE
// (0.7) so the camera always has wall clearance.
const CELL: f64 = std::f64::consts::TAU; // gyroid period per axis
const Z_PERIOD: f64 = 18.0; // seconds per cell of forward travel
const SWAY_X_PERIOD: f64 = 17.0;
const SWAY_Y_PERIOD: f64 = 26.0;
const SWAY_X_AMP: f64 = 0.26;
const SWAY_Y_AMP: f64 = 0.20;
const LOOK_X_PERIOD: f64 = 31.0;
const LOOK_Y_PERIOD: f64 = 47.0;
const LOOK_X_AMP: f64 = 0.16;
const LOOK_Y_AMP: f64 = 0.12;
// Base fog density at fog = 1.0: ~95% extinction at the march's
// far boundary (MAX_DIST = 40 in the shader).
const BASE_FOG: f32 = 0.08;

/// Sanitised animation parameters plus the start-of-life clock.
struct State {
    ramp: [[f32; 3]; 4],
    speed: f64,
    tan_half_fov: f32,
    fog_density: f32,
    render_scale: f64,
    frame_budget: Option<Duration>,
    start: Instant,
}

impl State {
    fn new(config: &Config) -> Self {
        // Normalise the stop list to exactly four uniforms. Cycling
        // (i % len) keeps every padding smooth: 2 stops become
        // c0,c1,c0,c1 (a back-and-forth), 3 become c0,c1,c2,c0 with
        // a flat hold on the last segment (which blends back to c0).
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

        let fov_deg = sane(config.fov_deg, 30.0, 110.0, default_fov_deg());
        let max_fps = sane(config.max_fps, 0.0, 240.0, default_max_fps());

        Self {
            ramp,
            speed: f64::from(sane(config.speed, 0.0, 10.0, default_speed())),
            tan_half_fov: (f64::from(fov_deg).to_radians() * 0.5).tan() as f32,
            fog_density: sane(config.fog, 0.0, 4.0, default_fog()) * BASE_FOG,
            render_scale: f64::from(sane(config.render_scale, 0.1, 1.0, default_render_scale())),
            frame_budget: (max_fps > 0.0)
                .then(|| Duration::from_secs_f64(1.0 / f64::from(max_fps))),
            start: Instant::now(),
        }
    }
}

/// Buffer dimension after render_scale, kept inside the protocol's
/// [1, 8192] bounds.
fn scaled_dim(dim: u32, scale: f64) -> u32 {
    ((f64::from(dim) * scale).round() as u32).clamp(1, 8192)
}

fn cross(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn normalize(v: [f64; 3]) -> [f64; 3] {
    let len = (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
    [v[0] / len, v[1] / len, v[2] / len]
}

struct Camera {
    pos: [f32; 3],
    right: [f32; 3],
    up: [f32; 3],
    fwd: [f32; 3],
}

/// Camera pose at `elapsed` seconds. All phases are wrapped in f64
/// before any f32 cast: raw elapsed seconds would lose fract()
/// precision after long lock sessions, and the shader must never
/// see unbounded values. The forward position wraps modulo the
/// gyroid cell, which is invisible because the scene repeats
/// exactly; every other motion is a bounded sine of its own
/// wrapped phase.
fn camera_at(state: &State, elapsed: f64) -> Camera {
    let tt = elapsed * state.speed;
    let z = (tt / Z_PERIOD).rem_euclid(1.0) * CELL;
    let sway_x = SWAY_X_AMP * ((tt / SWAY_X_PERIOD).rem_euclid(1.0) * CELL).sin();
    let sway_y = SWAY_Y_AMP * ((tt / SWAY_Y_PERIOD).rem_euclid(1.0) * CELL).sin();
    let look_x = LOOK_X_AMP * ((tt / LOOK_X_PERIOD).rem_euclid(1.0) * CELL).sin();
    let look_y = LOOK_Y_AMP * ((tt / LOOK_Y_PERIOD).rem_euclid(1.0) * CELL).sin();

    // Look-at basis: gaze drifts gently off the +z travel axis.
    let fwd = normalize([look_x, look_y, 1.0]);
    let right = normalize(cross([0.0, 1.0, 0.0], fwd));
    let up = cross(fwd, right);

    Camera {
        pos: [sway_x as f32, sway_y as f32, z as f32],
        right: right.map(|v| v as f32),
        up: up.map(|v| v as f32),
        fwd: fwd.map(|v| v as f32),
    }
}

struct GpuState {
    program: gl::types::GLuint,
    u_c_locs: [gl::types::GLint; 4],
    u_aspect_loc: gl::types::GLint,
    u_tan_half_fov_loc: gl::types::GLint,
    u_fog_loc: gl::types::GLint,
    u_cam_pos_loc: gl::types::GLint,
    u_cam_right_loc: gl::types::GLint,
    u_cam_up_loc: gl::types::GLint,
    u_cam_fwd_loc: gl::types::GLint,
}

unsafe fn build_gpu_state() -> Result<GpuState, String> {
    // Highp throughout — sphere tracing accumulates distance across
    // dozens of steps; mediump falls apart long before the ramp
    // banding that already forced gradient onto highp.
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
        // Palette stops. The ramp loops: the last segment blends\n\
        // back to u_c0, so fract() of the band index never pops.\n\
        uniform vec3 u_c0;\n\
        uniform vec3 u_c1;\n\
        uniform vec3 u_c2;\n\
        uniform vec3 u_c3;\n\
        // Buffer aspect (w/h), applied to x so pixels stay square.\n\
        uniform float u_aspect;\n\
        // tan(fov / 2): half-height of the image plane one unit\n\
        // ahead of the eye.\n\
        uniform float u_tan_half_fov;\n\
        // Exponential fog density, config multiplier pre-applied.\n\
        uniform float u_fog;\n\
        // Camera position. z is wrapped CPU-side to [0, 2pi): the\n\
        // gyroid repeats every 2pi per axis, so the wrap is\n\
        // seamless and f32 never sees unbounded time. Sway keeps\n\
        // x/y inside the carved corridor.\n\
        uniform vec3 u_cam_pos;\n\
        // Orthonormal camera basis, built CPU-side in f64.\n\
        uniform vec3 u_cam_right;\n\
        uniform vec3 u_cam_up;\n\
        uniform vec3 u_cam_fwd;\n\
        \n\
        // GLSL ES 1.0 requires a compile-time constant loop bound.\n\
        const int MAX_STEPS = 96;\n\
        const float EPS = 0.001;\n\
        const float MAX_DIST = 40.0;\n\
        const float TAU = 6.283185307;\n\
        // Shell half-thickness; bigger = heavier walls.\n\
        const float THICKNESS = 0.42;\n\
        // Corridor radius carved along the z axis. The raw gyroid\n\
        // crosses the axis at every multiple of pi; without the\n\
        // carve the camera would fly through walls.\n\
        const float CLEARANCE = 0.7;\n\
        // abs(gyroid) is a distance estimate, not a true distance.\n\
        // Scaling down keeps sphere tracing from overshooting thin\n\
        // walls, at the cost of extra steps.\n\
        const float SAFETY = 0.6;\n\
        \n\
        float map(vec3 p) {\n\
            float g = sin(p.x) * cos(p.y) + sin(p.y) * cos(p.z)\n\
                    + sin(p.z) * cos(p.x);\n\
            float shell = abs(g) - THICKNESS;\n\
            // max(a, -b) is SDF subtraction: cut the corridor out\n\
            // of the shell.\n\
            float carve = CLEARANCE - length(p.xy);\n\
            return max(shell, carve) * SAFETY;\n\
        }\n\
        \n\
        // Surface normal: the SDF gradient, sampled with central\n\
        // differences on each axis.\n\
        vec3 normal_at(vec3 p) {\n\
            vec2 e = vec2(0.002, 0.0);\n\
            return normalize(vec3(\n\
                map(p + e.xyy) - map(p - e.xyy),\n\
                map(p + e.yxy) - map(p - e.yxy),\n\
                map(p + e.yyx) - map(p - e.yyx)));\n\
        }\n\
        \n\
        // Unrolled 4-stop looping mix chain: GLSL ES 1.0 does not\n\
        // allow dynamic indexing of uniform arrays in the fragment\n\
        // shader. smoothstep clamps its input, so each segment is\n\
        // inert outside its [k, k+1) span of s.\n\
        vec3 palette(float t) {\n\
            float s = fract(t) * 4.0;\n\
            vec3 c = mix(u_c0, u_c1, smoothstep(0.0, 1.0, s));\n\
            c = mix(c, u_c2, smoothstep(1.0, 2.0, s));\n\
            c = mix(c, u_c3, smoothstep(2.0, 3.0, s));\n\
            return mix(c, u_c0, smoothstep(3.0, 4.0, s));\n\
        }\n\
        \n\
        void main() {\n\
            // The host samples the buffer y-flipped relative to GL\n\
            // frame coords; flip back so +y is up on screen.\n\
            vec2 ndc = vec2(v_uv.x, 1.0 - v_uv.y) * 2.0 - 1.0;\n\
            vec2 lens = vec2(ndc.x * u_aspect, ndc.y) * u_tan_half_fov;\n\
            vec3 rd = normalize(\n\
                u_cam_right * lens.x + u_cam_up * lens.y + u_cam_fwd);\n\
            vec3 ro = u_cam_pos;\n\
        \n\
            // Sphere tracing: map() returns a radius known to be\n\
            // free of surfaces, so stepping by it never tunnels\n\
            // through a wall.\n\
            float t = 0.0;\n\
            float steps = 0.0;\n\
            bool hit = false;\n\
            for (int i = 0; i < MAX_STEPS; i++) {\n\
                float d = map(ro + rd * t);\n\
                if (d < EPS) { hit = true; break; }\n\
                t += d;\n\
                steps += 1.0;\n\
                if (t > MAX_DIST) break;\n\
            }\n\
        \n\
            vec3 fog_color = u_c0 * 0.25;\n\
            vec3 c = fog_color;\n\
            if (hit) {\n\
                vec3 hp = ro + rd * t;\n\
                vec3 nor = normal_at(hp);\n\
                // Band index: one palette cycle per 2pi of x+y+z,\n\
                // so the camera's z wrap shifts it by exactly 1 and\n\
                // fract() hides it. Hit coords are bounded by\n\
                // MAX_DIST, so f32 trig keeps full precision here.\n\
                float band = fract((hp.x + hp.y + hp.z) / TAU);\n\
                vec3 base = palette(band);\n\
                // Fixed key light plus a headlight fill; no shadow\n\
                // rays in v1.\n\
                vec3 key_dir = normalize(vec3(0.5, 0.8, -0.3));\n\
                float key = max(dot(nor, key_dir), 0.0);\n\
                float head = clamp(dot(nor, -rd), 0.0, 1.0);\n\
                // Rays that spent many steps ended in crevices: a\n\
                // free ambient-occlusion approximation.\n\
                float occ = clamp(1.0 - steps / float(MAX_STEPS), 0.0, 1.0);\n\
                vec3 lit = base * (0.20 + 0.35 * key + 0.60 * head) * occ;\n\
                c = mix(lit, fog_color, 1.0 - exp(-u_fog * t));\n\
            }\n\
            // Hash dither, +/- half an 8-bit step: the slow fog\n\
            // ramp bands badly on ARGB8888 without it.\n\
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

        Ok(GpuState {
            program,
            u_c_locs,
            u_aspect_loc: gl::GetUniformLocation(program, c"u_aspect".as_ptr()),
            u_tan_half_fov_loc: gl::GetUniformLocation(program, c"u_tan_half_fov".as_ptr()),
            u_fog_loc: gl::GetUniformLocation(program, c"u_fog".as_ptr()),
            u_cam_pos_loc: gl::GetUniformLocation(program, c"u_cam_pos".as_ptr()),
            u_cam_right_loc: gl::GetUniformLocation(program, c"u_cam_right".as_ptr()),
            u_cam_up_loc: gl::GetUniformLocation(program, c"u_cam_up".as_ptr()),
            u_cam_fwd_loc: gl::GetUniformLocation(program, c"u_cam_fwd".as_ptr()),
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
        "veiland-{}: config stops={} speed={} fov={} fog={} render_scale={} max_fps={}",
        PLUGIN_NAME,
        config.colors.len(),
        config.speed,
        config.fov_deg,
        config.fog,
        config.render_scale,
        config.max_fps,
    );

    let state = State::new(&config);

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

    // render_scale: allocate a smaller buffer and let the host's
    // bilinear sampler stretch it to the region. Ray count scales
    // with the pixel count, so 0.5 costs a quarter of the rays.
    let mut dma = DmaBuffer::new(
        &gbm_egl,
        scaled_dim(first_configure.region_w, state.render_scale),
        scaled_dim(first_configure.region_h, state.render_scale),
    )?;
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

    // Self-paced, optionally throttled: the host keeps compositing
    // the last submitted buffer, so sleeping before the next submit
    // caps our GPU duty cycle without any protocol support. Motion
    // is wall-clock driven, so a cap makes it chunkier, not slower.
    let mut pacer = FramePacer::self_paced();
    let mut last_submit: Option<Instant> = None;
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                if let (Some(budget), Some(prev)) = (state.frame_budget, last_submit) {
                    let since = prev.elapsed();
                    if since < budget {
                        std::thread::sleep(budget - since);
                    }
                }
                render_and_send(&dma, &mut conn, &gbm_egl, &gpu, &state)?;
                last_submit = Some(Instant::now());
                pacer.submitted();
            }
            Frame::Reconfigure(c) => {
                dma.resize_or_keep(
                    &gbm_egl,
                    scaled_dim(c.region_w, state.render_scale),
                    scaled_dim(c.region_h, state.render_scale),
                    PLUGIN_NAME,
                );
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
    let cam = camera_at(state, state.start.elapsed().as_secs_f64());

    unsafe {
        gl::UseProgram(gpu.program);
        for (loc, c) in gpu.u_c_locs.iter().zip(state.ramp.iter()) {
            gl::Uniform3f(*loc, c[0], c[1], c[2]);
        }
        gl::Uniform1f(gpu.u_aspect_loc, aspect);
        gl::Uniform1f(gpu.u_tan_half_fov_loc, state.tan_half_fov);
        gl::Uniform1f(gpu.u_fog_loc, state.fog_density);
        gl::Uniform3f(gpu.u_cam_pos_loc, cam.pos[0], cam.pos[1], cam.pos[2]);
        gl::Uniform3f(
            gpu.u_cam_right_loc,
            cam.right[0],
            cam.right[1],
            cam.right[2],
        );
        gl::Uniform3f(gpu.u_cam_up_loc, cam.up[0], cam.up[1], cam.up[2]);
        gl::Uniform3f(gpu.u_cam_fwd_loc, cam.fwd[0], cam.fwd[1], cam.fwd[2]);

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
