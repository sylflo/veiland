// SPDX-License-Identifier: GPL-3.0-or-later

//! `PluginState` — per-plugin state held by the host across the
//! lifetime of a plugin process. Owns the connection, the most
//! recently imported texture, and the plugin's identifying info
//! from `Hello` (used for logs).

use std::os::fd::OwnedFd;

use khronos_egl as egl;

use veiland_protocol::ClientMessage;

use super::dmabuf::{self, GlTexture};
use super::{HostConnection, HostError};

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

    /// Dispatch one `(ClientMessage, fd?)` pair from `recv_message`.
    /// Enforces the protocol state machine: Hello must come first,
    /// then Buffer / BufferDestroy in any order. Violations close
    /// the plugin (caller's responsibility on error).
    ///
    /// Requires the caller's EGL context to be current on the calling
    /// thread — `import_dmabuf` and `release_texture` both make GL
    /// calls. Calloop handler in step 6 makes this so by running on
    /// the main thread where the lock-surface context was set current.
    pub fn handle_message(
        &mut self,
        msg: ClientMessage,
        fd: Option<OwnedFd>,
        egl: &egl::Instance<egl::Static>,
        display: egl::Display,
    ) -> Result<(), HostError> {
        let has_hello = !self.name.is_empty();

        match msg {
            ClientMessage::Hello(h) => {
                if has_hello {
                    return Err(HostError::ProtocolViolation("double Hello"));
                }
                self.name = h.plugin_name;
                self.version = h.plugin_version;
                Ok(())
            }

            ClientMessage::Buffer(b) => {
                if !has_hello {
                    return Err(HostError::ProtocolViolation("Buffer before Hello"));
                }
                let fd = fd.ok_or(HostError::ProtocolViolation("Buffer without fd"))?;

                // Import the new buffer first; only release the old
                // texture if the import succeeded, so a failing import
                // doesn't leave us with no texture at all.
                let new_texture = dmabuf::import_dmabuf(egl, display, &b, &fd)?;

                if let Some(old) = self.texture.take() {
                    dmabuf::release_texture(egl, display, old);
                }
                self.texture = Some(new_texture);
                self.current_buffer_id = Some(b.id);

                // The fd is dropped here; EGL has its own reference.
                drop(fd);
                Ok(())
            }

            ClientMessage::BufferDestroy(d) => {
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
    ) {
        let Some(texture) = self.texture.as_ref() else {
            // No texture imported yet: the caller's clear-to-black
            // is the fallback. Same behavior as M2's "pre-import"
            // window.
            return;
        };

        unsafe {
            gl::UseProgram(program);

            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, texture.name);
            gl::Uniform1i(sampler_loc, 0);

            gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
            let a_pos =
                gl::GetAttribLocation(program, b"a_pos\0".as_ptr() as *const _);
            gl::EnableVertexAttribArray(a_pos as u32);
            gl::VertexAttribPointer(a_pos as u32, 2, gl::FLOAT, gl::FALSE, 0, std::ptr::null());

            gl::DrawArrays(gl::TRIANGLES, 0, 6);
        }
    }
}
