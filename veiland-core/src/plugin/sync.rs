// SPDX-License-Identifier: GPL-3.0-or-later

//! Host-side EGL sync fence: import-from-fd (for plugin-sent fences) +
//! create-locally (for host's own egress sync), plus the shared wait/release.
//!
//! Two construction paths, same post-construction shape:
//! - `import_fence(fd)`: plugin sent a fence fd, host adopts it. Wait
//!   gates the plugin's GPU completion before the host samples the
//!   dmabuf. M5a steps 3 and 9.
//! - `create_host_fence()`: host inserts a fence into its own GL stream
//!   after sampling. Wait gates the host's GPU completion of the
//!   composite before sending `BufferReleased` to the plugin. M5a step 10.
//!
//! Both require `EGL_ANDROID_native_fence_sync` on the host's EGL display;
//! detection at startup gates `HOST_CAP_FENCE_FD` in the handshake, so
//! this module assumes the capability is present.

use std::os::fd::{AsRawFd, OwnedFd};

use khronos_egl as egl;

use super::HostError;

/// `EGL_SYNC_NATIVE_FENCE_ANDROID` — sync type for an imported dma-fence.
const EGL_SYNC_NATIVE_FENCE_ANDROID: egl::Enum = 0x3144;

/// `EGL_SYNC_NATIVE_FENCE_FD_ANDROID` — attribute key whose value is the
/// fd to import. EGL dups the fd internally during the create call.
const EGL_SYNC_NATIVE_FENCE_FD_ANDROID: egl::Attrib = 0x3145;

/// Generous timeout for `eglClientWaitSync`: 1 second. A real fence
/// signals in milliseconds; if it takes a full second the plugin's GPU
/// is wedged or never submitted the work it claimed.
const FENCE_WAIT_TIMEOUT_NS: egl::Time = 1_000_000_000;

/// One EGL sync fence held by the host. Either imported from a plugin-
/// supplied fd (`import_fence`) or created on the host's own GL stream
/// (`create_host_fence`); same shape post-construction.
///
/// No `Drop` — destruction requires an EGL context current. Caller
/// must `release_fence` before letting this go out of scope. Same
/// pattern as `GlTexture` in this module's sibling `dmabuf.rs`.
pub struct HostFence {
    pub sync: egl::Sync,
}

/// Import a fence fd as an EGL sync object. Takes the fd by value:
/// EGL dups internally so the original is dropped right after the
/// call, closing the host's copy of the plugin-sent fd.
///
/// On failure: returns `ProtocolViolation`. The fd may have been bad,
/// the driver may have rejected it, etc. — any of which suggests the
/// plugin is misbehaving. Caller treats as plugin death.
pub fn import_fence(
    egl: &egl::Instance<egl::Static>,
    display: egl::Display,
    fd: OwnedFd,
) -> Result<HostFence, HostError> {
    let attribs: [egl::Attrib; 3] = [
        EGL_SYNC_NATIVE_FENCE_FD_ANDROID,
        fd.as_raw_fd() as egl::Attrib,
        egl::ATTRIB_NONE,
    ];

    // SAFETY: create_sync is unsafe because the display must be a valid,
    // initialized EGL display; the host's main.rs initialized this one
    // and keeps it alive for the process lifetime. The attrib list is
    // properly terminated. The fd is borrowed by EGL for the duration
    // of this call only — EGL dups internally.
    let sync = unsafe {
        egl.create_sync(display, EGL_SYNC_NATIVE_FENCE_ANDROID, &attribs)
            .map_err(|_| {
                HostError::ProtocolViolation(
                    "eglCreateSync(NATIVE_FENCE_ANDROID, imported fd) failed",
                )
            })?
    };

    // `fd` drops here, closing the host's copy. The dma-fence object
    // is kept alive by EGL's internal dup until destroy_sync.
    Ok(HostFence { sync })
}

/// Create a fence on the host's own GL stream, with no fd export.
/// Used for the egress sync after `repaint_lock_surfaces`: the host
/// inserts a sync point, waits on it, then sends `BufferReleased` to
/// the plugin once the GPU has finished sampling the dmabuf.
///
/// Caller must have an active EGL context current on this thread and
/// must have `gl::Flush`-ed (or used `swap_buffers`, which flushes
/// implicitly) before calling this — otherwise the fence may signal
/// before the commands it's meant to gate reach the GPU.
///
/// On failure: returns `Render` (the host's EGL is misbehaving; this
/// is a host-side issue, not a plugin-protocol one).
pub fn create_host_fence(
    egl: &egl::Instance<egl::Static>,
    display: egl::Display,
) -> Result<HostFence, HostError> {
    // SAFETY: see import_fence — same invariants. The minimal attrib
    // list is just the terminator (no fd to import; we're creating a
    // fresh fence on the local stream).
    let sync = unsafe {
        egl.create_sync(display, EGL_SYNC_NATIVE_FENCE_ANDROID, &[egl::ATTRIB_NONE])
            .map_err(|_| {
                HostError::Render("eglCreateSync(NATIVE_FENCE_ANDROID, fresh) failed")
            })?
    };
    Ok(HostFence { sync })
}

/// Block the calling thread until the fence signals, or until
/// `FENCE_WAIT_TIMEOUT_NS` elapses. Returns `Render` for timeout or
/// any other wait failure; the caller decides what that means in
/// context (plugin-fence timeout → plugin death; host-fence timeout
/// → host GPU is wedged).
///
/// Caller must have an active EGL context current on this thread.
pub fn wait_fence(
    egl: &egl::Instance<egl::Static>,
    display: egl::Display,
    fence: &HostFence,
) -> Result<(), HostError> {
    // SAFETY: client_wait_sync is unsafe because both display and sync
    // must be valid. `display` is the host's process-wide display; the
    // sync was created by import_fence or create_host_fence against
    // the same display. No flags: caller is expected to have flushed
    // before issuing the sync, so SYNC_FLUSH_COMMANDS_BIT isn't needed.
    let status = unsafe {
        egl.client_wait_sync(display, fence.sync, 0, FENCE_WAIT_TIMEOUT_NS)
            .map_err(|_| HostError::Render("eglClientWaitSync failed"))?
    };

    if status == egl::CONDITION_SATISFIED {
        Ok(())
    } else if status == egl::TIMEOUT_EXPIRED {
        Err(HostError::Render(
            "eglClientWaitSync timed out (fence never signalled)",
        ))
    } else {
        // Other return values aren't documented for this entry point;
        // treat as a failure.
        Err(HostError::Render(
            "eglClientWaitSync returned unexpected status",
        ))
    }
}

/// Tear down a host fence. Caller must have an active EGL context
/// current. Failure is logged-and-ignored: we're done with this fence
/// either way and the worst case is a leaked EGL handle.
pub fn release_fence(
    egl: &egl::Instance<egl::Static>,
    display: egl::Display,
    fence: HostFence,
) {
    // SAFETY: same invariants as the create call — display and sync
    // must be valid; both were produced by this module's constructors
    // and have not been destroyed.
    unsafe {
        let _ = egl.destroy_sync(display, fence.sync);
    }
}

#[cfg(test)]
mod tests {
    //! Smoke test for the import + wait + release round-trip.
    //!
    //! Bootstraps a self-contained EGL/GBM/GL context (the production
    //! host gets its display from the Wayland connection — we don't
    //! have a compositor in the test environment), creates a fence the
    //! same way `veiland-plugin`'s `SyncFence::create` does, and runs
    //! `import_fence` → `wait_fence` → `release_fence` against the fd.
    //!
    //! `#[ignore]` because it needs `/dev/dri/renderD128` and a real GPU:
    //!
    //!     cargo test -p veiland-core --lib plugin::sync -- --ignored
    //!
    //! Platform caveat: bootstraps via GBM, so on drivers that only
    //! expose `EGL_ANDROID_native_fence_sync` on non-GBM platforms
    //! (notably NVIDIA proprietary) the test will fail even though the
    //! production host — using a Wayland-derived display — works fine.

    use super::*;

    use std::ffi::c_void;
    use std::os::fd::FromRawFd;

    const EGL_NO_NATIVE_FENCE_FD_ANDROID: i32 = -1;

    type EglDupNativeFenceFDANDROID =
        unsafe extern "system" fn(display: egl::EGLDisplay, sync: egl::EGLSync) -> i32;

    struct TestStack {
        egl: egl::Instance<egl::Static>,
        display: egl::Display,
        // Held for Drop: GBM device must outlive the EGL display we
        // got from its pointer. Never read after construction.
        #[allow(dead_code)]
        gbm: gbm::Device<OwnedFd>,
        #[allow(dead_code)]
        context: egl::Context,
    }

    fn bootstrap() -> TestStack {
        use gbm::AsRaw;

        let drm_fd = nix::fcntl::open(
            "/dev/dri/renderD128",
            nix::fcntl::OFlag::O_RDWR | nix::fcntl::OFlag::O_CLOEXEC,
            nix::sys::stat::Mode::empty(),
        )
        .expect("open /dev/dri/renderD128");
        let gbm = gbm::Device::new(drm_fd).expect("gbm::Device::new");

        let egl = egl::Instance::new(egl::Static);
        // SAFETY: gbm.as_raw() points into the live gbm::Device we
        // just built; we move it into TestStack alongside the display
        // so its lifetime covers the display's.
        let display = unsafe { egl.get_display(gbm.as_raw() as *mut c_void) }.expect("get_display");
        egl.initialize(display).expect("eglInitialize");
        egl.bind_api(egl::OPENGL_ES_API).expect("bindAPI");

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
        let config = egl
            .choose_first_config(display, &config_attribs)
            .expect("choose_config")
            .expect("no matching config");
        let context_attribs = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];
        let context = egl
            .create_context(display, config, None, &context_attribs)
            .expect("create_context");
        egl.make_current(display, None, None, Some(context))
            .expect("make_current");
        gl::load_with(|name| {
            egl.get_proc_address(name)
                .map(|p| p as *const _)
                .unwrap_or(std::ptr::null())
        });

        TestStack {
            egl,
            display,
            gbm,
            context,
        }
    }

    /// Create a fence on the local display and export its fd, the same
    /// way a plugin would. Returns the fd as an `OwnedFd`; the EGL
    /// sync handle is destroyed immediately because the kernel
    /// dma-fence is kept alive by the fd.
    fn make_fence_fd(stack: &TestStack) -> OwnedFd {
        // SAFETY: GL context current (bootstrap made it so); flush
        // ensures the submitted-but-empty command stream reaches the
        // driver before fence creation.
        unsafe {
            gl::Clear(gl::COLOR_BUFFER_BIT);
            gl::Flush();
        }

        // SAFETY: minimal well-formed attrib list, initialized display.
        let sync = unsafe {
            stack
                .egl
                .create_sync(
                    stack.display,
                    EGL_SYNC_NATIVE_FENCE_ANDROID,
                    &[egl::ATTRIB_NONE],
                )
                .expect("create_sync(NATIVE_FENCE_ANDROID)")
        };

        let dup_fn_ptr = stack
            .egl
            .get_proc_address("eglDupNativeFenceFDANDROID")
            .expect("eglDupNativeFenceFDANDROID not available");
        // SAFETY: signature matches the EGL_ANDROID_native_fence_sync spec.
        let dup_fn: EglDupNativeFenceFDANDROID = unsafe { std::mem::transmute(dup_fn_ptr) };
        // SAFETY: display + sync are live, owned by this scope.
        let raw_fd = unsafe {
            dup_fn(
                stack.display.as_ptr() as egl::EGLDisplay,
                sync.as_ptr() as egl::EGLSync,
            )
        };
        assert_ne!(
            raw_fd, EGL_NO_NATIVE_FENCE_FD_ANDROID,
            "eglDupNativeFenceFDANDROID returned NO_FD"
        );

        // SAFETY: we created `sync` against `stack.display` and have
        // not yet destroyed it.
        unsafe {
            let _ = stack.egl.destroy_sync(stack.display, sync);
        }

        // SAFETY: dup_fn returned a fresh, owned fd per the spec.
        unsafe { OwnedFd::from_raw_fd(raw_fd) }
    }

    #[test]
    #[ignore]
    fn import_wait_release_roundtrip() {
        let stack = bootstrap();
        let fd = make_fence_fd(&stack);

        let fence = import_fence(&stack.egl, stack.display, fd).expect("import_fence");
        wait_fence(&stack.egl, stack.display, &fence).expect("wait_fence");
        release_fence(&stack.egl, stack.display, fence);
        eprintln!("import → wait → release: ok");
    }
}
