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
//! let mut dma = DmaBuffer::new(&gbm_egl, cfg.region_w, cfg.region_h)?;
//! let mut pacer = FramePacer::self_paced();
//! loop {
//!     match pacer.next(&mut conn)? {
//!         Frame::Render => {
//!             // ... render into `dma`, then hand it off. `conn.submit_frame`
//!             // picks the sync model (fence fd vs glFinish) for you.
//!             conn.submit_frame(&dma, &gbm_egl)?;
//!             pacer.submitted();
//!         }
//!         Frame::Reconfigure(c) => {
//!             dma.resize_or_keep(&gbm_egl, c.region_w, c.region_h, PLUGIN_NAME);
//!         }
//!         Frame::Shutdown => return Ok(()),
//!     }
//! }
//! ```

use veiland_protocol::{Buffer, Configure, ServerMessage};

use crate::buffer::DmaBuffer;
use crate::error::PluginError;
use crate::render::GbmEgl;
use crate::socket::Connection;
use crate::sync::SyncFence;

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

/// Which cadence a [`FramePacer`] drives. The two modes capture the two
/// patterns the plugins actually use; they differ only in what happens when
/// the host releases a buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Animated plugins (particles, sakura): redraw continuously. As soon as
    /// the host releases our buffer, render the next frame — the compositor's
    /// repaint rate ends up driving the animation. Gated on the first
    /// FrameDone so we don't draw before the loop is opened.
    SelfPaced,
    /// Mostly-static plugins (clock, wallpaper, label, vignette): redraw only
    /// when the host asks (FrameDone). If a FrameDone arrives while a buffer
    /// is still in flight, the request is remembered and serviced when the
    /// buffer is released. No continuous redraw.
    OnDemand,
}

/// Encapsulates the FrameDone/BufferReleased pacing state machine the plugins
/// share — the part that was easy to get subtly wrong when copy-pasted. The
/// caller drives it; it just hands back actionable [`Frame`] outcomes:
///
/// ```ignore
/// let mut pacer = FramePacer::self_paced(); // or ::on_demand()
/// loop {
///     match pacer.next(&mut conn)? {
///         Frame::Render => { /* draw + submit */ pacer.submitted(); }
///         Frame::Reconfigure(c) => { /* update scale */ }
///         Frame::Shutdown => break,
///     }
/// }
/// ```
pub struct FramePacer {
    mode: Mode,
    /// True once the host has released our last buffer (or we've never sent
    /// one). Gate: don't render again until the previous buffer is released.
    buffer_released: bool,
    /// `SelfPaced`: set once the first FrameDone has arrived, after which a
    /// BufferReleased triggers the next render. `OnDemand`: set when a
    /// FrameDone arrives while a buffer is still in flight, so the deferred
    /// render fires on the next BufferReleased. Same bool, two readings.
    frame_pending: bool,
}

impl Default for FramePacer {
    fn default() -> Self {
        Self::self_paced()
    }
}

impl FramePacer {
    /// Continuous-redraw pacing for animated plugins. Renders again every time
    /// the host releases a buffer (after the first FrameDone), so the loop
    /// runs at the compositor's repaint rate.
    pub fn self_paced() -> Self {
        Self {
            mode: Mode::SelfPaced,
            buffer_released: true,
            frame_pending: false,
        }
    }

    /// Event-driven pacing for mostly-static plugins. Renders only on
    /// FrameDone (deferring to the next BufferReleased if a buffer is still in
    /// flight); does not redraw on its own.
    pub fn on_demand() -> Self {
        Self {
            mode: Mode::OnDemand,
            buffer_released: true,
            frame_pending: false,
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
                    if self.buffer_released {
                        // Free to draw now. In SelfPaced this also records that
                        // the loop has been opened (so future BufferReleased
                        // self-paces); the flag is harmless in OnDemand since
                        // it's only consulted while a buffer is in flight.
                        self.frame_pending = self.mode == Mode::SelfPaced;
                        return Ok(Frame::Render);
                    }
                    // Buffer in flight: defer the request to the next release.
                    self.frame_pending = true;
                }
                ServerMessage::BufferReleased(_) => {
                    self.buffer_released = true;
                    // Both modes: render now iff a frame is "pending". For
                    // SelfPaced `frame_pending` is latched true after the first
                    // FrameDone (continuous); for OnDemand it's true only when
                    // a FrameDone was deferred.
                    if self.frame_pending {
                        if self.mode == Mode::OnDemand {
                            self.frame_pending = false;
                        }
                        return Ok(Frame::Render);
                    }
                }
                ServerMessage::Shutdown => return Ok(Frame::Shutdown),
            }
        }
    }

    /// Tell the pacer a buffer was just submitted (one is now in flight), so
    /// it won't issue another [`Frame::Render`] until the host releases it.
    /// Call this immediately after `conn.submit_frame(...)`.
    pub fn submitted(&mut self) {
        self.buffer_released = false;
    }
}

impl Connection {
    /// Submit the current dmabuf frame to the host.
    ///
    /// Builds the wire `Buffer` message from `dma` fresh every call (id = 0).
    /// Picks sync model from the two cached capability booleans: fast path =
    /// `glFlush` + fence fd; slow path = `glFinish`. Replaces the
    /// `buffer_msg_for` + `fast_path` plumbing every plugin previously
    /// copy-pasted.
    pub fn submit_frame(&mut self, dma: &DmaBuffer, gbm_egl: &GbmEgl) -> Result<(), PluginError> {
        let buf_msg = Buffer {
            id: 0,
            width: dma.width(),
            height: dma.height(),
            format: dma.format(),
            modifier: dma.modifier(),
            stride: dma.stride(),
            offset: 0,
        };
        if self.host_supports_fence_fd() && gbm_egl.supports_fence_fd() {
            // SAFETY: requires a current GL context, which all plugins have
            // after GbmEgl::new(). glFlush flushes without blocking so the
            // fence fd signals when the GPU actually finishes.
            unsafe { gl::Flush() };
            let fence = SyncFence::create(gbm_egl)?;
            self.send_buffer(&buf_msg, dma.dmabuf_fd(), Some(fence.as_fd()))
        } else {
            dma.finish();
            self.send_buffer(&buf_msg, dma.dmabuf_fd(), None)
        }
    }
}
