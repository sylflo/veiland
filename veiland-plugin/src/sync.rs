// SPDX-License-Identifier: GPL-3.0-or-later

//! `SyncFence`: an EGL sync object whose underlying dma-fence has been
//! exported as a file descriptor, suitable for sending across SCM_RIGHTS
//! to the host. The host re-imports the fd to wait on GPU completion of
//! the plugin's draw before sampling the dmabuf.
//!
//! Requires `EGL_ANDROID_native_fence_sync` on the plugin's EGL display.

use std::os::fd::{AsFd, BorrowedFd, FromRawFd, OwnedFd};

use khronos_egl as egl;

use crate::error::PluginError;
use crate::render::GbmEgl;

/// `EGL_SYNC_NATIVE_FENCE_ANDROID` — sync type for fences that can be
/// exported as dma-fence fds. khronos_egl doesn't expose this constant
/// because it's extension-defined.
const EGL_SYNC_NATIVE_FENCE_ANDROID: egl::Enum = 0x3144;

/// Sentinel returned by `eglDupNativeFenceFDANDROID` when no fd is
/// available (driver wouldn't or couldn't materialize one).
const EGL_NO_NATIVE_FENCE_FD_ANDROID: i32 = -1;

/// Function-pointer signature for `eglDupNativeFenceFDANDROID`. The
/// khronos_egl crate doesn't bind ANDROID-suffixed extensions, so we
/// resolve at runtime via `eglGetProcAddress` (same pattern as
/// `glEGLImageTargetTexture2DOES` in buffer.rs).
type EglDupNativeFenceFDANDROID =
    unsafe extern "system" fn(display: egl::EGLDisplay, sync: egl::EGLSync) -> i32;

pub struct SyncFence {
    sync: egl::Sync,
    fence_fd: OwnedFd,
    egl: egl::Instance<egl::Static>,
    egl_display: egl::Display,
}

impl SyncFence {
    /// Insert a sync point into the current GL stream and export the
    /// underlying dma-fence as an fd.
    ///
    /// Caller MUST have called `gl::Flush()` (or `gl::Finish()`) after
    /// the draw commands the fence is meant to gate. Without the flush
    /// GL commands may still be in the driver's userspace queue and
    /// the fence will signal before they reach the GPU. M5a step 8
    /// adds the flush to the plugin render loop; for now callers are
    /// responsible.
    pub fn create(gbm_egl: &GbmEgl) -> Result<Self, PluginError> {
        let (egl, egl_display) = gbm_egl.egl();

        // 1. Insert the sync into the GL stream. The attribute list
        //    contains only the EGL_NONE terminator — "fresh fence, no
        //    fd to import yet." The terminator is mandatory: khronos_egl's
        //    `check_attrib_list` rejects any slice whose last element
        //    isn't ATTRIB_NONE with BadParameter, before the call even
        //    reaches the driver.
        // SAFETY: create_sync is unsafe because `egl_display` must be a valid,
        // initialized EGL display; we got it from GbmEgl which guarantees both.
        // The attrib list is the minimal well-formed form (just the terminator).
        let sync = unsafe {
            egl.create_sync(
                egl_display,
                EGL_SYNC_NATIVE_FENCE_ANDROID,
                &[egl::ATTRIB_NONE],
            )
            .map_err(|_| PluginError::Render("eglCreateSync(NATIVE_FENCE_ANDROID) failed"))?
        };

        // 2. Export the dma-fence as an fd. ANDROID extension entry
        //    point, resolved at runtime — khronos_egl doesn't bind it.
        let dup_fn_ptr =
            egl.get_proc_address("eglDupNativeFenceFDANDROID")
                .ok_or(PluginError::Render(
                    "eglDupNativeFenceFDANDROID not available",
                ))?;
        // SAFETY: the EGL spec for EGL_ANDROID_native_fence_sync
        // declares the signature we're transmuting to. The extension
        // string check at host startup proves the entry point exists;
        // get_proc_address returning non-null is the runtime proof we
        // got a real function pointer.
        let dup_fn: EglDupNativeFenceFDANDROID = unsafe { std::mem::transmute(dup_fn_ptr) };

        // SAFETY: egl_display and sync are live handles owned by this
        // call's scope; the FFI call returns an integer fd or the
        // sentinel. No aliasing, no lifetime concerns at the boundary.
        let raw_fd = unsafe {
            dup_fn(
                egl_display.as_ptr() as egl::EGLDisplay,
                sync.as_ptr() as egl::EGLSync,
            )
        };
        if raw_fd == EGL_NO_NATIVE_FENCE_FD_ANDROID {
            // SAFETY: `destroy_sync` is unsafe because `sync` must be a valid
            // handle for `egl_display`. We created `sync` against this same
            // display a few lines above and have not destroyed it. Best-effort
            // cleanup on the error path — Result ignored because we're already
            // returning Err.
            unsafe {
                let _ = egl.destroy_sync(egl_display, sync);
            }
            return Err(PluginError::Render(
                "eglDupNativeFenceFDANDROID returned NO_FD",
            ));
        }

        // SAFETY: dup_fn returned a fresh fd owned by us per the EGL
        // spec; wrap so Drop closes it. The sync object continues to
        // reference the underlying dma-fence independently.
        let fence_fd = unsafe { OwnedFd::from_raw_fd(raw_fd) };

        Ok(Self {
            sync,
            fence_fd,
            egl: egl::Instance::new(egl::Static),
            egl_display,
        })
    }

    /// Borrow the fence fd for SCM_RIGHTS. Caller does not own it;
    /// `SyncFence` retains ownership and closes it on Drop.
    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.fence_fd.as_fd()
    }
}

impl Drop for SyncFence {
    fn drop(&mut self) {
        // SAFETY: `self.sync` is the handle this struct constructed in
        // `create()` against `self.egl_display`; both are still live because
        // Drop runs before the fields are dropped. Result is ignored — Drop
        // has no error channel and the worst case is a leaked sync object
        // that the process exiting collects anyway.
        unsafe {
            let _ = self.egl.destroy_sync(self.egl_display, self.sync);
        }
        // fence_fd drops in field order after this body returns, closing
        // the dma-fence fd.
    }
}
