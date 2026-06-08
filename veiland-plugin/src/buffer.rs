// SPDX-License-Identifier: GPL-3.0-or-later

//! `DmaBuffer`: one dmabuf-backed GBM bo wrapped as a GL FBO that the plugin
//! renders into. The buffer's pixels live in GPU memory; the plugin renders
//! into the FBO, then hands the dmabuf fd to the host via
//! `Connection::send_buffer`. No CPU-side pixel copy is involved.
//!
//! Named `DmaBuffer` rather than `Buffer` to avoid collision with
//! `veiland_protocol::Buffer`, which is the wire-format message that
//! describes a `DmaBuffer` when handing it to the host.
//!
//! v1 is single-buffer: the plugin creates one `DmaBuffer` at startup and
//! reuses it for every frame. M5 will replace this with a 2–3 buffer pool
//! plus `BufferReleased` driven recycling.
//!
//! The format is ARGB8888 with LINEAR modifier — the v1 protocol allowlist
//! (see `docs/protocol.md` §6.2). The M2 plugin used XRGB8888; we switched
//! to ARGB to match the protocol crate's `Buffer` decoder.

use std::ffi::c_void;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use khronos_egl as egl;
use veiland_protocol::{Fourcc, Modifier};

use crate::error::PluginError;
use crate::render::GbmEgl;

/// One dmabuf-backed render target.
///
/// Owns the GBM buffer object, the dmabuf fd exported from it, the
/// EGLImage that wraps the dmabuf for the GL side, the GL texture bound
/// to the image, and the framebuffer object that targets the texture.
/// Drop order is significant — GL resources release before EGL/GBM does.
pub struct DmaBuffer {
    // Held for its Drop: the GBM allocation must outlive the dmabuf fd
    // we exported from it. Never read after construction.
    #[allow(dead_code)]
    bo: gbm::BufferObject<()>,
    bo_fd: OwnedFd,
    egl_image: egl::Image,
    texture: gl::types::GLuint,
    framebuffer: gl::types::GLuint,
    width: u32,
    height: u32,
    stride: u32,
    modifier: Modifier,
    // Borrowed from GbmEgl on construction. Same EGL instance is used to
    // destroy the EGLImage on drop, so we keep a handle.
    egl: egl::Instance<egl::Static>,
    egl_display: egl::Display,
}

impl DmaBuffer {
    /// Allocate a `width × height` ARGB8888 dmabuf-backed buffer, import it
    /// as a GL framebuffer the plugin can render into.
    ///
    /// The caller's `GbmEgl` must have an EGL context current — `GbmEgl::new`
    /// does this — because `eglCreateImage` and the GL setup all assume it.
    pub fn new(gbm_egl: &GbmEgl, width: u32, height: u32) -> Result<Self, PluginError> {
        // 1. Allocate the GBM buffer object. RENDERING flag = "we will
        //    render into this." ARGB8888 to match the protocol allowlist.
        let bo = gbm_egl
            .gbm()
            .create_buffer_object::<()>(
                width,
                height,
                gbm::Format::Argb8888,
                gbm::BufferObjectFlags::RENDERING,
            )
            .map_err(|_| PluginError::Render("gbm create_buffer_object failed"))?;

        let modifier = u64::from(bo.modifier());
        let stride = bo.stride();

        // 2. Export the dmabuf fd. The fd is owned here; the plugin re-sends
        //    the same fd across frames via `Connection::send_buffer`.
        let bo_fd: OwnedFd = bo
            .fd()
            .map_err(|_| PluginError::Render("export dmabuf fd failed"))?;

        // 3. Import the dmabuf as an EGLImage. Constants from
        //    EGL_EXT_image_dma_buf_import.
        const EGL_LINUX_DMA_BUF_EXT: egl::Int = 0x3270;
        const EGL_LINUX_DRM_FOURCC_EXT: egl::Int = 0x3271;
        const EGL_DMA_BUF_PLANE0_FD_EXT: egl::Int = 0x3272;
        const EGL_DMA_BUF_PLANE0_OFFSET_EXT: egl::Int = 0x3273;
        const EGL_DMA_BUF_PLANE0_PITCH_EXT: egl::Int = 0x3274;
        const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: egl::Int = 0x3443;
        const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: egl::Int = 0x3444;

        let (egl, egl_display) = gbm_egl.egl();
        let raw_fd = std::os::fd::AsRawFd::as_raw_fd(&bo_fd);
        let image_attribs: [egl::Attrib; 17] = [
            egl::WIDTH as egl::Attrib,
            width as egl::Attrib,
            egl::HEIGHT as egl::Attrib,
            height as egl::Attrib,
            EGL_LINUX_DRM_FOURCC_EXT as egl::Attrib,
            gbm::Format::Argb8888 as u32 as egl::Attrib,
            EGL_DMA_BUF_PLANE0_FD_EXT as egl::Attrib,
            raw_fd as egl::Attrib,
            EGL_DMA_BUF_PLANE0_OFFSET_EXT as egl::Attrib,
            0,
            EGL_DMA_BUF_PLANE0_PITCH_EXT as egl::Attrib,
            stride as egl::Attrib,
            EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT as egl::Attrib,
            (modifier & 0xFFFF_FFFF) as egl::Attrib,
            EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT as egl::Attrib,
            (modifier >> 32) as egl::Attrib,
            egl::ATTRIB_NONE,
        ];

        // SAFETY: EGL_NO_CONTEXT and EGL_NO_CLIENT_BUFFER are the documented
        // sentinels for dmabuf import (see EGL_EXT_image_dma_buf_import). The
        // attrib array is statically sized and properly terminated with
        // ATTRIB_NONE. `raw_fd` is live for the duration of this call because
        // `bo_fd` is the OwnedFd we just constructed; EGL dup's the fd
        // internally, so its lifetime after this call is EGL's problem.
        let egl_image = egl
            .create_image(
                egl_display,
                unsafe { egl::Context::from_ptr(std::ptr::null_mut()) },
                EGL_LINUX_DMA_BUF_EXT as std::ffi::c_uint,
                unsafe { egl::ClientBuffer::from_ptr(std::ptr::null_mut()) },
                &image_attribs,
            )
            .map_err(|_| PluginError::Render("eglCreateImage(dmabuf) failed"))?;

        // 4. Create a GL texture, bind the EGLImage to it, then build an FBO
        //    that targets the texture. After this the plugin's draw calls
        //    can land in the dmabuf via the FBO.
        let mut texture: gl::types::GLuint = 0;
        let mut framebuffer: gl::types::GLuint = 0;

        // SAFETY: every gl::* call below requires a current GL context. The
        // caller's `GbmEgl::new` made one current; we documented that
        // precondition. The handles `texture` and `framebuffer` are
        // out-params filled by gl::GenTextures / gl::GenFramebuffers.
        unsafe {
            gl::GenTextures(1, &mut texture);
            gl::BindTexture(gl::TEXTURE_2D, texture);

            // glEGLImageTargetTexture2DOES is an extension; resolve at runtime.
            let target_fn: extern "system" fn(gl::types::GLenum, *const c_void) =
                std::mem::transmute(egl.get_proc_address("glEGLImageTargetTexture2DOES").ok_or(
                    PluginError::Render("glEGLImageTargetTexture2DOES not available"),
                )?);
            target_fn(gl::TEXTURE_2D, egl_image.as_ptr() as *const _);

            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);

            gl::GenFramebuffers(1, &mut framebuffer);
            gl::BindFramebuffer(gl::FRAMEBUFFER, framebuffer);
            gl::FramebufferTexture2D(
                gl::FRAMEBUFFER,
                gl::COLOR_ATTACHMENT0,
                gl::TEXTURE_2D,
                texture,
                0,
            );

            let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER);
            if status != gl::FRAMEBUFFER_COMPLETE {
                return Err(PluginError::Render("FBO incomplete after attach"));
            }
        }

        Ok(Self {
            bo,
            bo_fd,
            egl_image,
            texture,
            framebuffer,
            width,
            height,
            stride,
            modifier: Modifier(modifier),
            egl: egl::Instance::new(egl::Static),
            egl_display,
        })
    }

    /// Reallocate this buffer to `width × height` if it differs from the
    /// current size, replacing the backing GBM bo / EGLImage / FBO in place
    /// and leaving the **new** buffer bound for rendering. Returns `true` if a
    /// reallocation happened (the caller must then rebuild any cached
    /// `veiland_protocol::Buffer` wire message, since fd/stride/modifier move
    /// with the bo), `false` if the size was unchanged and nothing was done.
    ///
    /// This is the host-resize response every plugin needs: on a cold lock the
    /// host spawns plugins at a 1080p fallback, then resends `Configure` with
    /// the output's true size (e.g. 4K). A plugin that keeps its first buffer
    /// has its output stretched by the compositor — soft for images, distorted
    /// for text. Calling this from the `Frame::Reconfigure` arm keeps the
    /// buffer at native resolution so the host's composite is 1:1.
    ///
    /// **Errors are returned, not fatal by policy** — but callers should treat
    /// a failed realloc as non-fatal (keep the old buffer, log, carry on)
    /// rather than `?`-propagating out of the render loop: a transient GBM
    /// allocation failure must not take down the locker. The old buffer is
    /// only dropped once the new one is fully constructed, so on error `self`
    /// is left untouched and still valid.
    ///
    /// Safe to call from `Frame::Reconfigure`: `FramePacer` only surfaces a
    /// reconfigure between frames, after the host has released the in-flight
    /// buffer and imported it into its own EGLImage (the host holds no
    /// reference to our fd past import), so dropping the old bo here cannot
    /// free a buffer the host is still sampling.
    pub fn resize_to(
        &mut self,
        gbm_egl: &GbmEgl,
        width: u32,
        height: u32,
    ) -> Result<bool, PluginError> {
        if width == self.width && height == self.height {
            return Ok(false);
        }
        // Construct the replacement first; only on success do we drop the old
        // one (via the move-assign below), so a failure leaves `self` intact.
        let new = DmaBuffer::new(gbm_egl, width, height)?;
        *self = new;
        self.bind_for_rendering()?;
        Ok(true)
    }

    /// Bind the buffer as the active GL framebuffer and set the viewport.
    /// Plugin's draw calls after this land in the dmabuf.
    pub fn bind_for_rendering(&self) -> Result<(), PluginError> {
        // SAFETY: gl::BindFramebuffer and gl::Viewport require a current GL
        // context. The plugin obtains that via `GbmEgl::new` / `make_current`.
        // `self.framebuffer` is a handle this struct allocated and still owns.
        unsafe {
            gl::BindFramebuffer(gl::FRAMEBUFFER, self.framebuffer);
            gl::Viewport(0, 0, self.width as i32, self.height as i32);
        }
        Ok(())
    }

    /// Block until all GL work issued to this buffer has completed. The v1
    /// sync model — replaced by explicit sync-fence fds in M5. Plugin MUST
    /// call this after rendering and before `Connection::send_buffer`,
    /// otherwise the host may sample a half-rendered frame.
    pub fn finish(&self) {
        // SAFETY: requires current GL context (see other methods).
        unsafe {
            gl::Finish();
        }
    }

    /// The dmabuf fd, borrowed. Pass to `Connection::send_buffer`. The fd
    /// is owned by this `DmaBuffer`; the plugin keeps reusing the same fd
    /// across frames in v1.
    pub fn dmabuf_fd(&self) -> BorrowedFd<'_> {
        self.bo_fd.as_fd()
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn stride(&self) -> u32 {
        self.stride
    }

    pub fn modifier(&self) -> Modifier {
        self.modifier
    }

    /// v1 always reports ARGB8888 — the only format on the allowlist.
    /// Method on `DmaBuffer` for symmetry with the other getters; this
    /// is the format the plugin should put in `veiland_protocol::Buffer`.
    pub fn format(&self) -> Fourcc {
        Fourcc::ARGB8888
    }
}

impl Drop for DmaBuffer {
    fn drop(&mut self) {
        // SAFETY: gl::DeleteTextures / gl::DeleteFramebuffers require a
        // current GL context. The plugin's process is shutting down or
        // recycling this buffer; either way the context is still current
        // because GbmEgl outlives DmaBuffer in any sensible usage pattern.
        unsafe {
            gl::DeleteFramebuffers(1, &self.framebuffer);
            gl::DeleteTextures(1, &self.texture);
        }
        // EGLImage destruction. `destroy_image` returns a Result; we
        // can't propagate from Drop, so we ignore the result — the
        // process is going away anyway.
        let _ = self.egl.destroy_image(self.egl_display, self.egl_image);
        // bo and bo_fd drop in field order (after this Drop runs), which
        // closes the dmabuf fd and frees the GBM bo. Correct as-is.
    }
}
