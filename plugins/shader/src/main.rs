// SPDX-License-Identifier: GPL-3.0-or-later

//! veiland-shader: runs a Shadertoy-convention GLSL fragment shader (an
//! embedded preset or a user file) as a self-paced fullscreen background.
//!
//! The plugin concatenates a Shadertoy-compatible preamble, the shader
//! body, and a `main()` wrapper (see `preamble`), compiles it in a GLES 3
//! context, and renders it into a dmabuf every frame. Content failures
//! (unknown preset, compile error, bad file) never exit: the plugin falls
//! back to a dark fill and logs loudly, matching the wallpaper's contract.

mod config;
mod fallback;
mod glsl;
mod preamble;
mod presets;
mod source;

use std::time::{Duration, Instant};

use veiland_plugin::{
    Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, Rng, gl as vgl,
};

use config::{Settings, Source};

const PLUGIN_NAME: &str = "shader";

// ---- GPU state: the program in use plus its cached uniform locations ----

/// A compiled+linked shader program with every Shadertoy uniform's
/// location resolved once at build time. A location of -1 means the
/// uniform was optimized out (unused by this shader) -- `glUniform*` with
/// -1 is a documented no-op, so we never branch on it.
struct Program {
    program: gl::types::GLuint,
    // Per-frame Shadertoy uniforms we actually feed.
    u_resolution: gl::types::GLint,
    u_time: gl::types::GLint,
    u_time_delta: gl::types::GLint,
    u_frame: gl::types::GLint,
    u_frame_rate: gl::types::GLint,
    u_mouse: gl::types::GLint,
    u_date: gl::types::GLint,
    u_sample_rate: gl::types::GLint,
    // iChannel0..3 sampler units + their resolutions/times (arrays).
    u_channels: [gl::types::GLint; 4],
    u_channel_resolution: gl::types::GLint, // vec3[4], set with Uniform3fv(count=4)
    u_channel_time: gl::types::GLint,       // float[4], set with Uniform1fv(count=4)
    // Layer opacity, present only in the transparent wrapper (-1 and a
    // harmless no-op in the opaque one).
    u_opacity: gl::types::GLint,
    // The quad's vertex attribute, resolved once (the program is fixed for
    // the plugin's life, so the location never changes).
    a_pos: gl::types::GLint,
}

impl Program {
    /// Resolve every uniform location for an already-linked program. Names
    /// match the preamble's declarations exactly. Array uniforms resolve by
    /// their base name ("iChannelResolution"), which is portable for a
    /// whole-array upload.
    unsafe fn locate(program: gl::types::GLuint) -> Self {
        let loc = |n: &std::ffi::CStr| unsafe { gl::GetUniformLocation(program, n.as_ptr()) };
        let u_channels = [
            loc(c"iChannel0"),
            loc(c"iChannel1"),
            loc(c"iChannel2"),
            loc(c"iChannel3"),
        ];
        Self {
            program,
            u_resolution: loc(c"iResolution"),
            u_time: loc(c"iTime"),
            u_time_delta: loc(c"iTimeDelta"),
            u_frame: loc(c"iFrame"),
            u_frame_rate: loc(c"iFrameRate"),
            u_mouse: loc(c"iMouse"),
            u_date: loc(c"iDate"),
            u_sample_rate: loc(c"iSampleRate"),
            u_channels,
            u_channel_resolution: loc(c"iChannelResolution"),
            u_channel_time: loc(c"iChannelTime"),
            u_opacity: loc(c"u_veiland_opacity"),
            a_pos: unsafe { gl::GetAttribLocation(program, c"a_pos".as_ptr()) },
        }
    }
}

// ---- Building the two programs: user shader and fallback ----

/// The fullscreen quad shared by both programs: two triangles covering
/// clip space. Uploaded once; `a_pos` is the only attribute.
const QUAD: [f32; 12] = [
    -1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0,
];

/// The vertex shader paired with every assembled fragment shader. It is
/// `#version 300 es` (to match the fragment stage) and just passes the
/// clip-space quad through -- Shadertoy shaders read `gl_FragCoord`, not a
/// varying, so there is nothing else to forward.
const USER_VERTEX: &[u8] = b"#version 300 es\n\
    precision highp float;\n\
    in vec2 a_pos;\n\
    void main() {\n\
        gl_Position = vec4(a_pos, 0.0, 1.0);\n\
    }\n\0";

/// Compile+link an assembled fragment shader (preamble + body) with the
/// passthrough vertex shader, then resolve uniforms. Returns the full,
/// untruncated compile log on failure so the caller can show the user
/// exactly what went wrong.
unsafe fn build_program(fragment_src: &str) -> Result<Program, String> {
    unsafe {
        // Null-terminate for glShaderSource.
        let mut frag = fragment_src.as_bytes().to_vec();
        frag.push(0);

        let vs = glsl::compile_shader_full_log(gl::VERTEX_SHADER, USER_VERTEX)?;
        let fs = glsl::compile_shader_full_log(gl::FRAGMENT_SHADER, &frag)?;
        // link_program (SDK) is fine here: a link failure of a valid frag +
        // our trivial vertex is our-bug territory, not user-facing product.
        let program = vgl::link_program(vs, fs)?;
        // The shader objects are no longer needed once linked.
        gl::DeleteShader(vs);
        gl::DeleteShader(fs);
        Ok(Program::locate(program))
    }
}

/// Compile+link the fallback program (dark fill). Its uniform locations
/// are all -1 (it declares none), so `Program::locate` is still valid and
/// every per-frame `glUniform*` becomes a harmless no-op.
unsafe fn build_fallback() -> Result<Program, String> {
    unsafe {
        let vs = glsl::compile_shader_full_log(gl::VERTEX_SHADER, fallback::VERTEX)?;
        let fs = glsl::compile_shader_full_log(gl::FRAGMENT_SHADER, fallback::FRAGMENT)?;
        let program = vgl::link_program(vs, fs)?;
        gl::DeleteShader(vs);
        gl::DeleteShader(fs);
        Ok(Program::locate(program))
    }
}

// ---- Channel textures: iChannel0 = noise, iChannel1..3 = black ----

/// The four iChannel textures. Owned for the plugin's life; bound to
/// texture units 0..3 before each draw.
struct Channels {
    textures: [gl::types::GLuint; 4],
}

impl Channels {
    /// Create the channel textures: iChannel0 is a 256x256 RGBA white-noise
    /// LUT (a large cohort of single-pass shaders uses iChannel0 purely as
    /// a noise source, so noise-by-default renders them approximately right
    /// instead of black); iChannel1..3 are 1x1 black.
    unsafe fn new() -> Self {
        // 256x256 RGBA noise from the SDK Rng. One independent byte per
        // channel per texel, so R/G/B/A are uncorrelated -- shaders sampling
        // a single channel still get full-range noise.
        const N: usize = 256;
        let mut rng = Rng::new(0x9E3779B9);
        let mut noise = vec![0u8; N * N * 4];
        for b in noise.iter_mut() {
            *b = (rng.next_f32() * 256.0) as u8;
        }

        let mut textures = [0u32; 4];
        unsafe {
            gl::GenTextures(4, textures.as_mut_ptr());

            // iChannel0: the noise LUT. GL_REPEAT so shaders can tile it,
            // NEAREST so a noise lookup returns a real sample, not a blur.
            gl::BindTexture(gl::TEXTURE_2D, textures[0]);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::REPEAT as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::REPEAT as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
            // 256-wide RGBA rows are 1024 bytes, a multiple of 4, so the
            // default GL_UNPACK_ALIGNMENT of 4 is already satisfied.
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::RGBA as i32,
                N as i32,
                N as i32,
                0,
                gl::RGBA,
                gl::UNSIGNED_BYTE,
                noise.as_ptr() as *const _,
            );

            // iChannel1..3: 1x1 opaque black. Shaders that sample an unused
            // channel get black (Shadertoy's unbound default) rather than
            // whatever garbage an unbound sampler would read.
            let black: [u8; 4] = [0, 0, 0, 255];
            for &tex in &textures[1..4] {
                gl::BindTexture(gl::TEXTURE_2D, tex);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::NEAREST as i32);
                gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::NEAREST as i32);
                gl::TexImage2D(
                    gl::TEXTURE_2D,
                    0,
                    gl::RGBA as i32,
                    1,
                    1,
                    0,
                    gl::RGBA,
                    gl::UNSIGNED_BYTE,
                    black.as_ptr() as *const _,
                );
            }
        }
        Self { textures }
    }

    /// Bind the four channel textures to units 0..3 and point each sampler
    /// uniform at its unit. Call once per frame after `glUseProgram`.
    unsafe fn bind(&self, prog: &Program) {
        unsafe {
            for i in 0..4 {
                gl::ActiveTexture(gl::TEXTURE0 + i as u32);
                gl::BindTexture(gl::TEXTURE_2D, self.textures[i]);
                gl::Uniform1i(prog.u_channels[i], i as i32);
            }
        }
    }
}

// ---- Per-frame uniform state (the CPU-side clock and counters) ----

/// Everything that changes per frame and drives the Shadertoy time/frame
/// uniforms. Constructed once; `tick()` advances it each render.
struct Uniforms {
    /// Plugin start, the origin for iTime.
    start: Instant,
    /// Timestamp of the previous render, for iTimeDelta.
    last_render: Option<Instant>,
    /// iFrame counter, incremented per submitted frame.
    frame: i32,
    /// Smoothed frames-per-second estimate for iFrameRate.
    frame_rate: f32,
    /// Optional iTime wrap period (from loop_seconds); None = raw iTime.
    loop_seconds: Option<f32>,
    /// Latest Configure's wall-clock fields, for iDate. Re-latched on every
    /// Reconfigure. `configure_at` is when we received them, so iDate can
    /// advance by real elapsed time between Configures.
    time_unix_seconds: i64,
    time_tz_offset_seconds: i32,
    configure_at: Instant,
}

/// The values fed to the GPU for a single frame.
struct FrameUniforms {
    time: f32,
    time_delta: f32,
    frame: i32,
    frame_rate: f32,
    /// (year, month, day, seconds-since-local-midnight). Only .w is
    /// meaningful here; see `compute_date`.
    date: [f32; 4],
}

impl Uniforms {
    fn new(loop_seconds: Option<f32>, time_unix_seconds: i64, time_tz_offset_seconds: i32) -> Self {
        let now = Instant::now();
        Self {
            start: now,
            last_render: None,
            frame: 0,
            frame_rate: 60.0, // seeded; smooths to the real rate within a few frames
            loop_seconds,
            time_unix_seconds,
            time_tz_offset_seconds,
            configure_at: now,
        }
    }

    /// Re-latch the wall-clock fields from a fresh Configure. Called on
    /// every Reconfigure so iDate tracks the host's clock (e.g. after a
    /// suspend/resume the host sends a new time).
    fn latch_time(&mut self, time_unix_seconds: i64, time_tz_offset_seconds: i32) {
        self.time_unix_seconds = time_unix_seconds;
        self.time_tz_offset_seconds = time_tz_offset_seconds;
        self.configure_at = Instant::now();
    }

    /// Advance the clock/counters and produce this frame's uniform values.
    /// Call once per render, before issuing draw calls.
    fn tick(&mut self) -> FrameUniforms {
        let now = Instant::now();

        // iTime: raw seconds since start, optionally wrapped. Wrapping
        // trades a periodic pop for bounded f32 precision on long locks.
        let raw = now.duration_since(self.start).as_secs_f32();
        let time = match self.loop_seconds {
            Some(p) if p > 0.0 => raw.rem_euclid(p),
            _ => raw,
        };

        // iTimeDelta: seconds since the previous render (0 on the first).
        let time_delta = self
            .last_render
            .map(|prev| now.duration_since(prev).as_secs_f32())
            .unwrap_or(0.0);
        self.last_render = Some(now);

        // iFrameRate: exponential moving average of 1/dt, ignoring the
        // first frame and absurd deltas (a long pause between reconfigure
        // and first paint shouldn't crater the estimate).
        if time_delta > 1e-4 && time_delta < 1.0 {
            let inst = 1.0 / time_delta;
            self.frame_rate = self.frame_rate * 0.9 + inst * 0.1;
        }

        let date = self.compute_date(now);

        let out = FrameUniforms {
            time,
            time_delta,
            frame: self.frame,
            frame_rate: self.frame_rate,
            date,
        };
        self.frame = self.frame.wrapping_add(1);
        out
    }

    /// Derive iDate. Only `.w` (seconds since local midnight) is computed,
    /// since it is the one component with a real use on a lock screen
    /// (day/night tinting). Year/month/day are `0`: a calendar breakdown
    /// would need either a date dependency (deliberately avoided) or a
    /// hand-rolled algorithm, and no lock-screen shader meaningfully uses
    /// the date components. The uniform still exists so date-aware shaders
    /// compile. If a real calendar is ever wanted, that is the one place a
    /// date crate would earn its keep.
    fn compute_date(&self, now: Instant) -> [f32; 4] {
        let elapsed = now.duration_since(self.configure_at).as_secs_f64();
        // Local-time unix seconds = UTC unix + tz offset, so the modulo
        // gives seconds since *local* midnight.
        let local =
            self.time_unix_seconds as f64 + f64::from(self.time_tz_offset_seconds) + elapsed;
        let secs_of_day = local.rem_euclid(86_400.0);
        [0.0, 0.0, 0.0, secs_of_day as f32]
    }
}

// ---- Assemble + build the selected shader, or fall back ----

/// Resolve the config's `Source` to a shader body: an embedded preset or
/// the contents of the user's file. `None` on any content failure, already
/// logged.
fn resolve_body(settings: &Settings) -> Option<String> {
    match &settings.source {
        Source::Preset(name) => match presets::get(name) {
            Some(b) => Some(b.to_string()),
            None => {
                eprintln!(
                    "veiland-{PLUGIN_NAME}: unknown preset {name:?}; valid presets: {}. \
                     Using fallback.",
                    presets::names()
                );
                None
            }
        },
        Source::Path(path) => source::load_body(PLUGIN_NAME, path),
    }
}

/// How to name the selected shader in user-facing output.
fn describe(src: &Source) -> String {
    match src {
        Source::Preset(name) => format!("preset {name:?}"),
        Source::Path(path) => format!("{path:?}"),
    }
}

/// The compile log plus a hint for reading its line numbers.
fn annotate_log(log: &str) -> String {
    format!(
        "{log}\n(if these line numbers look too high, your driver ignored #line; \
         subtract {} for line numbers in your file)",
        preamble::preamble_lines()
    )
}

/// Resolve the config's `Source` to a compiled `Program`. On any content
/// failure (unknown preset, unreadable file, compile error) this logs
/// loudly and returns `None`; the caller then uses the fallback program.
/// It never returns `Err` for content problems -- staying alive on bad
/// content is the whole contract.
fn build_selected(settings: &Settings) -> Option<Program> {
    let body = resolve_body(settings)?;

    // opacity == 1.0 uses the opaque wrapper (shader alpha ignored); any
    // value below 1.0 uses the transparent wrapper so the layer composites
    // over what is below it.
    let opaque = settings.opacity >= 1.0;
    let source = preamble::assemble(&body, opaque);
    match unsafe { build_program(&source) } {
        Ok(prog) => Some(prog),
        Err(log) => {
            // The compile log is the product: print it verbatim so the user
            // can see exactly what failed.
            eprintln!(
                "veiland-{PLUGIN_NAME}: shader failed to compile:\n{}",
                annotate_log(&log)
            );
            None
        }
    }
}

// ---- Buffer sizing (render_scale), shared with reconfigure ----

/// Buffer dimension after render_scale, clamped to the protocol's [1, 8192].
fn scaled_dim(dim: u32, scale: f64) -> u32 {
    ((f64::from(dim) * scale).round() as u32).clamp(1, 8192)
}

fn run() -> Result<(), PluginError> {
    eprintln!(
        "veiland-{} (pid {}) starting",
        PLUGIN_NAME,
        std::process::id()
    );

    let raw = veiland_plugin::load_config::<config::Config>(PLUGIN_NAME);
    let settings = Settings::from(raw);
    eprintln!(
        "veiland-{PLUGIN_NAME}: source={:?} render_scale={} max_fps={:?} loop_seconds={:?}",
        settings.source, settings.render_scale, settings.max_fps, settings.loop_seconds
    );

    // ES3 is required to compile #version 300 es. If it is unavailable the
    // plugin can't do its job -- propagate and exit; the host draws the
    // region fallback like it does for any plugin whose GL setup fails.
    let gbm_egl = GbmEgl::new_es3()?;

    let mut conn = Connection::connect(PLUGIN_NAME, env!("CARGO_PKG_VERSION"))?;
    eprintln!("connected to host, hello sent");
    eprintln!(
        "sync model: {}",
        if conn.host_supports_fence_fd() && gbm_egl.supports_fence_fd() {
            "fast (fence fd)"
        } else {
            "slow (glFinish)"
        },
    );

    let first = match conn.wait_for_configure()? {
        Some(c) => c,
        None => {
            eprintln!("veiland-{PLUGIN_NAME}: shutdown before first configure");
            return Ok(());
        }
    };
    eprintln!(
        "veiland-{PLUGIN_NAME}: first configure {}x{} scale_120={}",
        first.region_w, first.region_h, first.scale_120
    );

    let mut dma = DmaBuffer::new(
        &gbm_egl,
        scaled_dim(first.region_w, settings.render_scale),
        scaled_dim(first.region_h, settings.render_scale),
    )?;
    dma.bind_for_rendering()?;

    // Build the fallback first so it is always available, then the selected
    // shader. `active` points at whichever we will actually draw.
    let fallback = unsafe { build_fallback() }.map_err(|e| {
        eprintln!("veiland-{PLUGIN_NAME}: fallback shader failed to build: {e}");
        PluginError::Render("fallback shader build failed")
    })?;
    let selected = build_selected(&settings);
    let using_fallback = selected.is_none();
    let active = selected.as_ref().unwrap_or(&fallback);
    // The fallback is an opaque dark fill regardless of config, so failure
    // stays visible-but-safe; only a working selected shader honours the
    // configured opacity.
    let opacity = if using_fallback {
        1.0
    } else {
        settings.opacity
    };
    if using_fallback {
        eprintln!("veiland-{PLUGIN_NAME}: rendering fallback fill");
    }

    // Fullscreen quad, uploaded once and shared by both programs.
    let mut vbo: gl::types::GLuint = 0;
    unsafe {
        gl::GenBuffers(1, &mut vbo);
        gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
        gl::BufferData(
            gl::ARRAY_BUFFER,
            std::mem::size_of_val(&QUAD) as isize,
            QUAD.as_ptr() as *const _,
            gl::STATIC_DRAW,
        );
    }

    let channels = unsafe { Channels::new() };
    let mut uniforms = Uniforms::new(
        settings.loop_seconds,
        first.time_unix_seconds,
        first.time_tz_offset_seconds,
    );

    // Self-paced with an optional frame-rate cap, exactly the raymarcher's
    // model: the host keeps compositing the last submitted buffer, so
    // sleeping before the next submit caps our duty cycle for free.
    let frame_budget = settings
        .max_fps
        .map(|fps| Duration::from_secs_f64(1.0 / f64::from(fps)));
    let mut pacer = FramePacer::self_paced();
    let mut last_submit: Option<Instant> = None;

    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                if let (Some(budget), Some(prev)) = (frame_budget, last_submit) {
                    let since = prev.elapsed();
                    if since < budget {
                        std::thread::sleep(budget - since);
                    }
                }
                render(&dma, active, &channels, &mut uniforms, vbo, opacity)?;
                conn.submit_frame(&dma, &gbm_egl)?;
                last_submit = Some(Instant::now());
                pacer.submitted();
            }
            Frame::Reconfigure(c) => {
                dma.resize_or_keep(
                    &gbm_egl,
                    scaled_dim(c.region_w, settings.render_scale),
                    scaled_dim(c.region_h, settings.render_scale),
                    PLUGIN_NAME,
                );
                uniforms.latch_time(c.time_unix_seconds, c.time_tz_offset_seconds);
            }
            Frame::Shutdown => {
                eprintln!("host requested shutdown");
                return Ok(());
            }
        }
    }
}

/// Render one frame: bind the buffer, feed uniforms, bind channels, draw
/// the fullscreen quad. Uniform locations that don't exist in the active
/// program are -1, which makes every `glUniform*` a no-op -- so the same
/// path drives both the user shader and the uniform-less fallback.
fn render(
    dma: &DmaBuffer,
    prog: &Program,
    channels: &Channels,
    uniforms: &mut Uniforms,
    vbo: gl::types::GLuint,
    opacity: f32,
) -> Result<(), PluginError> {
    dma.bind_for_rendering()?;
    let w = dma.width() as f32;
    let h = dma.height() as f32;
    let u = uniforms.tick();
    let transparent = opacity < 1.0;

    unsafe {
        // In transparent mode the shader writes alpha 0 where it wants the
        // layer below to show through, but the fragment shader still runs on
        // every pixel and overwrites the buffer, so a clear is not strictly
        // required. It is cheap insurance against any pixel the draw misses
        // (none today, one full-screen quad) leaving stale opaque data.
        if transparent {
            gl::ClearColor(0.0, 0.0, 0.0, 0.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }

        gl::UseProgram(prog.program);

        // Layer opacity (transparent wrapper only; -1 no-op otherwise).
        gl::Uniform1f(prog.u_opacity, opacity);

        // Per-frame Shadertoy uniforms.
        gl::Uniform3f(prog.u_resolution, w, h, 1.0);
        gl::Uniform1f(prog.u_time, u.time);
        gl::Uniform1f(prog.u_time_delta, u.time_delta);
        gl::Uniform1i(prog.u_frame, u.frame);
        gl::Uniform1f(prog.u_frame_rate, u.frame_rate);
        gl::Uniform4f(prog.u_mouse, 0.0, 0.0, 0.0, 0.0);
        gl::Uniform4f(prog.u_date, u.date[0], u.date[1], u.date[2], u.date[3]);
        gl::Uniform1f(prog.u_sample_rate, 44100.0);

        // Channel time is all zeros (no audio/video channels); channel
        // resolution reports the noise size for iChannel0 and 0 elsewhere.
        let channel_time = [0.0f32; 4];
        gl::Uniform1fv(prog.u_channel_time, 4, channel_time.as_ptr());
        let channel_res: [f32; 12] = [
            256.0, 256.0, 1.0, // iChannel0: the noise LUT
            0.0, 0.0, 1.0, // iChannel1: 1x1 black
            0.0, 0.0, 1.0, // iChannel2
            0.0, 0.0, 1.0, // iChannel3
        ];
        gl::Uniform3fv(prog.u_channel_resolution, 4, channel_res.as_ptr());

        channels.bind(prog);

        // Bind the quad and draw. a_pos was resolved at program build time.
        gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
        if prog.a_pos >= 0 {
            gl::EnableVertexAttribArray(prog.a_pos as u32);
            gl::VertexAttribPointer(
                prog.a_pos as u32,
                2,
                gl::FLOAT,
                gl::FALSE,
                0,
                std::ptr::null(),
            );
        }
        gl::DrawArrays(gl::TRIANGLES, 0, 6);
    }

    Ok(())
}

// ---- Command line ----

/// What the command line asked for.
enum Mode {
    /// Normal operation: connect to the host and render.
    Run,
    /// Compile the selected shader, print the log, exit. `Some(path)`
    /// checks that file; `None` checks whatever the config selects.
    Check(Option<String>),
    /// Print usage or version and exit 0.
    Print(String),
    /// Bad usage: print to stderr and exit 2.
    Usage(String),
}

const USAGE: &str = "\
veiland-shader: runs a Shadertoy-convention GLSL shader as a background.

Normally spawned by veiland; see docs/config.md for the [plugin.config] keys.

Usage:
  veiland-shader                 run as a plugin (needs a host)
  veiland-shader --check [FILE]  compile FILE (or the configured shader),
                                 print the compile log, exit 0 on success
  veiland-shader --help
  veiland-shader --version
";

fn parse_args() -> Mode {
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        None => Mode::Run,
        Some("--check") => {
            let file = args.next();
            match args.next() {
                Some(extra) => Mode::Usage(format!("unexpected argument {extra:?}")),
                None => Mode::Check(file),
            }
        }
        Some("--help" | "-h") => Mode::Print(USAGE.to_string()),
        Some("--version" | "-V") => Mode::Print(format!(
            "{} {}\n",
            env!("CARGO_PKG_NAME"),
            env!("CARGO_PKG_VERSION")
        )),
        Some(other) => Mode::Usage(format!("unknown argument {other:?}")),
    }
}

/// Compile the selected shader and report, without connecting to a host.
/// Returns the process exit code.
fn check(file: Option<String>) -> i32 {
    // An explicit FILE overrides the config; without one we check whatever
    // the config selects, so --check validates a real setup too.
    let settings = match file {
        Some(path) => Settings::from(config::Config {
            path: Some(path),
            ..Default::default()
        }),
        None => Settings::from(veiland_plugin::load_config::<config::Config>(PLUGIN_NAME)),
    };

    let body = match resolve_body(&settings) {
        Some(b) => b,
        None => return 1, // resolve_body logged why
    };

    // Compiling needs a live GL context; it must outlive build_program, so
    // it is bound rather than dropped. No dmabuf and no host are involved.
    let _gbm_egl = match GbmEgl::new_es3() {
        Ok(g) => g,
        Err(e) => {
            eprintln!("veiland-{PLUGIN_NAME}: no GLES3 context, cannot compile: {e}");
            return 1;
        }
    };

    let source = preamble::assemble(&body, settings.opacity >= 1.0);
    match unsafe { build_program(&source) } {
        Ok(_) => {
            println!("{}: compiles cleanly", describe(&settings.source));
            0
        }
        Err(log) => {
            println!("{}: compile failed", describe(&settings.source));
            println!("{}", annotate_log(&log));
            1
        }
    }
}

fn main() {
    match parse_args() {
        Mode::Run => {
            if let Err(e) = run() {
                eprintln!("{}: {}", env!("CARGO_PKG_NAME"), e);
                std::process::exit(1);
            }
        }
        Mode::Check(file) => std::process::exit(check(file)),
        Mode::Print(text) => print!("{text}"),
        Mode::Usage(msg) => {
            eprintln!("{}: {msg}\n\n{USAGE}", env!("CARGO_PKG_NAME"));
            std::process::exit(2);
        }
    }
}
