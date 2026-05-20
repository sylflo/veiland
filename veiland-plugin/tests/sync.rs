// SPDX-License-Identifier: GPL-3.0-or-later

//! Smoke test for `SyncFence::create` — does the EGL fence + fd export
//! actually work on this machine's GPU?
//!
//! `#[ignore]` because it opens `/dev/dri/renderD128` and exercises EGL,
//! neither of which is available in CI or a sandbox. Run by hand on the
//! dev boxes:
//!
//!     cargo test -p veiland-plugin --test sync -- --ignored
//!
//! Pass = `EGL_ANDROID_native_fence_sync` is functional on this driver
//! and the M5a fast path is buildable here. Fail = either the extension
//! is missing (step 1's detection should have already warned) or the
//! driver advertises it but `eglDupNativeFenceFDANDROID` returns NO_FD.

use std::os::fd::AsRawFd;

use veiland_plugin::{DmaBuffer, GbmEgl, SyncFence};

#[test]
#[ignore]
fn create_fence_after_minimal_draw() {
    let gbm_egl = GbmEgl::new().expect("GbmEgl::new (no render node? wrong perms?)");
    // A real FBO so the fence has something meaningful to gate. A fence
    // on an empty stream should still succeed in principle, but mirroring
    // the production caller's setup makes the test more representative.
    let dma = DmaBuffer::new(&gbm_egl, 256, 256).expect("DmaBuffer::new");
    dma.bind_for_rendering().expect("bind FBO");

    // SAFETY: GL context is current on this thread (GbmEgl::new made it
    // current); the FBO is bound; the call sequence is well-formed.
    unsafe {
        gl::Clear(gl::COLOR_BUFFER_BIT);
        // Required before fence creation — without Flush, GL commands may
        // still be in the driver's userspace queue and the fence would
        // signal before they reach the GPU. SyncFence's doc-comment
        // mandates this; the test enforces it.
        gl::Flush();
    }

    let fence = SyncFence::create(&gbm_egl).expect("SyncFence::create");
    let raw = fence.as_fd().as_raw_fd();
    assert!(raw >= 0, "fence fd should be valid, got {}", raw);
    eprintln!("SyncFence created, fd = {}", raw);
    // fence drops here: closes fd + destroys EGL sync object.
}
