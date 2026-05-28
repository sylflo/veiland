// SPDX-License-Identifier: GPL-3.0-or-later

//! M6 throwaway test plugin — fills its region with blue at 50%
//! opacity. Exists to validate the host's per-region compositing
//! and straight-alpha blending. Not a reference plugin; M7 will
//! produce polished reference plugins (clock, wallpaper, shader-bg).
//!
//! Sister plugins: `red-box` (~78% opaque), `green-box` (fully
//! opaque). All three share the same shape; only the colour
//! constants and the name strings differ.

use veiland_plugin::{Connection, DmaBuffer, GbmEgl, PluginError, SyncFence};
use veiland_protocol::{Buffer, ServerMessage};

const PLUGIN_NAME: &str = "blue-box";

const BUFFER_WIDTH: u32 = 64;
const BUFFER_HEIGHT: u32 = 64;

// Blue at 70% opacity — bright enough on its own (so the contrast
// with green-covers-blue is loud), still partly transparent (so
// red-over-blue produces a visibly purple overlap zone). 50% was
// the plan's first guess and produced a too-dim blue.
const COLOR_R: f32 = 0.0;
const COLOR_G: f32 = 0.0;
const COLOR_B: f32 = 1.0;
const COLOR_A: f32 = 0.7;

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

unsafe fn build_solid_program() -> gl::types::GLuint {
    let vs_src = b"#version 100\n\
        attribute vec2 a_pos;\n\
        void main() {\n\
            gl_Position = vec4(a_pos, 0.0, 1.0);\n\
        }\n\0";

    let fs_src = format!(
        "#version 100\n\
         precision mediump float;\n\
         void main() {{\n\
             gl_FragColor = vec4({:.4}, {:.4}, {:.4}, {:.4});\n\
         }}\n",
        COLOR_R, COLOR_G, COLOR_B, COLOR_A
    );
    let fs_src_nul = std::ffi::CString::new(fs_src).expect("no interior NUL in shader");

    unsafe {
        let vs = compile_shader(gl::VERTEX_SHADER, vs_src);
        let fs = compile_shader(gl::FRAGMENT_SHADER, fs_src_nul.as_bytes_with_nul());
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

        let a_pos = gl::GetAttribLocation(program, b"a_pos\0".as_ptr() as *const _);
        gl::EnableVertexAttribArray(a_pos as u32);
        gl::VertexAttribPointer(a_pos as u32, 2, gl::FLOAT, gl::FALSE, 0, std::ptr::null());

        program
    }
}

fn run() -> Result<(), PluginError> {
    eprintln!(
        "veiland-{} (pid {}) starting",
        PLUGIN_NAME,
        std::process::id()
    );

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

    dma.bind_for_rendering()?;
    let _program = unsafe { build_solid_program() };

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

    let buf_msg = Buffer {
        id: 0,
        width: dma.width(),
        height: dma.height(),
        format: dma.format(),
        modifier: dma.modifier(),
        stride: dma.stride(),
        offset: 0,
    };

    let mut buffer_released = true;
    let mut pending_frame = false;

    loop {
        match conn.recv_event()? {
            ServerMessage::Configure(c) => {
                eprintln!(
                    "configure: region=({},{}) {}x{} scale={}",
                    c.region_x, c.region_y, c.region_w, c.region_h, c.scale
                );
            }
            ServerMessage::FrameDone => {
                if !buffer_released {
                    // Common case post-commit-3 — see wallpaper plugin
                    // for the explanation. Silent deferral is correct.
                    pending_frame = true;
                    continue;
                }
                render_and_send(&dma, &gbm_egl, &mut conn, &buf_msg, fast_path)?;
                buffer_released = false;
            }
            ServerMessage::BufferReleased(_) => {
                buffer_released = true;
                if pending_frame {
                    pending_frame = false;
                    render_and_send(&dma, &gbm_egl, &mut conn, &buf_msg, fast_path)?;
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
    fast_path: bool,
) -> Result<(), PluginError> {
    dma.bind_for_rendering()?;
    unsafe {
        gl::Viewport(0, 0, BUFFER_WIDTH as i32, BUFFER_HEIGHT as i32);
        gl::Clear(gl::COLOR_BUFFER_BIT);
        gl::DrawArrays(gl::TRIANGLES, 0, 6);
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
