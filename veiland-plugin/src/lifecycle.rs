// SPDX-License-Identifier: GPL-3.0-or-later

//! Lifecycle primitives that collapse the boilerplate every plugin used to
//! copy-paste: the connect/handshake/hello preamble, the wait-for-first-
//! Configure loop, and the FrameDone/BufferReleased self-pacing state
//! machine.
//!
//! These are **primitives the author drives**, not a framework. The author
//! still owns `main()`, the render code, and the event loop — `FramePacer`
//! just hands back one of three outcomes per turn so the loop body stays a
//! few obvious lines instead of a subtle flag dance. (If a `run_plugin()`
//! framework is ever wanted, it would be a thin layer over these — see the
//! crate-level decision in CLAUDE.md / project memory.)
//!
//! Typical shape:
//!
//! ```ignore
//! let mut conn = Connection::connect(PLUGIN_NAME, env!("CARGO_PKG_VERSION"))?;
//! let cfg = match conn.wait_for_configure()? {
//!     Some(c) => c,
//!     None => return Ok(()), // shutdown before first configure
//! };
//! let dma = DmaBuffer::new(&gbm_egl, cfg.region_w, cfg.region_h)?;
//! let mut pacer = FramePacer::new();
//! loop {
//!     match pacer.next(&mut conn)? {
//!         Frame::Render => {
//!             // ... render into `dma`, then submit the buffer ...
//!             conn.send_buffer(&buf_msg, dma.dmabuf_fd(), fence)?;
//!             pacer.submitted();
//!         }
//!         Frame::Reconfigure(c) => { scale = c.scale; }
//!         Frame::Shutdown => return Ok(()),
//!     }
//! }
//! ```

use veiland_protocol::{Configure, ServerMessage};

use crate::error::PluginError;
use crate::socket::Connection;

impl Connection {
    /// The full connect preamble in one call: read the socket fd from the
    /// environment, negotiate the protocol version + host capabilities, and
    /// send `Hello`. Equivalent to `from_env()` + `handshake()` +
    /// `send_hello(name, version)`, which every plugin did verbatim.
    ///
    /// `name` is the plugin's wire name (≤64 bytes), `version` its version
    /// string (≤32 bytes) — typically `env!("CARGO_PKG_VERSION")`.
    pub fn connect(name: &str, version: &str) -> Result<Self, PluginError> {
        let mut conn = Self::from_env()?;
        conn.handshake()?;
        conn.send_hello(name, version)?;
        Ok(conn)
    }

    /// Block until the host sends the first `Configure`, returning it.
    ///
    /// Returns `Ok(None)` if the host shuts down (or disconnects) before any
    /// Configure arrives — the caller should exit cleanly, not treat it as an
    /// error. Non-Configure messages that arrive before the first Configure
    /// (there generally are none in v1) are logged and skipped.
    ///
    /// The plugin needs the first Configure to learn its region size before
    /// it can allocate a dmabuf, so this is always the step right after
    /// `connect`.
    pub fn wait_for_configure(&mut self) -> Result<Option<Configure>, PluginError> {
        loop {
            match self.recv_event() {
                Ok(ServerMessage::Configure(c)) => return Ok(Some(c)),
                Ok(ServerMessage::Shutdown) => return Ok(None),
                Ok(other) => {
                    eprintln!(
                        "veiland-plugin: unexpected pre-configure message {other:?}, ignoring"
                    );
                }
                // A disconnect before the first Configure is a clean exit, not
                // a crash — same treatment as Shutdown.
                Err(PluginError::Disconnected) => return Ok(None),
                Err(e) => return Err(e),
            }
        }
    }
}

/// One turn of the plugin's main loop, as decided by [`FramePacer::next`].
#[derive(Debug)]
pub enum Frame {
    /// Render a new frame now and submit a buffer, then call
    /// [`FramePacer::submitted`]. The pacer has accounted for the
    /// FrameDone/BufferReleased handshake; the caller only has to draw.
    Render,
    /// The host re-sent `Configure` mid-stream (scale or region changed).
    /// The caller should update its state (e.g. `scale`); no render is forced
    /// this turn — the next `Render` will pick up the new state.
    Reconfigure(Configure),
    /// The host asked the plugin to exit (or disconnected). The caller should
    /// return from its loop cleanly.
    Shutdown,
}

/// Encapsulates the FrameDone/BufferReleased self-pacing the animated plugins
/// share: render on `BufferReleased` so the compositor's repaint rate drives
/// the animation, but only after the first `FrameDone` has arrived.
///
/// The state machine (two bools) is the part that was easy to get subtly
/// wrong when copy-pasted; this owns it. The caller drives it:
///
/// ```ignore
/// loop {
///     match pacer.next(&mut conn)? {
///         Frame::Render => { /* draw + submit */ pacer.submitted(); }
///         Frame::Reconfigure(c) => { /* update scale */ }
///         Frame::Shutdown => break,
///     }
/// }
/// ```
pub struct FramePacer {
    /// True once the host has released our last buffer (or we've never sent
    /// one). Gate: don't render again until the previous buffer is released.
    buffer_released: bool,
    /// True once the first FrameDone has arrived. Before that, BufferReleased
    /// alone must not trigger a render (the host opens the loop with a
    /// FrameDone alongside the first Configure).
    got_first_frame_done: bool,
}

impl Default for FramePacer {
    fn default() -> Self {
        Self::new()
    }
}

impl FramePacer {
    pub fn new() -> Self {
        Self {
            buffer_released: true,
            got_first_frame_done: false,
        }
    }

    /// Block for the next host event and translate it into a [`Frame`]
    /// outcome, advancing the pacing state machine.
    ///
    /// Returns:
    /// - [`Frame::Render`] when the caller should draw+submit a frame. The
    ///   caller MUST call [`submitted`](Self::submitted) after sending the
    ///   buffer, so the pacer knows a buffer is in flight again.
    /// - [`Frame::Reconfigure`] on a mid-stream Configure.
    /// - [`Frame::Shutdown`] on Shutdown or a clean disconnect.
    ///
    /// Events that don't map to a caller action (e.g. a FrameDone while a
    /// buffer is still in flight) are absorbed internally and `next` blocks
    /// again, so the caller only ever sees actionable outcomes.
    pub fn next(&mut self, conn: &mut Connection) -> Result<Frame, PluginError> {
        loop {
            let event = match conn.recv_event() {
                Ok(e) => e,
                Err(PluginError::Disconnected) => return Ok(Frame::Shutdown),
                Err(e) => return Err(e),
            };
            match event {
                ServerMessage::Configure(c) => return Ok(Frame::Reconfigure(c)),
                ServerMessage::FrameDone => {
                    self.got_first_frame_done = true;
                    if self.buffer_released {
                        return Ok(Frame::Render);
                    }
                    // Buffer still in flight; nothing to do — keep waiting.
                }
                ServerMessage::BufferReleased(_) => {
                    self.buffer_released = true;
                    // Self-pacing: redraw as soon as our buffer comes back,
                    // but only once the loop has been opened by a FrameDone.
                    if self.got_first_frame_done {
                        return Ok(Frame::Render);
                    }
                }
                ServerMessage::Shutdown => return Ok(Frame::Shutdown),
            }
        }
    }

    /// Tell the pacer a buffer was just submitted (one is now in flight), so
    /// it won't issue another [`Frame::Render`] until the host releases it.
    /// Call this immediately after `conn.send_buffer(...)`.
    pub fn submitted(&mut self) {
        self.buffer_released = false;
    }
}
