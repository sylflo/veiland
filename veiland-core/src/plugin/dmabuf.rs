// SPDX-License-Identifier: GPL-3.0-or-later

//! Dmabuf → EGLImage → GL texture import. The host's side of the
//! cross-process buffer share: a fd received via `SCM_RIGHTS` becomes
//! something the lock-surface compositor can sample.
//!
//! `eglCreateImage` doubles as our modifier-validation gate (see
//! `docs/protocol.md` §11). If the GL stack refuses the dmabuf — bad
//! modifier, unsupported format, mismatched stride — the call fails
//! and we propagate `ProtocolViolation`. The caller treats the plugin
//! as dead per the threat-model rule "never `.expect()` on plugin
//! input."

use std::os::fd::{AsRawFd, OwnedFd};

use khronos_egl as egl;

use veiland_protocol::Buffer;

use super::HostError;

/// One imported dmabuf, ready to be sampled.
///
/// Both handles are tied to the same GPU resource and are released
/// together. `Drop` is *not* implemented — the GL/EGL teardown
/// requires an active EGL context which this struct does not own.
/// Callers must drop via an explicit `release(&self, egl, display)`
/// before the struct goes out of scope. (M5+ may revisit this when
/// the buffer pool lands.)
pub struct GlTexture {
    pub image: egl::Image,
    pub name: gl::types::GLuint,
    /// Which GL texture target the EGLImage actually bound to. Almost
    /// always `GL_TEXTURE_2D`; `GL_TEXTURE_EXTERNAL_OES` for dmabufs the
    /// driver only accepts as external (NVIDIA does this for LINEAR /
    /// CPU-written buffers, so every CPU plugin lands here on that stack).
    /// The compositor picks the matching sampler program from this, and
    /// `PluginState::composite` binds this target rather than assuming 2D.
    pub target: gl::types::GLenum,
}

// GL_OES_EGL_image_external texture target. The `gl` 0.14 crate omits it
// (it's a GLES extension enum), so we hand-roll it -- same idiom as the
// EGL_* consts below. Public so the repaint loop can recognise an
// external-bound texture and pick the matching compositor program.
pub const GL_TEXTURE_EXTERNAL_OES: gl::types::GLenum = 0x8D65;

// EGL_EXT_image_dma_buf_import constants. Values from the extension
// spec; not re-exported by khronos_egl because it's an extension.
const EGL_LINUX_DMA_BUF_EXT: egl::Int = 0x3270;
const EGL_LINUX_DRM_FOURCC_EXT: egl::Int = 0x3271;
const EGL_DMA_BUF_PLANE0_FD_EXT: egl::Int = 0x3272;
const EGL_DMA_BUF_PLANE0_OFFSET_EXT: egl::Int = 0x3273;
const EGL_DMA_BUF_PLANE0_PITCH_EXT: egl::Int = 0x3274;
const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: egl::Int = 0x3443;
const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: egl::Int = 0x3444;

/// Import a dmabuf as a GL-sampleable texture.
///
/// The caller must have an active EGL context current on the calling
/// thread. The `fd` is borrowed for the duration of the call — EGL
/// dups internally — so dropping the `OwnedFd` after this returns is
/// safe and is what the caller should do.
///
/// On EGL failure (bad modifier / format / stride / unsupported
/// hardware combination), returns `ProtocolViolation`. This is the
/// host-side modifier validation that replaces the codec allowlist;
/// see `docs/protocol.md` §11.
pub fn import_dmabuf(
    egl: &egl::Instance<egl::Static>,
    display: egl::Display,
    buffer: &Buffer,
    fd: &OwnedFd,
) -> Result<GlTexture, HostError> {
    let modifier_lo = (buffer.modifier.0 & 0xFFFF_FFFF) as egl::Attrib;
    let modifier_hi = (buffer.modifier.0 >> 32) as egl::Attrib;

    let image_attribs: [egl::Attrib; 17] = [
        egl::WIDTH as egl::Attrib,
        buffer.width as egl::Attrib,
        egl::HEIGHT as egl::Attrib,
        buffer.height as egl::Attrib,
        EGL_LINUX_DRM_FOURCC_EXT as egl::Attrib,
        buffer.format.0 as egl::Attrib,
        EGL_DMA_BUF_PLANE0_FD_EXT as egl::Attrib,
        fd.as_raw_fd() as egl::Attrib,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT as egl::Attrib,
        buffer.offset as egl::Attrib,
        EGL_DMA_BUF_PLANE0_PITCH_EXT as egl::Attrib,
        buffer.stride as egl::Attrib,
        EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT as egl::Attrib,
        modifier_lo,
        EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT as egl::Attrib,
        modifier_hi,
        egl::ATTRIB_NONE,
    ];

    // EGL_NO_CONTEXT + EGL_NO_CLIENT_BUFFER for dmabuf imports.
    // The dmabuf fd itself is in the attribs.
    let image = egl
        .create_image(
            display,
            unsafe { egl::Context::from_ptr(std::ptr::null_mut()) },
            EGL_LINUX_DMA_BUF_EXT as std::ffi::c_uint,
            unsafe { egl::ClientBuffer::from_ptr(std::ptr::null_mut()) },
            &image_attribs,
        )
        .map_err(|_| {
            HostError::ProtocolViolation(
                "dmabuf import failed; bad modifier or format on this hardware",
            )
        })?;

    // Bind the EGLImage to a fresh GL texture name. The
    // glEGLImageTargetTexture2DOES entry point is loaded via EGL's
    // proc-address lookup (it's not in GLES2 core).
    let target_fn: extern "system" fn(gl::types::GLenum, *const std::ffi::c_void) = unsafe {
        std::mem::transmute(egl.get_proc_address("glEGLImageTargetTexture2DOES").ok_or(
            HostError::Render("glEGLImageTargetTexture2DOES not available"),
        )?)
    };

    // Try GL_TEXTURE_2D first (the common, tiled-modifier path). If the
    // driver refuses -- NVIDIA rejects LINEAR/CPU-written dmabufs as
    // "<image> and <target> are incompatible" (GL_INVALID_OPERATION),
    // marking them external-only -- fall back to GL_TEXTURE_EXTERNAL_OES.
    // On the fallback path the compositor samples via samplerExternalOES.
    //
    // The bind failure is detected with glGetError; note this is
    // ALWAYS-ON control flow, not gated behind VEILAND_GL_DEBUG. The
    // debug env var only governs diagnostics; if this check were gated the
    // fallback would never fire in production and NVIDIA would go black.
    // The check runs once per imported buffer (not per frame), so it's off
    // the hot path and costs nothing to leave unconditional.
    let mut name: gl::types::GLuint = 0;
    let target = unsafe {
        gl::GenTextures(1, &mut name);

        // Drain any pre-existing error so the check below reflects only
        // this bind, not stale state from an earlier GL call.
        while gl::GetError() != gl::NO_ERROR {}

        gl::BindTexture(gl::TEXTURE_2D, name);
        target_fn(gl::TEXTURE_2D, image.as_ptr() as *const _);

        if gl::GetError() == gl::NO_ERROR {
            gl::TEXTURE_2D
        } else {
            // The TEXTURE_2D bind failed (NVIDIA: LINEAR/CPU dmabuf is
            // external-only). Retry against GL_TEXTURE_EXTERNAL_OES, but on
            // a FRESH texture name: NVIDIA commits a texture object's target
            // on its first glBindTexture, so reusing `name` here fails with
            // "Target doesn't match the texture's target." Delete the
            // 2D-committed name and allocate a new one for the external bind.
            gl::DeleteTextures(1, &name);
            let mut ext_name: gl::types::GLuint = 0;
            gl::GenTextures(1, &mut ext_name);
            while gl::GetError() != gl::NO_ERROR {}

            gl::BindTexture(GL_TEXTURE_EXTERNAL_OES, ext_name);
            target_fn(GL_TEXTURE_EXTERNAL_OES, image.as_ptr() as *const _);
            if gl::GetError() != gl::NO_ERROR {
                // Both targets refused the image. Clean up and report a
                // protocol violation; the caller treats the plugin as dead
                // and draws the fallback -- never a crash (dmabuf care bar).
                gl::DeleteTextures(1, &ext_name);
                let _ = egl.destroy_image(display, image);
                return Err(HostError::ProtocolViolation(
                    "dmabuf bound to neither GL_TEXTURE_2D nor GL_TEXTURE_EXTERNAL_OES",
                ));
            }
            name = ext_name;
            GL_TEXTURE_EXTERNAL_OES
        }
    };

    // Sampler params on whichever target actually bound. (External
    // textures require CLAMP_TO_EDGE wrap and non-mip filters, which is
    // exactly what we set.)
    unsafe {
        gl::TexParameteri(target, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(target, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(target, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
        gl::TexParameteri(target, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
    }
    crate::gl_debug::check_gl("import_dmabuf: EGLImage bind + texture setup");

    Ok(GlTexture {
        image,
        name,
        target,
    })
}

/// Release a `GlTexture`'s GPU resources. Must be called on a thread
/// where the EGL context is current. Caller's responsibility — `Drop`
/// can't do this without owning the EGL bits.
pub fn release_texture(egl: &egl::Instance<egl::Static>, display: egl::Display, tex: GlTexture) {
    unsafe {
        gl::DeleteTextures(1, &tex.name);
    }
    // Best-effort: if the destroy fails (e.g. display gone), log and
    // move on. Nothing we can do about leaking at process-exit time.
    let _ = egl.destroy_image(display, tex.image);
}
