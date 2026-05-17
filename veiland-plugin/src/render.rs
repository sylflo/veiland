// SPDX-License-Identifier: GPL-3.0-or-later

//! `GbmEgl`: per-process render setup. Opens the DRM render node, creates
//! a GBM device, gets an EGL display + context, and makes the context
//! current surfacelessly so plugins can render into FBOs backed by GBM
//! buffer objects.
//!
//! This module contains all the `unsafe` blocks needed to talk to the EGL
//! and GL C APIs. Every `unsafe` block has a `// SAFETY:` comment naming
//! the invariant being upheld. Plugin authors should not need to touch
//! anything in here — the API is `GbmEgl::new()`, and from there the
//! `Buffer` type (next module) hands them an FBO.

use std::ffi::c_void;
use std::os::fd::OwnedFd;

use gbm::AsRaw;
use khronos_egl as egl;

use crate::error::PluginError;

/// Per-process EGL/GBM state. One instance per plugin process.
///
/// Order of fields is significant: Rust drops fields top-down, so `egl`
/// (which owns the libEGL handle) dropping last is what we want. The
/// `gbm::Device` owns the render-node fd and closes it on drop.
pub struct GbmEgl {
    egl: egl::Instance<egl::Static>,
    egl_display: egl::Display,
    egl_context: egl::Context,
    // `gbm` must outlive `egl_display` because the display was created
    // from a pointer into this `gbm::Device`. Kept private so callers
    // can't accidentally drop it early.
    #[allow(dead_code)]
    gbm: gbm::Device<OwnedFd>,
}

impl GbmEgl {
    /// Open the default DRM render node, set up EGL, and make a context
    /// current surfacelessly. After this returns, GL function pointers are
    /// loaded and the plugin can issue draw calls (once a `Buffer` is bound
    /// as the framebuffer).
    pub fn new() -> Result<Self, PluginError> {
        // 1. Open the render node. O_CLOEXEC so child processes don't
        //    inherit; O_RDWR because GBM needs both.
        let render_node = "/dev/dri/renderD128";
        let drm_fd = nix::fcntl::open(
            render_node,
            nix::fcntl::OFlag::O_RDWR | nix::fcntl::OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )
        .map_err(|_| PluginError::Render("open /dev/dri/renderD128 failed"))?;

        // 2. Hand the fd to gbm::Device. From here on the Device owns the
        //    close — we hold the Device in the struct for its lifetime.
        let gbm = gbm::Device::new(drm_fd)
            .map_err(|_| PluginError::Render("gbm::Device::new failed"))?;

        // 3. Get the EGL display from the GBM device pointer.
        let egl = egl::Instance::new(egl::Static);
        // SAFETY: `gbm.as_raw()` returns a pointer into the live gbm::Device
        // we just constructed. The Device is owned by the struct we're
        // about to return, so the pointer remains valid for the entire
        // lifetime of `egl_display`. Field-drop order (egl_display before
        // gbm) preserves this.
        let egl_display = unsafe { egl.get_display(gbm.as_raw() as *mut c_void) }
            .ok_or(PluginError::Render("eglGetDisplay returned NO_DISPLAY"))?;

        // 4. Initialize the display. We don't care about the version pair.
        egl.initialize(egl_display)
            .map_err(|_| PluginError::Render("eglInitialize failed"))?;

        // 5. Bind GLES — we're an OpenGL ES 2 client.
        egl.bind_api(egl::OPENGL_ES_API)
            .map_err(|_| PluginError::Render("eglBindAPI(GLES) failed"))?;

        // 6. Pick a config. No SURFACE_TYPE: we never bind a surface
        //    (eglMakeCurrent gets NO_SURFACE and we render into an FBO),
        //    and on Mesa's GBM platform no config advertises PBUFFER_BIT,
        //    so asking for one returns zero matches on Intel/Mesa.
        //    OPENGL_ES2_BIT picks a config compatible with a GLES2 context.
        //    RGB8, no alpha — alpha is decided when the host samples.
        let config_attribs = [
            egl::RENDERABLE_TYPE,
            egl::OPENGL_ES2_BIT,
            egl::RED_SIZE,
            8,
            egl::GREEN_SIZE,
            8,
            egl::BLUE_SIZE,
            8,
            egl::NONE,
        ];
        let egl_config = egl
            .choose_first_config(egl_display, &config_attribs)
            .map_err(|_| PluginError::Render("eglChooseConfig failed"))?
            .ok_or(PluginError::Render("no matching EGL config"))?;

        // 7. Create a GLES2 context.
        let context_attribs = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];
        let egl_context = egl
            .create_context(egl_display, egl_config, None, &context_attribs)
            .map_err(|_| PluginError::Render("eglCreateContext failed"))?;

        // 8. Make current with no surface. We render into an FBO, not a
        //    surface — `Buffer::bind_for_rendering` (next module) does that.
        egl.make_current(egl_display, None, None, Some(egl_context))
            .map_err(|_| PluginError::Render("eglMakeCurrent failed"))?;

        // 9. Load GL function pointers. One-time per process. After this,
        //    `gl::Foo(...)` calls resolve against the current context.
        gl::load_with(|name| {
            egl.get_proc_address(name)
                .map(|p| p as *const _)
                .unwrap_or(std::ptr::null())
        });

        Ok(Self {
            egl,
            egl_display,
            egl_context,
            gbm,
        })
    }

    /// Re-arm the EGL context as current on the calling thread.
    ///
    /// `new()` already does this; calling `make_current` again is a no-op
    /// on the typical single-threaded plugin. Exposed for plugins that
    /// detach/reattach the context (multi-threaded rendering, future
    /// buffer-pool scenarios). Belt-and-braces in v1.
    pub fn make_current(&self) -> Result<(), PluginError> {
        self.egl
            .make_current(self.egl_display, None, None, Some(self.egl_context))
            .map_err(|_| PluginError::Render("eglMakeCurrent failed"))?;
        Ok(())
    }

    /// Internal accessor: hand the EGL instance + display to `Buffer` so it
    /// can call `eglCreateImage`. Crate-private; not part of the public API.
    pub(crate) fn egl(&self) -> (&egl::Instance<egl::Static>, egl::Display) {
        (&self.egl, self.egl_display)
    }

    /// Internal accessor: hand the GBM device to `Buffer` so it can
    /// allocate buffer objects. Crate-private.
    pub(crate) fn gbm(&self) -> &gbm::Device<OwnedFd> {
        &self.gbm
    }
}
