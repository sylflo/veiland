// SPDX-License-Identifier: GPL-3.0-or-later

//! Animated gradient — reference plugin built on `veiland-plugin`.
//! Renders a diagonal RGB gradient that drifts over time into a
//! dmabuf-backed FBO, hands the dmabuf to the host on every
//! `FrameDone`.

use std::time::Instant;

use veiland_plugin::{Connection, DmaBuffer, GbmEgl, PluginError};
use veiland_protocol::{Buffer, ServerMessage};

const BUFFER_WIDTH: u32 = 512;
const BUFFER_HEIGHT: u32 = 512;

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
            gl::GetShaderInfoLog(shader, log.len() as i32, &mut len, log.as_mut_ptr() as *mut _);
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
            gl::GetProgramInfoLog(program, log.len() as i32, &mut len, log.as_mut_ptr() as *mut _);
            panic!(
                "program link failed: {}",
                std::str::from_utf8(&log[..len as usize]).unwrap_or("<invalid utf8>")
            );
        }
        program
    }
}

/// Compile shaders, upload VBO, return (program, u_time location).
/// Called once at startup; drawing reuses these every frame.
unsafe fn build_gradient_program() -> (gl::types::GLuint, gl::types::GLint) {
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
        let vs = compile_shader(gl::VERTEX_SHADER, vs_src);
        let fs = compile_shader(gl::FRAGMENT_SHADER, fs_src);
        let program = link_program(vs, fs);
        gl::UseProgram(program);

        let quad: [f32; 12] = [
            -1.0, -1.0,
             1.0, -1.0,
            -1.0,  1.0,
            -1.0,  1.0,
             1.0, -1.0,
             1.0,  1.0,
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

        let a_pos = gl::GetAttribLocation(program, b"a_pos\0".as_ptr() as *const _);
        gl::EnableVertexAttribArray(a_pos as u32);
        gl::VertexAttribPointer(a_pos as u32, 2, gl::FLOAT, gl::FALSE, 0, std::ptr::null());

        let u_time = gl::GetUniformLocation(program, b"u_time\0".as_ptr() as *const _);

        (program, u_time)
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
    let (_program, u_time_loc) = unsafe { build_gradient_program() };

    // 3. Connect to host. Reads fd from VEILAND_PLUGIN_SOCKET=3,
    //    negotiates protocol version, sends Hello.
    let mut conn = Connection::from_env()?;
    conn.handshake()?;
    conn.send_hello("gradient", env!("CARGO_PKG_VERSION"))?;
    eprintln!("connected to host, hello sent");

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

    // 5. Canonical event loop: receive a ServerMessage, react. We
    //    drive our own match — veiland-plugin gives us primitives,
    //    not a callback runner.
    loop {
        match conn.recv_event()? {
            ServerMessage::Configure(c) => {
                eprintln!(
                    "configure: region=({},{}) {}x{} scale={}",
                    c.region_x, c.region_y, c.region_w, c.region_h, c.scale
                );
                // For M3 single-buffer we ignore the region — we render
                // at our fixed 512×512 and the host stretches. M6 lets
                // us actually respond to region changes.
            }
            ServerMessage::FrameDone => {
                let t = start.elapsed().as_secs_f32();
                dma.bind_for_rendering()?;
                // SAFETY: bind_for_rendering left an FBO and our program
                // current; the GL context is on this thread.
                unsafe {
                    gl::Viewport(0, 0, BUFFER_WIDTH as i32, BUFFER_HEIGHT as i32);
                    gl::Uniform1f(u_time_loc, t);
                    gl::Clear(gl::COLOR_BUFFER_BIT);
                    gl::DrawArrays(gl::TRIANGLES, 0, 6);
                }
                dma.finish();
                conn.send_buffer(&buf_msg, dma.dmabuf_fd())?;
            }
            ServerMessage::BufferReleased(_) => {
                // M5+: would return this buffer to our pool. M3 single
                // buffer: nothing to do.
            }
            ServerMessage::Shutdown => {
                eprintln!("host requested shutdown");
                return Ok(());
            }
        }
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("veiland-gradient: {}", e);
        std::process::exit(1);
    }
}
