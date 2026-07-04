// SPDX-License-Identifier: GPL-3.0-or-later

//! Stress plugin — deliberately heavy fragment shader, used to measure
//! the per-frame round-trip cost before and after the fence-based
//! sync path.
//!
//! The fragment shader runs `ITERATIONS` iterations of a sin/cos loop
//! per pixel. With `ITERATIONS = 0` we measure the IPC + composite
//! floor (no GPU work to hide); with `ITERATIONS = N_heavy` we
//! measure a workload M5a should be able to overlap with the host's
//! compositing.
//!
//! The plugin times the round-trip from one `FrameDone` to the next
//! and prints a rolling average every 60 frames. Plugin-side timing
//! captures the *full* round-trip — render + send + host sample +
//! FrameDone — which is the number M5a is supposed to improve.

use std::time::Instant;

use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, SyncFence};
use veiland_protocol::Buffer;

/// Render-target size. 1920×1080 covers the common-desktop case; the
/// host will scale to fit if your display is smaller. Tune down for
/// a per-pixel-cheaper workload, up to push pixel-fill harder.
const BUFFER_WIDTH: u32 = 1920;
const BUFFER_HEIGHT: u32 = 1080;

/// How many nested sin/cos iterations the fragment shader runs per
/// pixel. The per-iteration work is data-dependent (chained `sin(...)`
/// with the previous result as input) so the GLSL compiler cannot
/// CSE, reorder, or collapse the loop — the work the GPU does scales
/// roughly linearly with this number.
///
/// **The committed default is `0` — the IPC floor measurement.** With
/// `ITERATIONS = 0` the shader runs the loop zero times and emits a
/// flat colour; per-frame GPU work is essentially zero. The round-trip
/// dt you see in stderr is then dominated by the host's compositing,
/// the IPC, and (on a Wayland compositor) the vsync wait. This is
/// half of the M5a step-0 baseline.
///
/// **For the workload measurement**, crank `ITERATIONS` until the dt
/// in stderr exceeds 16.67 ms (i.e. falls off vsync). Then you're
/// measuring GPU-bound time and M5a's parallelism will move the
/// number. Order-of-magnitude starting points (empirical, your
/// hardware will differ — *record what you actually chose alongside
/// the resulting dt in your M5 tracking notes*):
///
/// - **Older / mobile Intel iGPU**: start at `~1000`, expect to land
///   in the 1000–5000 range.
/// - **Mesa on midrange Intel / AMD APU**: 5000–20000.
/// - **NVIDIA dGPU (proprietary driver)**: ≥20000; the dGPU has so
///   much ALU headroom that you may need 50000+ to fall off vsync at
///   1920×1080. On a fast dGPU you can also turn `BUFFER_WIDTH/HEIGHT`
///   up to push pixel count instead of per-pixel work.
///
/// Recompile (`cargo build -p veiland-stress`) after changing.
const ITERATIONS: u32 = 300;

/// How often to print rolling-average frame time. 60 frames ≈ 1 second
/// at 60 Hz so the log feels approximately one-line-per-second.
const LOG_EVERY_N_FRAMES: u32 = 60;

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

/// Build the stress program. Vertex shader is a pass-through quad.
/// Fragment shader runs a sin/cos accumulation loop per pixel. The
/// loop count is baked into the source as a literal so the GLSL
/// compiler can keep the loop bounds constant — most drivers will
/// refuse or pessimise dynamic-bounded loops in ES 1.00.
unsafe fn build_stress_program(iterations: u32) -> (gl::types::GLuint, gl::types::GLint) {
    let vs_src = b"#version 100\n\
        attribute vec2 a_pos;\n\
        varying vec2 v_uv;\n\
        void main() {\n\
            v_uv = a_pos * 0.5 + 0.5;\n\
            gl_Position = vec4(a_pos, 0.0, 1.0);\n\
        }\n\0";

    // Fragment shader source is built at runtime so we can bake the
    // iteration count as a GLSL `const int`. We need it to be a
    // compile-time literal for `for (int i = 0; i < N; ++i)` to be
    // accepted by every driver — GLES 1.00 requires statically-bounded
    // loops.
    let fs_src = format!(
        "#version 100\n\
         precision highp float;\n\
         varying vec2 v_uv;\n\
         uniform float u_time;\n\
         const int ITERATIONS = {iters};\n\
         void main() {{\n\
             vec3 acc = vec3(0.0);\n\
             float t = u_time;\n\
             for (int i = 0; i < ITERATIONS; ++i) {{\n\
                 // Nested sin/cos chains create a true data dependency\n\
                 // between consecutive transcendentals: the optimiser\n\
                 // can't reorder, CSE, or collapse them. Pure ALU burn\n\
                 // the compiler is forced to actually emit.\n\
                 float fi = float(i);\n\
                 vec2 p = v_uv * 30.0 + vec2(fi * 0.13, fi * 0.17) + t;\n\
                 acc.r += sin(p.x + sin(p.y + sin(t + fi)));\n\
                 acc.g += cos(p.y + cos(p.x + cos(t - fi)));\n\
                 acc.b += sin(length(p) + cos(p.x * p.y + fi));\n\
             }}\n\
             // Normalise so the colour is visible regardless of ITERATIONS.\n\
             // Adding 1 avoids division by zero when ITERATIONS == 0.\n\
             float n = float(ITERATIONS) + 1.0;\n\
             vec3 col = 0.5 + 0.5 * (acc / n);\n\
             gl_FragColor = vec4(col, 1.0);\n\
         }}\n\0",
        iters = iterations,
    );

    unsafe {
        let vs = compile_shader(gl::VERTEX_SHADER, vs_src);
        let fs = compile_shader(gl::FRAGMENT_SHADER, fs_src.as_bytes());
        let program = link_program(vs, fs);
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

        let u_time = gl::GetUniformLocation(program, c"u_time".as_ptr());

        (program, u_time)
    }
}

fn run() -> Result<(), PluginError> {
    eprintln!(
        "veiland-stress (pid {}) starting, ITERATIONS = {}",
        std::process::id(),
        ITERATIONS,
    );

    // 1. Render context + dmabuf. Same primitives the gradient plugin
    //    uses; veiland-plugin hides the renderD128 / EGL / GBM dance.
    let gbm_egl = GbmEgl::new()?;
    let dma = DmaBuffer::new(&gbm_egl, BUFFER_WIDTH, BUFFER_HEIGHT)?;
    eprintln!(
        "allocated {}x{} {:?}, modifier=0x{:016x}, stride={}",
        dma.width(),
        dma.height(),
        dma.format(),
        u64::from(dma.modifier()),
        dma.stride(),
    );

    // 2. Compile the stress program with ITERATIONS baked in.
    dma.bind_for_rendering()?;
    let (_program, u_time_loc) = unsafe { build_stress_program(ITERATIONS) };

    // 3. Protocol bootstrap: connect, handshake, Hello — one call.
    let mut conn = Connection::connect("stress", env!("CARGO_PKG_VERSION"))?;
    eprintln!("connected to host, hello sent");

    // 3b. Decide fast/slow path once. The whole point of this plugin is
    //     to measure M5a's pipeline overlap, so the path decision is the
    //     measurement-relevant variable: step 0's baselines were taken
    //     on the slow path; step 12 reruns on whichever path the boxes
    //     end up using. Log clearly so the per-box numbers can be
    //     attributed correctly.
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

    // 4. Buffer message: constant for the lifetime of `dma`. Same
    //    single-buffer model as gradient — id = 0, reused every frame.
    let buf_msg = Buffer {
        id: 0,
        width: dma.width(),
        height: dma.height(),
        format: dma.format(),
        modifier: dma.modifier(),
        stride: dma.stride(),
        offset: 0,
    };

    let start = Instant::now();

    // Frame timing state. `last_render` records the start of the most
    // recently rendered frame; `frame_count` and `accumulated_dt` build
    // the rolling average we print every LOG_EVERY_N_FRAMES.
    //
    // Migration note: this used to time FrameDone-to-FrameDone (every
    // FrameDone, including deferred ones). FramePacer absorbs deferred
    // FrameDones and only surfaces Frame::Render, so we now time
    // render-to-render. Under the host's current strict ordering
    // (BufferReleased before the next FrameDone) deferral never fires,
    // so the two are 1:1 and the numbers are unchanged in practice. If
    // a future host relaxes that ordering, deferred FrameDones would no
    // longer be counted here. See docs/plugin-primitive-migration-plan.md.
    let mut last_render: Option<Instant> = None;
    let mut frame_count: u32 = 0;
    let mut accumulated_dt: f64 = 0.0;

    // 5. Canonical event loop. FramePacer owns the pacing state machine;
    //    self_paced renders on every BufferReleased after the first
    //    FrameDone, so the round-trip runs back-to-back — exactly the
    //    cadence this plugin is measuring.
    let mut pacer = FramePacer::self_paced();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                let now = Instant::now();

                // Roll the frame-time average. Skip the first render (no
                // previous timestamp to diff against); from the second
                // render onward, accumulate dt and print every Nth frame.
                if let Some(prev) = last_render {
                    let dt_ms = now.duration_since(prev).as_secs_f64() * 1000.0;
                    accumulated_dt += dt_ms;
                    frame_count += 1;

                    if frame_count >= LOG_EVERY_N_FRAMES {
                        let avg = accumulated_dt / frame_count as f64;
                        eprintln!(
                            "stress: ITERATIONS={} avg frame dt = {:.2} ms ({:.1} fps) over last {} frames",
                            ITERATIONS,
                            avg,
                            1000.0 / avg,
                            frame_count,
                        );
                        accumulated_dt = 0.0;
                        frame_count = 0;
                    }
                }
                last_render = Some(now);

                render_and_send(
                    &dma,
                    &gbm_egl,
                    &mut conn,
                    &buf_msg,
                    u_time_loc,
                    start.elapsed().as_secs_f32(),
                    fast_path,
                )?;
                pacer.submitted();
            }
            Frame::Reconfigure(c) => {
                eprintln!(
                    "configure: region=({},{}) {}x{} scale_120={}",
                    c.region_x, c.region_y, c.region_w, c.region_h, c.scale_120
                );
            }
            Frame::Shutdown => {
                eprintln!("host requested shutdown");
                return Ok(());
            }
        }
    }
}

/// Render one frame into the dmabuf and ship it. Extracted to dedupe
/// the FrameDone and "deferred FrameDone via BufferReleased" call sites.
fn render_and_send(
    dma: &DmaBuffer,
    gbm_egl: &GbmEgl,
    conn: &mut Connection,
    buf_msg: &Buffer,
    u_time_loc: gl::types::GLint,
    t: f32,
    fast_path: bool,
) -> Result<(), PluginError> {
    dma.bind_for_rendering()?;
    // SAFETY: bind_for_rendering left an FBO bound and our program
    // current; the GL context is on this thread.
    unsafe {
        gl::Viewport(0, 0, BUFFER_WIDTH as i32, BUFFER_HEIGHT as i32);
        gl::Uniform1f(u_time_loc, t);
        gl::Clear(gl::COLOR_BUFFER_BIT);
        gl::DrawArrays(gl::TRIANGLES, 0, 6);
    }

    if fast_path {
        // Submit drawn commands + fence; host waits on the fence
        // before sampling so this thread doesn't need to block.
        // SAFETY: gl::Flush requires a current context.
        unsafe {
            gl::Flush();
        }
        let fence = SyncFence::create(gbm_egl)?;
        conn.send_buffer(buf_msg, dma.dmabuf_fd(), Some(fence.as_fd()))?;
    } else {
        // Slow path: drain the GPU before send. M3 baseline.
        dma.finish();
        conn.send_buffer(buf_msg, dma.dmabuf_fd(), None)?;
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("veiland-stress: {}", e);
        std::process::exit(1);
    }
}
