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
    /// Blur strength. 0 = off (default), and 0 is a HARD no-op: the
    /// FBO/ping-pong path is only taken when blur > 0, so a plain
    /// wallpaper pays nothing.
    #[serde(default)]
    blur: f32,
    /// Optional dim applied after blur (frosted-glass look), 0..1.
    /// Multiplies RGB in the final pass. 0 = no dimming (default)
    #[serde(default)]
    darken: f32,
    /// Optional "treated zone": inside this rect the wallpaper is
    /// blurred AND dimmed, outside it stays sharp and full-brightness.
    /// Absent (default) = blur+darken apply to the whole surface. Only
    /// meaningful when blur > 0; with blur = 0 it is ignored.
    #[serde(default)]
    blur_region: Option<BlurRegion>,
}

/// A fraction-of-surface rectangle. `x`/`y` are the top-left corner and
/// `w`/`h` the size, all in [0, 1]; `y` is measured from the TOP (the
/// shader flips to GL's bottom-left UV origin). Resolution-independent,
/// matching veiland's other fraction-of-surface regions.
#[derive(Debug, Clone, Copy, Deserialize)]
struct BlurRegion {
    #[serde(default)]
    x: f32,
    #[serde(default)]
    y: f32,
    #[serde(default)]
    w: f32,
    #[serde(default)]
    h: f32,
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
    // and avoids handing a PNG to the JPEG decoder (which would just
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
    let img = match image::load_from_memory_with_format(bytes, image::ImageFormat::Jpeg) {
        Ok(i) => i,
        Err(e) => {
            eprintln!(
                "veiland-{}: image crate failed to decode JPEG {:?}: {}",
                PLUGIN_NAME, path, e
            );
            return None;
        }
    };
    let rgba = img.to_rgba8();
    let (width, height) = (rgba.width(), rgba.height());
    eprintln!(
        "veiland-{}: decoded {:?} as {}x{} RGBA (image crate, JPEG)",
        PLUGIN_NAME, path, width, height
    );
    Some(DecodedImage {
        width,
        height,
        rgba: rgba.into_raw(),
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

/// Offscreen render target: the wallpaper is drawn into `tex` (via
/// `fb`), then `tex` is sampled to produce the dmabuf. Only built when
/// blur > 0; `dims` is the size it was allocated at, so a Reconfigure
/// can detect a size change and rebuild.
struct Fbo {
    fb: gl::types::GLuint,
    tex: gl::types::GLuint,
    dims: (u32, u32),
}

/// GPU state held across frames. `tex` is `None` when no image is
/// loaded — render() then just clears to black. See `fbos` for the
/// blur-off / blur-failed fallback.
struct GpuState {
    program: gl::types::GLuint,
    u_tex_loc: gl::types::GLint,
    // Sharp-wallpaper sampler for the copy pass (outside the region).
    u_sharp_loc: gl::types::GLint,
    // Post-blur dim applied in the copy pass; 0..1, 0 = unchanged.
    u_darken_loc: gl::types::GLint,
    // Treated-zone rect (x0,y0,x1,y1 in UV); (0,0,1,1) = whole surface.
    u_region_loc: gl::types::GLint,
    // Separable Gaussian blur; direction chosen per pass by u_dir.
    blur_program: gl::types::GLuint,
    u_blur_tex_loc: gl::types::GLint,
    u_dir_loc: gl::types::GLint,
    tex: Option<gl::types::GLuint>,
    // Ping-pong pair (both-or-neither). None when blur is off or the
    // FBOs failed to build, in which case the wallpaper renders straight
    // to the dmabuf as before.
    fbos: Option<[Fbo; 2]>,
    // Clamped blur pass count driving the ping-pong loop. `> 0` exactly
    // when `fbos` is `Some` (both keyed off the same resolved value).
    passes: u32,
    // Clamped 0..1 dim, applied in the copy pass independently of blur.
    darken: f32,
    // Treated-zone rect in UV (x0,y0,x1,y1), y already flipped from the
    // config's top-down convention. (0,0,1,1) when no blur_region is set.
    region_uv: [f32; 4],
}

/// Build an offscreen framebuffer + colour texture at `w`x`h`. Returns
/// Err (rather than panicking) if the framebuffer is incomplete, so the
/// caller can fall back to the direct no-blur path. Detaches its FBO on
/// the way out (the caller rebinds the dmabuf before drawing). Requires
/// a current EGL context.
unsafe fn build_fbo(w: u32, h: u32) -> Result<Fbo, String> {
    unsafe {
        let mut tex: gl::types::GLuint = 0;
        gl::GenTextures(1, &mut tex);
        gl::BindTexture(gl::TEXTURE_2D, tex);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
        // Null data: we render into this texture, we don't upload to it.
        gl::TexImage2D(
            gl::TEXTURE_2D,
            0,
            gl::RGBA as i32,
            w as i32,
            h as i32,
            0,
            gl::RGBA,
            gl::UNSIGNED_BYTE,
            std::ptr::null(),
        );

        let mut fb: gl::types::GLuint = 0;
        gl::GenFramebuffers(1, &mut fb);
        gl::BindFramebuffer(gl::FRAMEBUFFER, fb);
        gl::FramebufferTexture2D(
            gl::FRAMEBUFFER,
            gl::COLOR_ATTACHMENT0,
            gl::TEXTURE_2D,
            tex,
            0,
        );

        let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER);
        // Detach our FBO; the caller rebinds the dmabuf (an FBO of its own,
        // via dma.bind_for_rendering) before the next draw.
        gl::BindFramebuffer(gl::FRAMEBUFFER, 0);
        if status != gl::FRAMEBUFFER_COMPLETE {
            gl::DeleteFramebuffers(1, &fb);
            gl::DeleteTextures(1, &tex);
            return Err(format!("FBO incomplete: status 0x{status:04x}"));
        }

        Ok(Fbo {
            fb,
            tex,
            dims: (w, h),
        })
    }
}

/// Build the two ping-pong FBOs at `w`x`h`. Returns None if either
/// fails to build (rolling back the first), so the caller can fall back
/// to the unblurred path. Requires a current EGL context.
unsafe fn build_fbos(w: u32, h: u32) -> Option<[Fbo; 2]> {
    unsafe {
        let a = match build_fbo(w, h) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("veiland-{PLUGIN_NAME}: FBO build failed ({e})");
                return None;
            }
        };
        let b = match build_fbo(w, h) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("veiland-{PLUGIN_NAME}: FBO build failed ({e})");
                gl::DeleteFramebuffers(1, &a.fb);
                gl::DeleteTextures(1, &a.tex);
                return None;
            }
        };
        Some([a, b])
    }
}

/// Build the textured-quad program and upload the VBO. Must be called
/// with a current EGL context (i.e. after `dma.bind_for_rendering()`).
/// The texture starts unset; `upload_texture` fills it in when the
/// decode worker finishes. When `passes > 0`, also builds the two
/// offscreen FBOs (at `w`x`h`) the blur passes ping-pong through; when
/// blur is off, or the FBOs fail to build, `fbos` stays `None` and the
/// wallpaper renders straight to the dmabuf as before.
unsafe fn build_gpu_state(
    passes: u32,
    darken: f32,
    region_uv: [f32; 4],
    w: u32,
    h: u32,
) -> Result<GpuState, String> {
    let vs_src = b"#version 100\n\
        attribute vec2 a_pos;\n\
        varying vec2 v_uv;\n\
        void main() {\n\
            v_uv = a_pos * 0.5 + 0.5;\n\
            gl_Position = vec4(a_pos, 0.0, 1.0);\n\
        }\n\0";

    // Copy/composite pass. u_tex is the blurred image, u_tex_sharp the
    // original wallpaper. u_region (x0,y0,x1,y1 in UV) is the treated
    // zone: inside -> blurred + dimmed, outside -> sharp, full bright.
    // When there is no blur_region, u_region is the whole surface
    // (0,0,1,1) so the mix is blurred+dimmed everywhere, as before.
    // Dim RGB only; alpha stays as sampled (wallpaper is opaque, so
    // premultiplied == straight and rgb<=a holds). u_darken 0 = off.
    let fs_src = b"#version 100\n\
        precision mediump float;\n\
        varying vec2 v_uv;\n\
        uniform sampler2D u_tex;\n\
        uniform sampler2D u_tex_sharp;\n\
        uniform float u_darken;\n\
        uniform vec4 u_region;\n\
        void main() {\n\
            float inside = step(u_region.x, v_uv.x) * step(v_uv.x, u_region.z)\n\
                         * step(u_region.y, v_uv.y) * step(v_uv.y, u_region.w);\n\
            vec4 blurred = texture2D(u_tex, v_uv);\n\
            vec4 sharp = texture2D(u_tex_sharp, v_uv);\n\
            vec4 treated = vec4(blurred.rgb * (1.0 - u_darken), blurred.a);\n\
            gl_FragColor = mix(sharp, treated, inside);\n\
        }\n\0";

    // Separable Gaussian blur. u_dir is the texel step in the blur
    // direction: (1/width, 0) blurs horizontally, (0, 1/height)
    // vertically. 9-tap (sigma ~ 2). A single pass is a mild blur;
    // strength comes from looping horizontal+vertical passes.
    //
    // Each tap is counted only if it lands inside the texture, and the
    // result is divided by the weight actually used. Without this,
    // out-of-bounds taps collapse onto the border texel (clamp-to-edge)
    // and over-weight it; across many ping-pong passes that over-weight
    // compounds and smears the edge inward as a straight streak.
    // highp (not mediump): the per-tap offset u_dir.x = 1/width is finer
    // than mediump can resolve at large x, so mediump quantizes the tap
    // coordinates and the seam between quantization regimes shows up as
    // vertical streaks that worsen toward the right edge. highp resolves
    // the offsets cleanly. Available in GLES2 fragment shaders on desktop
    // Mesa/NVIDIA (veiland's targets).
    let blur_fs_src = b"#version 100\n\
        precision highp float;\n\
        varying vec2 v_uv;\n\
        uniform sampler2D u_tex;\n\
        uniform vec2 u_dir;\n\
        void tap(vec2 uv, float w, inout vec4 c, inout float total) {\n\
            float m = step(0.0, uv.x) * step(uv.x, 1.0)\n\
                    * step(0.0, uv.y) * step(uv.y, 1.0);\n\
            c += texture2D(u_tex, uv) * (w * m);\n\
            total += w * m;\n\
        }\n\
        void main() {\n\
            float w0 = 0.2270270270;\n\
            float w1 = 0.1945945946;\n\
            float w2 = 0.1216216216;\n\
            float w3 = 0.0540540541;\n\
            float w4 = 0.0162162162;\n\
            vec4 c = vec4(0.0);\n\
            float total = 0.0;\n\
            tap(v_uv, w0, c, total);\n\
            tap(v_uv + u_dir * 1.0, w1, c, total);\n\
            tap(v_uv - u_dir * 1.0, w1, c, total);\n\
            tap(v_uv + u_dir * 2.0, w2, c, total);\n\
            tap(v_uv - u_dir * 2.0, w2, c, total);\n\
            tap(v_uv + u_dir * 3.0, w3, c, total);\n\
            tap(v_uv - u_dir * 3.0, w3, c, total);\n\
            tap(v_uv + u_dir * 4.0, w4, c, total);\n\
            tap(v_uv - u_dir * 4.0, w4, c, total);\n\
            gl_FragColor = c / total;\n\
        }\n\0";

    unsafe {
        let vs = vgl::compile_shader(gl::VERTEX_SHADER, vs_src)?;
        let fs = vgl::compile_shader(gl::FRAGMENT_SHADER, fs_src)?;
        let program = vgl::link_program(vs, fs)?;
        gl::UseProgram(program);

        // Blur program reuses the same fullscreen-quad vertex shader;
        // link_program does not consume the shader object.
        let blur_fs = vgl::compile_shader(gl::FRAGMENT_SHADER, blur_fs_src)?;
        let blur_program = vgl::link_program(vs, blur_fs)?;
        let u_blur_tex_loc = gl::GetUniformLocation(blur_program, c"u_tex".as_ptr());
        let u_dir_loc = gl::GetUniformLocation(blur_program, c"u_dir".as_ptr());

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
        let u_sharp_loc = gl::GetUniformLocation(program, c"u_tex_sharp".as_ptr());
        let u_darken_loc = gl::GetUniformLocation(program, c"u_darken".as_ptr());
        let u_region_loc = gl::GetUniformLocation(program, c"u_region".as_ptr());

        let fbos = if passes > 0 {
            let built = build_fbos(w, h);
            if built.is_none() {
                eprintln!("veiland-{PLUGIN_NAME}: falling back to unblurred wallpaper");
            }
            built
        } else {
            None
        };

        Ok(GpuState {
            program,
            u_tex_loc,
            u_sharp_loc,
            u_darken_loc,
            u_region_loc,
            blur_program,
            u_blur_tex_loc,
            u_dir_loc,
            tex: None,
            fbos,
            passes,
            darken,
            region_uv,
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
    eprintln!(
        "veiland-{}: config path={:?}, blur={}, darken={}",
        PLUGIN_NAME, config.path, config.blur, config.darken
    );

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
    // Resolve the pass count once: round + clamp to [0, 20]. This single
    // value gates both FBO construction and the render loop, so they
    // can't disagree. blur < 0.5 -> 0 passes -> the plain direct path.
    let passes = (config.blur.round() as i32).clamp(0, 20) as u32;
    let darken = config.darken.clamp(0.0, 1.0);
    // Resolve blur_region (fractions of surface, x/y from the top-left) to
    // the UV rect (x0,y0,x1,y1) the copy shader tests against. This
    // pipeline's v_uv runs top-down relative to the screen (the FBO
    // round-trip leaves the image upright without a flip, see the copy
    // pass), so config y maps straight to UV y with no inversion. No
    // region -> whole surface, so the copy pass treats everything
    // (blurred+dimmed), matching prior behavior.
    let region_uv = match config.blur_region {
        Some(r) => {
            let x0 = r.x.clamp(0.0, 1.0);
            let x1 = (r.x + r.w).clamp(0.0, 1.0);
            let y0 = r.y.clamp(0.0, 1.0);
            let y1 = (r.y + r.h).clamp(0.0, 1.0);
            [x0, y0, x1, y1]
        }
        None => [0.0, 0.0, 1.0, 1.0],
    };
    let mut gpu = unsafe { build_gpu_state(passes, darken, region_uv, dma.width(), dma.height()) }
        .map_err(|e| {
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
                // The FBOs must match the dmabuf size or the blur samples a
                // stale-sized target. Rebuild only on an actual size change;
                // on failure, fall back to the unblurred direct path. Copy
                // the old handles out first so the &gpu.fbos borrow ends
                // before we reassign gpu.fbos below.
                let stale = gpu
                    .fbos
                    .as_ref()
                    .filter(|fbos| fbos[0].dims != (dma.width(), dma.height()))
                    .map(|fbos| [(fbos[0].fb, fbos[0].tex), (fbos[1].fb, fbos[1].tex)]);
                if let Some(old) = stale {
                    // build_fbos issues GL calls; Reconfigure can arrive
                    // without an intervening render, so bind the context.
                    dma.bind_for_rendering()?;
                    gpu.fbos = unsafe { build_fbos(dma.width(), dma.height()) };
                    if gpu.fbos.is_none() {
                        eprintln!(
                            "veiland-{PLUGIN_NAME}: FBO resize failed; \
                             falling back to unblurred wallpaper"
                        );
                    }
                    unsafe {
                        for (fb, tex) in old {
                            gl::DeleteFramebuffers(1, &fb);
                            gl::DeleteTextures(1, &tex);
                        }
                    }
                }
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
        match (gpu.tex, gpu.fbos.as_ref()) {
            // Blur path present + texture ready. One horizontal + one
            // vertical pass = a full 2D Gaussian, ping-ponging A -> B,
            // then copy B to the dmabuf:
            //   wallpaper --H--> FBO A --V--> FBO B --copy--> dmabuf
            // A single H+V iteration is a mild blur; the N-pass loop that
            // strengthens it lands next.
            (Some(tex), Some(fbos)) => {
                let a = &fbos[0];
                let b = &fbos[1];

                // Blur program stays bound across every blur pass; only
                // u_dir and the source texture / target FBO change. The
                // clear colour is black and never changes, so set it once.
                gl::UseProgram(gpu.blur_program);
                gl::Uniform1i(gpu.u_blur_tex_loc, 0);
                gl::ActiveTexture(gl::TEXTURE0);
                gl::ClearColor(0.0, 0.0, 0.0, 1.0);

                // Ping-pong N iterations. Iteration 1 samples the sharp
                // wallpaper; each later iteration samples the previous
                // result (FBO B). Each iteration is H (src -> A) then
                // V (A -> B), so FBO B always holds the final result.
                // gpu.passes >= 1 whenever fbos is Some.
                let mut src = tex;
                for _ in 0..gpu.passes {
                    // H: src -> A. Texel step along X.
                    gl::Uniform2f(gpu.u_dir_loc, 1.0 / a.dims.0 as f32, 0.0);
                    gl::BindFramebuffer(gl::FRAMEBUFFER, a.fb);
                    gl::Viewport(0, 0, a.dims.0 as i32, a.dims.1 as i32);
                    gl::Clear(gl::COLOR_BUFFER_BIT);
                    gl::BindTexture(gl::TEXTURE_2D, src);
                    gl::DrawArrays(gl::TRIANGLES, 0, 6);

                    // V: A -> B. Texel step along Y.
                    gl::Uniform2f(gpu.u_dir_loc, 0.0, 1.0 / b.dims.1 as f32);
                    gl::BindFramebuffer(gl::FRAMEBUFFER, b.fb);
                    gl::Viewport(0, 0, b.dims.0 as i32, b.dims.1 as i32);
                    gl::Clear(gl::COLOR_BUFFER_BIT);
                    gl::BindTexture(gl::TEXTURE_2D, a.tex);
                    gl::DrawArrays(gl::TRIANGLES, 0, 6);

                    src = b.tex;
                }

                // Composite to dmabuf: blurred (FBO B, unit 0) vs sharp
                // wallpaper (unit 1), picked per fragment by u_region;
                // inside also gets darken. The dmabuf is itself an FBO
                // (buffer.rs), NOT framebuffer 0, so rebind via the SDK;
                // this also restores the viewport the blur passes clobbered.
                gl::UseProgram(gpu.program);
                gl::Uniform1i(gpu.u_tex_loc, 0);
                gl::Uniform1i(gpu.u_sharp_loc, 1);
                gl::Uniform1f(gpu.u_darken_loc, gpu.darken);
                gl::Uniform4f(
                    gpu.u_region_loc,
                    gpu.region_uv[0],
                    gpu.region_uv[1],
                    gpu.region_uv[2],
                    gpu.region_uv[3],
                );
                dma.bind_for_rendering()?;
                gl::Clear(gl::COLOR_BUFFER_BIT);
                gl::ActiveTexture(gl::TEXTURE0);
                gl::BindTexture(gl::TEXTURE_2D, b.tex);
                gl::ActiveTexture(gl::TEXTURE1);
                gl::BindTexture(gl::TEXTURE_2D, tex);
                gl::DrawArrays(gl::TRIANGLES, 0, 6);
                // Leave unit 0 active so the next frame's blur passes
                // (which assume TEXTURE0) aren't surprised by unit 1.
                gl::ActiveTexture(gl::TEXTURE0);
            }
            // No blur (or FBOs absent): sharp wallpaper, global darken.
            // blur_region is meaningless without a blurred image, so force
            // u_region to the whole surface and bind the sharp wallpaper to
            // both samplers -> the mix is sharp+darken everywhere.
            (Some(tex), None) => {
                gl::ClearColor(0.0, 0.0, 0.0, 1.0);
                gl::Clear(gl::COLOR_BUFFER_BIT);
                gl::UseProgram(gpu.program);
                gl::Uniform1i(gpu.u_tex_loc, 0);
                gl::Uniform1i(gpu.u_sharp_loc, 1);
                gl::Uniform1f(gpu.u_darken_loc, gpu.darken);
                gl::Uniform4f(gpu.u_region_loc, 0.0, 0.0, 1.0, 1.0);
                gl::ActiveTexture(gl::TEXTURE0);
                gl::BindTexture(gl::TEXTURE_2D, tex);
                gl::ActiveTexture(gl::TEXTURE1);
                gl::BindTexture(gl::TEXTURE_2D, tex);
                gl::DrawArrays(gl::TRIANGLES, 0, 6);
                gl::ActiveTexture(gl::TEXTURE0);
            }
            // No texture yet: black, exactly as before (both blur and no-blur).
            (None, _) => {
                gl::ClearColor(0.0, 0.0, 0.0, 1.0);
                gl::Clear(gl::COLOR_BUFFER_BIT);
            }
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
