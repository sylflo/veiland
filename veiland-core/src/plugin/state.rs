// SPDX-License-Identifier: GPL-3.0-or-later

//! `PluginState` — per-plugin state held by the host across the
//! lifetime of a plugin process. Owns the connection, the most
//! recently imported texture, and the plugin's identifying info
//! from `Hello` (used for logs).

use khronos_egl as egl;

use veiland_protocol::ClientMessage;

use super::dmabuf::{self, GlTexture};
use super::sync::{import_fence, release_fence, wait_fence};
use super::{HostConnection, HostError, ReceivedFds};

pub struct PluginState {
    pub texture: Option<GlTexture>,
    pub current_buffer_id: Option<u32>,
    pub connection: HostConnection,
    pub name: String,
    pub version: String,
}

impl PluginState {
    /// Build a fresh state for a plugin we've just spawned. `name`
    /// and `version` are empty until the plugin's `Hello` arrives —
    /// that's also how `handle_message` detects "before Hello".
    pub fn new(connection: HostConnection) -> Self {
        Self {
            texture: None,
            current_buffer_id: None,
            connection,
            name: String::new(),
            version: String::new(),
        }
    }

    /// Dispatch one `(ClientMessage, ReceivedFds)` pair from `recv_message`.
    /// Enforces the protocol state machine: Hello must come first,
    /// then Buffer / BufferDestroy in any order. Violations close
    /// the plugin (caller's responsibility on error).
    ///
    /// The fd-shape invariant (Buffer has a dmabuf, others have none) is
    /// already enforced inside `recv_message`; the matches here are
    /// exhaustive on the variant and any cross-pair fallthrough is a
    /// protocol violation rather than a panic (per the "never panic on
    /// plugin input" rule).
    ///
    /// Requires the caller's EGL context to be current on the calling
    /// thread — `import_dmabuf` and `release_texture` both make GL
    /// calls. The calloop handler in main.rs makes this so by running
    /// on the main thread where the lock-surface context was set current.
    pub fn handle_message(
        &mut self,
        msg: ClientMessage,
        fds: ReceivedFds,
        egl: &egl::Instance<egl::Static>,
        display: egl::Display,
    ) -> Result<(), HostError> {
        let has_hello = !self.name.is_empty();

        match (msg, fds) {
            (ClientMessage::Hello(h), ReceivedFds::None) => {
                if has_hello {
                    return Err(HostError::ProtocolViolation("double Hello"));
                }
                self.name = h.plugin_name;
                self.version = h.plugin_version;
                Ok(())
            }

            (ClientMessage::Buffer(b), ReceivedFds::Buffer { dmabuf, fence }) => {
                if !has_hello {
                    return Err(HostError::ProtocolViolation("Buffer before Hello"));
                }

                // Fast path: wait on the plugin's fence before sampling
                // the dmabuf. If the wait fails (1-second timeout or EGL
                // error), bail without touching the previous texture —
                // defensive ordering: any error returned here leaves
                // self.texture in its prior, still-valid state, and the
                // calloop handler in main.rs treats the error as plugin
                // death. The fence is released either way.
                //
                // Slow path (`fence == None`): the plugin called
                // gl::Finish before send_buffer, so the dmabuf is already
                // GPU-stable. No wait needed.
                if let Some(fence_fd) = fence {
                    let imported = import_fence(egl, display, fence_fd)?;
                    let wait_result = wait_fence(egl, display, &imported);
                    release_fence(egl, display, imported);
                    wait_result?;
                }

                // Import the new buffer first; only release the old
                // texture if the import succeeded, so a failing import
                // doesn't leave us with no texture at all.
                let new_texture = dmabuf::import_dmabuf(egl, display, &b, &dmabuf)?;

                if let Some(old) = self.texture.take() {
                    dmabuf::release_texture(egl, display, old);
                }
                self.texture = Some(new_texture);
                self.current_buffer_id = Some(b.id);

                // The dmabuf fd is dropped here; EGL has its own reference.
                drop(dmabuf);
                Ok(())
            }

            (ClientMessage::BufferDestroy(d), ReceivedFds::None) => {
                if !has_hello {
                    return Err(HostError::ProtocolViolation("BufferDestroy before Hello"));
                }
                if self.current_buffer_id == Some(d.id) {
                    if let Some(old) = self.texture.take() {
                        dmabuf::release_texture(egl, display, old);
                    }
                    self.current_buffer_id = None;
                }
                // Mismatched id: not an error per spec — the plugin
                // may legitimately destroy a buffer the host already
                // released. Log and move on.
                Ok(())
            }

            // Variant ↔ fd-shape mismatch. recv_message should have
            // rejected this at the wire layer; if we see it here,
            // recv_message has a bug. Treat as violation rather than
            // panic per the "never panic on plugin input" rule —
            // belt-and-braces if a future refactor breaks the invariant.
            (_, _) => Err(HostError::ProtocolViolation(
                "variant/fd shape mismatch (recv_message bug?)",
            )),
        }
    }

    /// Draw the plugin's current texture into the bound framebuffer
    /// using the host's compositor shader. No-op if no texture is
    /// imported yet — the caller's `glClear` is the fallback.
    ///
    /// The shader, VBO, and sampler uniform are owned by `main.rs`
    /// (per-host, not per-plugin) and passed in. The EGL context
    /// must be current on the calling thread and the target
    /// framebuffer already bound.
    pub fn composite(
        &self,
        program: gl::types::GLuint,
        vbo: gl::types::GLuint,
        sampler_loc: gl::types::GLint,
        rect_loc: gl::types::GLint,
        rect: [f32; 4],
    ) {
        let Some(texture) = self.texture.as_ref() else {
            // No texture imported yet: the caller's clear-to-black
            // is the fallback. Same behavior as M2's "pre-import"
            // window.
            return;
        };

        unsafe {
            gl::UseProgram(program);

            // Per-plugin clip-space rect. Must come *after* UseProgram
            // (uniform locations are program-scoped; setting before
            // binding the program is undefined). See main.rs's
            // region_to_clip_rect for the pixel→clip math.
            gl::Uniform4f(rect_loc, rect[0], rect[1], rect[2], rect[3]);

            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, texture.name);
            gl::Uniform1i(sampler_loc, 0);

            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
            let a_pos = gl::GetAttribLocation(program, c"a_pos".as_ptr());
            gl::EnableVertexAttribArray(a_pos as u32);
            gl::VertexAttribPointer(a_pos as u32, 2, gl::FLOAT, gl::FALSE, 0, std::ptr::null());

            gl::DrawArrays(gl::TRIANGLES, 0, 6);
        }
        crate::gl_debug::check_gl("composite: draw plugin texture");
    }
}
