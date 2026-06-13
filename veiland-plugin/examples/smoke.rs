// SPDX-License-Identifier: GPL-3.0-or-later

//! Smoke test for the `veiland-plugin` public API.
//!
//! Exercises every public method in the order a real plugin would call
//! them, against a real `GbmEgl` (so the EGL/GBM dance runs) and a
//! `Connection::from_env()` that will fail because no host has set
//! `VEILAND_PLUGIN_SOCKET`. The point is not to *run* a plugin
//! end-to-end — that needs the host-side conversion, which is the next
//! M3 task — but to confirm the API composes the way the canonical
//! event loop expects.
//!
//! Run with: `cargo run --example smoke -p veiland-plugin`
//! Expected output: GBM/EGL setup logs, then "no VEILAND_PLUGIN_SOCKET
//! in env, as expected for a standalone smoke test, exiting." If the GL
//! setup fails before that, something in `GbmEgl::new` or
//! `DmaBuffer::new` is wrong.

use std::process::ExitCode;

use veiland_plugin::{Connection, DmaBuffer, GbmEgl, PluginError};
use veiland_protocol::{Buffer, ServerMessage};

fn main() -> ExitCode {
    match smoke() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("smoke test failed: {}", e);
            ExitCode::FAILURE
        }
    }
}

fn smoke() -> Result<(), PluginError> {
    // 1. Render setup. Runs the full EGL/GBM dance and leaves a GL context
    //    current. If this fails, the smoke test fails — there is nothing
    //    to fall back to on a machine without /dev/dri/renderD128.
    eprintln!("smoke: GbmEgl::new()");
    let gbm_egl = GbmEgl::new()?;

    // 2. Allocate a 512×512 ARGB8888 dmabuf-backed render target.
    eprintln!("smoke: DmaBuffer::new(512, 512)");
    let dma = DmaBuffer::new(&gbm_egl, 512, 512)?;
    eprintln!(
        "smoke: allocated {}x{}, stride={}, modifier=0x{:016x}, format={:?}",
        dma.width(),
        dma.height(),
        dma.stride(),
        u64::from(dma.modifier()),
        dma.format(),
    );

    // 3. Bind the FBO and clear it to a known color. This is the smallest
    //    GL operation that proves the FBO is actually attached: if the
    //    framebuffer status is bad, glClear with no current FBO would
    //    silently target the default (which doesn't exist surfacelessly).
    eprintln!("smoke: bind_for_rendering + glClear");
    dma.bind_for_rendering()?;
    // SAFETY: gl::ClearColor / gl::Clear need a current GL context.
    // GbmEgl::new made one current; we have not detached it.
    unsafe {
        gl::ClearColor(0.5, 0.0, 0.5, 1.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);
    }
    dma.finish();

    // 4. Build the wire-format Buffer message the plugin would send. We do
    //    not send it (there is no host), but constructing it confirms the
    //    DmaBuffer getters return the fields the protocol expects.
    let buf_msg = Buffer {
        id: 0,
        width: dma.width(),
        height: dma.height(),
        format: dma.format(),
        modifier: dma.modifier(),
        stride: dma.stride(),
        offset: 0,
    };
    eprintln!("smoke: built Buffer message: {:?}", buf_msg);

    // 5. Borrow the dmabuf fd — this is what would go in send_buffer's
    //    second argument. Just exercising the call.
    let _fd = dma.dmabuf_fd();

    // 6. Attempt to connect. This is expected to fail with
    //    `PluginError::Env(...)` because we are not running under a host
    //    that set VEILAND_PLUGIN_SOCKET. Treat that specific failure as
    //    success — any other failure is a real bug.
    eprintln!("smoke: Connection::from_env()");
    match Connection::from_env() {
        Err(PluginError::Env(_)) => {
            eprintln!(
                "smoke: no VEILAND_PLUGIN_SOCKET in env, as expected for \
                 a standalone smoke test, exiting."
            );
            Ok(())
        }
        Err(e) => Err(e),
        Ok(mut conn) => {
            // Hypothetical: if a host *did* set the env var (e.g. someone
            // ran us under the real host as a test), do the full minimal
            // loop. This branch is unreachable in the smoke test today
            // but documents the canonical event loop for plugin authors.
            conn.handshake()?;
            conn.send_hello("smoke", "0.1")?;
            loop {
                match conn.recv_event()? {
                    ServerMessage::Configure(_) => {}
                    ServerMessage::FrameDone => {
                        dma.bind_for_rendering()?;
                        // ... draw calls ...
                        dma.finish();
                        conn.send_buffer(&buf_msg, dma.dmabuf_fd(), None)?;
                    }
                    ServerMessage::BufferReleased(_) => {}
                    ServerMessage::Shutdown => return Ok(()),
                }
            }
        }
    }
}
