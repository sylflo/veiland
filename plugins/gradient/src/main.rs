// SPDX-License-Identifier: GPL-3.0-or-later

//! Animated gradient — reference plugin built on `veiland-plugin`.
//! Renders a diagonal RGB gradient that drifts over time into a
//! dmabuf-backed FBO, hands the dmabuf to the host on every
//! `FrameDone`.

use std::time::Instant;

use veiland_plugin::{
    gl as vgl, Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, SyncFence,
};
use veiland_protocol::Buffer;

const BUFFER_WIDTH: u32 = 512;
const BUFFER_HEIGHT: u32 = 512;

/// Compile shaders, upload VBO, return (program, u_time location).
/// Called once at startup; drawing reuses these every frame.
unsafe fn build_gradient_program() -> Result<(gl::types::GLuint, gl::types::GLint), String> {
    let vs_src = b"#version 100\n\
        attribute vec2 a_pos;\n\
        varying vec2 v_uv;\n\
        void main() {\n\
            v_uv = a_pos * 0.5 + 0.5;\n\
            gl_Position = vec4(a_pos, 0.0, 1.0);\n\
        }\n\0";

    let fs_src = b"#version 100\n\
        precision mediump float;\n\
        varying vec2 v_uv;\n\
        uniform float u_time;\n\
        void main() {\n\
            float r = v_uv.x;\n\
            float g = v_uv.y;\n\
            float b = 0.5 + 0.5 * sin(u_time);\n\
            gl_FragColor = vec4(r, g, b, 1.0);\n\
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

        let u_time = gl::GetUniformLocation(program, c"u_time".as_ptr());

        Ok((program, u_time))
    }
}

fn run() -> Result<(), PluginError> {
    eprintln!("veiland-gradient (pid {}) starting", std::process::id());

    // 1. Set up render context + dmabuf. veiland-plugin hides the
    //    /dev/dri/renderD128 open, the EGL display/context/config dance,
    //    and the dmabuf → EGLImage → FBO plumbing.
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

    // 2. Compile gradient shader once. Subsequent frames re-bind and
    //    re-draw with an updated time uniform.
    dma.bind_for_rendering()?;
    let (_program, u_time_loc) = unsafe { build_gradient_program() }.map_err(|e| {
        eprintln!("veiland-gradient: {e}");
        PluginError::Render("shader build failed")
    })?;

    // 3. Connect to host. Reads fd from VEILAND_PLUGIN_SOCKET=3,
    //    negotiates protocol version, sends Hello — all in one call.
    let mut conn = Connection::connect("gradient", env!("CARGO_PKG_VERSION"))?;
    eprintln!("connected to host, hello sent");

    // 3b. Decide fast/slow path once, after the handshake. Both sides must
    //     support EGL_ANDROID_native_fence_sync for the fast path. This
    //     choice is fixed for the connection's lifetime (protocol.md §6.2).
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

    // 4. Build the Buffer message once. Fields are constant for the
    //    lifetime of `dma`; we re-send the same struct (with id = 0)
    //    on every FrameDone. M5+ buffer pool will use real ids.
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

    // 5. Canonical event loop. veiland-plugin gives us primitives, not a
    //    callback runner: FramePacer owns the FrameDone/BufferReleased
    //    pacing state machine, and we drive our own three-arm match.
    //
    //    Self-paced: render again on every BufferReleased (after the
    //    first FrameDone), so the compositor's repaint rate drives the
    //    drift instead of the host's input-event cadence. The region in
    //    Configure is still ignored — we render at our fixed 512×512 and
    //    the host stretches; making this region-aware is a separate
    //    change (see docs/plugin-primitive-migration-plan.md).
    let mut pacer = FramePacer::self_paced();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
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
    // SAFETY: bind_for_rendering left an FBO and our program current;
    // the GL context is on this thread.
    unsafe {
        gl::Viewport(0, 0, BUFFER_WIDTH as i32, BUFFER_HEIGHT as i32);
        gl::Uniform1f(u_time_loc, t);
        gl::Clear(gl::COLOR_BUFFER_BIT);
        gl::DrawArrays(gl::TRIANGLES, 0, 6);
    }

    if fast_path {
        // Submit draw commands; insert + export a fence. The host
        // waits on it before sampling. SAFETY: gl::Flush requires a
        // current context; same invariant as the draw calls above.
        unsafe {
            gl::Flush();
        }
        let fence = SyncFence::create(gbm_egl)?;
        conn.send_buffer(buf_msg, dma.dmabuf_fd(), Some(fence.as_fd()))?;
        // `fence` drops here: destroy_sync on the local handle and
        // close the local fd. The dma-fence kernel object stays
        // alive via the cmsg dup that travelled to the host.
    } else {
        // Slow path: drain the GPU before send, so the dmabuf is
        // fully written by the time the host receives it.
        dma.finish();
        conn.send_buffer(buf_msg, dma.dmabuf_fd(), None)?;
    }
    Ok(())
}

fn main() {
    if let Err(e) = run() {
        eprintln!("veiland-gradient: {}", e);
        std::process::exit(1);
    }
}
