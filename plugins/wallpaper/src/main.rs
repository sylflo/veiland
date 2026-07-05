// SPDX-License-Identifier: GPL-3.0-or-later

//! Reference plugin — displays a single fixed image as a
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
use veiland_plugin::{Connection, DmaBuffer, Frame, FramePacer, GbmEgl, PluginError, gl as vgl};

const PLUGIN_NAME: &str = "wallpaper";

#[derive(Debug, Clone, Default, Deserialize)]
struct Config {
    #[serde(default)]
    path: String,
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
unsafe fn build_gpu_state() -> Result<GpuState, String> {
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

        let u_tex_loc = gl::GetUniformLocation(program, c"u_tex".as_ptr());

        Ok(GpuState {
            program,
            u_tex_loc,
            tex: None,
        })
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

    let config = veiland_plugin::load_config::<Config>(PLUGIN_NAME);
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

    // Connect preamble (from_env + handshake + hello) in one call.
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

    let dma = DmaBuffer::new(&gbm_egl, first_configure.region_w, first_configure.region_h)?;
    eprintln!(
        "allocated {}x{} {:?}, modifier=0x{:016x}, stride={}",
        dma.width(),
        dma.height(),
        dma.format(),
        u64::from(dma.modifier()),
        dma.stride(),
    );

    let mut dma = dma;
    dma.bind_for_rendering()?;
    let mut gpu = unsafe { build_gpu_state() }.map_err(|e| {
        eprintln!("veiland-{PLUGIN_NAME}: {e}");
        PluginError::Render("shader build failed")
    })?;
    let mut decode_rx: Option<Receiver<Option<DecodedImage>>> = Some(decode_rx);

    // On-demand: the wallpaper redraws only when the host asks (and once
    // more when the worker thread's decode lands, via FrameDone). FramePacer
    // owns the deferral state machine.
    let mut pacer = FramePacer::on_demand();
    loop {
        match pacer.next(&mut conn)? {
            Frame::Render => {
                render_and_send(&dma, &mut conn, &gbm_egl, &mut gpu, &mut decode_rx)?;
                pacer.submitted();
            }
            Frame::Reconfigure(c) => {
                dma.resize_or_keep(&gbm_egl, c.region_w, c.region_h, PLUGIN_NAME);
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
    gpu: &mut GpuState,
    decode_rx: &mut Option<Receiver<Option<DecodedImage>>>,
) -> Result<(), PluginError> {
    dma.bind_for_rendering()?;

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

    conn.submit_frame(dma, gbm_egl)
}

fn main() {
    if let Err(e) = run() {
        eprintln!("{}: {}", env!("CARGO_PKG_NAME"), e);
        std::process::exit(1);
    }
}
