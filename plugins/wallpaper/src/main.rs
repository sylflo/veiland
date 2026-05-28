// SPDX-License-Identifier: GPL-3.0-or-later

//! M11 reference plugin — displays a single fixed image as a
//! full-surface background.
//!
//! Reads `path = "..."` from `VEILAND_PLUGIN_CONFIG`, kicks off a
//! worker thread that decodes the image via the `image` crate, and
//! in parallel proceeds through the host handshake and first-frame
//! path. Early frames render solid black; once the worker finishes,
//! the main thread uploads the pixels to a GL texture and subsequent
//! frames draw a textured full-buffer quad.
//!
//! On any failure to load the configured image (missing path, decode
//! error, unsupported format) the plugin logs and falls back to
//! clearing the buffer to solid black. A bad wallpaper path must
//! never take down the locker (lockscreen-grade error handling per
//! CLAUDE.md).

use serde::Deserialize;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use veiland_plugin::{Connection, DmaBuffer, GbmEgl, PluginError, SyncFence};
use veiland_protocol::{Buffer, ServerMessage};

const PLUGIN_NAME: &str = "wallpaper";

#[derive(Debug, Clone, Deserialize)]
struct Config {
    #[serde(default)]
    path: String,
}

fn load_config() -> Config {
    match std::env::var("VEILAND_PLUGIN_CONFIG") {
        Ok(s) => match serde_json::from_str::<Config>(&s) {
            Ok(c) => c,
            Err(e) => {
                eprintln!(
                    "veiland-{}: failed to parse VEILAND_PLUGIN_CONFIG as JSON: {} \
                     — falling back to black background",
                    PLUGIN_NAME, e
                );
                Config {
                    path: String::new(),
                }
            }
        },
        Err(_) => {
            eprintln!(
                "veiland-{}: VEILAND_PLUGIN_CONFIG unset — black background",
                PLUGIN_NAME
            );
            Config {
                path: String::new(),
            }
        }
    }
}

/// CPU-side decoded image. Held only between `decode_image` and the
/// `glTexImage2D` upload — the pixel data lives on the GPU after
/// that, and this buffer is dropped.
struct DecodedImage {
    width: u32,
    height: u32,
    rgba: Vec<u8>,
}

fn decode_image(path: &str) -> Option<DecodedImage> {
    if path.is_empty() {
        return None;
    }
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("veiland-{}: failed to read {:?}: {}", PLUGIN_NAME, path, e);
            return None;
        }
    };

    // Sniff by magic bytes, not extension — handles mislabelled files
    // and avoids handing a PNG to libjpeg-turbo (which would just
    // error). JPEG: FF D8 FF. PNG: 89 50 4E 47 0D 0A 1A 0A.
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        decode_jpeg(path, &bytes)
    } else if bytes.starts_with(&[0x89, 0x50, 0x4E, 0x47]) {
        decode_png(path, &bytes)
    } else {
        eprintln!(
            "veiland-{}: {:?} is neither JPEG nor PNG (first bytes {:02X?}); \
             black background",
            PLUGIN_NAME,
            path,
            &bytes[..bytes.len().min(8)]
        );
        None
    }
}

fn decode_jpeg(path: &str, bytes: &[u8]) -> Option<DecodedImage> {
    let img = match turbojpeg::decompress(bytes, turbojpeg::PixelFormat::RGBA) {
        Ok(i) => i,
        Err(e) => {
            eprintln!(
                "veiland-{}: turbojpeg failed to decode {:?}: {}",
                PLUGIN_NAME, path, e
            );
            return None;
        }
    };

    // turbojpeg allows pitch > width*4 (row padding). Our GL upload
    // assumes tightly-packed RGBA, so reject the padded case rather
    // than copy row-by-row. Doesn't happen for typical wallpaper
    // sizes; if it ever bites we'll add the repack.
    let expected_pitch = img.width.checked_mul(4).unwrap_or(0);
    if img.pitch != expected_pitch {
        eprintln!(
            "veiland-{}: turbojpeg returned pitch={} for width={} (expected {}); \
             black background",
            PLUGIN_NAME, img.pitch, img.width, expected_pitch
        );
        return None;
    }

    eprintln!(
        "veiland-{}: decoded {:?} as {}x{} RGBA (turbojpeg)",
        PLUGIN_NAME, path, img.width, img.height
    );
    Some(DecodedImage {
        width: img.width as u32,
        height: img.height as u32,
        rgba: img.pixels,
    })
}

fn decode_png(path: &str, bytes: &[u8]) -> Option<DecodedImage> {
    let img = match image::load_from_memory_with_format(bytes, image::ImageFormat::Png) {
        Ok(i) => i,
        Err(e) => {
            eprintln!(
                "veiland-{}: image crate failed to decode PNG {:?}: {}",
                PLUGIN_NAME, path, e
            );
            return None;
        }
    };
    let rgba = img.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    eprintln!(
        "veiland-{}: decoded {:?} as {}x{} RGBA (image crate, PNG)",
        PLUGIN_NAME, path, width, height
    );
    Some(DecodedImage {
        width,
        height,
        rgba: rgba.into_raw(),
    })
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

/// GPU state held across frames. `tex` is `None` when no image is
/// loaded — render() then just clears to black.
struct GpuState {
    program: gl::types::GLuint,
    u_tex_loc: gl::types::GLint,
    tex: Option<gl::types::GLuint>,
}

/// Build the textured-quad program and upload the VBO. Must be called
/// with a current EGL context (i.e. after `dma.bind_for_rendering()`).
/// The texture starts unset; `upload_texture` fills it in when the
/// decode worker finishes.
unsafe fn build_gpu_state() -> GpuState {
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
        uniform sampler2D u_tex;\n\
        void main() {\n\
            gl_FragColor = texture2D(u_tex, v_uv);\n\
        }\n\0";

    unsafe {
        let vs = compile_shader(gl::VERTEX_SHADER, vs_src);
        let fs = compile_shader(gl::FRAGMENT_SHADER, fs_src);
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

        let u_tex_loc = gl::GetUniformLocation(program, b"u_tex\0".as_ptr() as *const _);

        GpuState {
            program,
            u_tex_loc,
            tex: None,
        }
    }
}

/// Upload a decoded image to a fresh GL texture. Must be called with a
/// current EGL context — call sites are inside the render loop, after
/// `dma.bind_for_rendering()`.
unsafe fn upload_texture(img: &DecodedImage) -> gl::types::GLuint {
    unsafe {
        let mut tex: gl::types::GLuint = 0;
        gl::GenTextures(1, &mut tex);
        gl::BindTexture(gl::TEXTURE_2D, tex);
        // Linear filtering — fit-to-buffer stretch is acceptable
        // for M11 v1; cover/contain modes are M12+.
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
        // Default GL_UNPACK_ALIGNMENT is 4, which matches RGBA8
        // (4 bytes per pixel) — no need to override.
        gl::TexImage2D(
            gl::TEXTURE_2D,
            0,
            gl::RGBA as i32,
            img.width as i32,
            img.height as i32,
            0,
            gl::RGBA,
            gl::UNSIGNED_BYTE,
            img.rgba.as_ptr() as *const _,
        );
        tex
    }
}

fn run() -> Result<(), PluginError> {
    eprintln!(
        "veiland-{} (pid {}) starting",
        PLUGIN_NAME,
        std::process::id()
    );

    let config = load_config();
    eprintln!("veiland-{}: config path={:?}", PLUGIN_NAME, config.path);

    // Decode runs on a worker thread so the connection handshake and
    // first-frame path don't block on it. A 4K JPEG can take ~5s on
    // the user's box; rendering black during that window beats
    // stalling the lock surface on the core's clear colour.
    let (decode_tx, decode_rx) = mpsc::channel::<Option<DecodedImage>>();
    let decode_path = config.path.clone();
    std::thread::spawn(move || {
        let decoded = decode_image(&decode_path);
        // Receiver may already be gone if the plugin shut down early.
        // Either way there's nothing useful to do with the result.
        let _ = decode_tx.send(decoded);
    });

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
    let mut gpu = unsafe { build_gpu_state() };
    let mut decode_rx: Option<Receiver<Option<DecodedImage>>> = Some(decode_rx);

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
                if c.region_w != dma.width() || c.region_h != dma.height() {
                    eprintln!(
                        "veiland-{}: configure region {}x{} differs from initial {}x{}; \
                         keeping initial buffer size, wallpaper may stretch",
                        PLUGIN_NAME,
                        c.region_w,
                        c.region_h,
                        dma.width(),
                        dma.height(),
                    );
                }
            }
            ServerMessage::FrameDone => {
                if !buffer_released {
                    // Common case post-commit-3: host paints on the next
                    // frame callback rather than immediately on Buffer,
                    // so FrameDone often arrives before BufferReleased.
                    // Silent deferral is correct.
                    pending_frame = true;
                    continue;
                }
                render_and_send(
                    &dma,
                    &gbm_egl,
                    &mut conn,
                    &buf_msg,
                    &mut gpu,
                    &mut decode_rx,
                    fast_path,
                )?;
                buffer_released = false;
            }
            ServerMessage::BufferReleased(_) => {
                buffer_released = true;
                if pending_frame {
                    pending_frame = false;
                    render_and_send(
                        &dma,
                        &gbm_egl,
                        &mut conn,
                        &buf_msg,
                        &mut gpu,
                        &mut decode_rx,
                        fast_path,
                    )?;
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
    gpu: &mut GpuState,
    decode_rx: &mut Option<Receiver<Option<DecodedImage>>>,
    fast_path: bool,
) -> Result<(), PluginError> {
    dma.bind_for_rendering()?;
    let (w, h) = (dma.width(), dma.height());

    // Check the decode worker before drawing — a freshly-arrived
    // texture renders on the same frame. The receiver is taken once
    // the worker has reported (success or failure) so we stop polling.
    if let Some(rx) = decode_rx.as_ref() {
        match rx.try_recv() {
            Ok(Some(img)) => {
                gpu.tex = Some(unsafe { upload_texture(&img) });
                *decode_rx = None;
            }
            Ok(None) => {
                eprintln!(
                    "veiland-{}: decode worker reported failure; staying black",
                    PLUGIN_NAME
                );
                *decode_rx = None;
            }
            Err(TryRecvError::Disconnected) => {
                eprintln!(
                    "veiland-{}: decode worker disconnected (likely panicked); \
                     staying black",
                    PLUGIN_NAME
                );
                *decode_rx = None;
            }
            Err(TryRecvError::Empty) => {}
        }
    }

    unsafe {
        gl::Viewport(0, 0, w as i32, h as i32);
        gl::ClearColor(0.0, 0.0, 0.0, 1.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);

        if let Some(tex) = gpu.tex {
            gl::UseProgram(gpu.program);
            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, tex);
            gl::Uniform1i(gpu.u_tex_loc, 0);
            gl::DrawArrays(gl::TRIANGLES, 0, 6);
        }
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
