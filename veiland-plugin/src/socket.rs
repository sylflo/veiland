// SPDX-License-Identifier: GPL-3.0-or-later

//! `Connection`: the plugin-side wrapper over the seqpacket socket to the host.
//! Owns the handshake, the tagged-message I/O, and the `SCM_RIGHTS` fd dance.

use nix::sys::socket::{ControlMessage, ControlMessageOwned, MsgFlags, recvmsg, sendmsg};

use std::{
    io::{IoSlice, IoSliceMut},
    os::{
        fd::{AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
        unix::net::UnixStream,
    },
};

use veiland_protocol::{
    Buffer, BufferDestroy, ClientMessage, Hello, PROTOCOL_VERSION, ServerMessage, read_version,
    write_version,
};

use crate::error::PluginError;

/// Plugin-side connection to the host. One per process.
///
/// `UnixStream` is normally a `SOCK_STREAM` type, but it works fine over
/// `SOCK_SEQPACKET`: we never call its stream methods, only `as_raw_fd()`
/// to feed `nix::sendmsg`/`recvmsg`. The seqpacket framing lives in the
/// kernel, not in `UnixStream`.
pub struct Connection {
    socket: UnixStream,
}

impl Connection {
    /// Wrap an already-opened seqpacket fd. Takes ownership.
    ///
    /// In production: the parent process creates a socketpair, `exec`s the
    /// plugin with one end as fd 3, and tells the plugin via the env var.
    /// In tests: the test creates a socketpair directly and hands one end here.
    pub fn from_fd(fd: OwnedFd) -> Self {
        Self {
            socket: UnixStream::from(fd),
        }
    }

    /// Read `VEILAND_PLUGIN_SOCKET` from the environment, parse it as a fd
    /// number, and wrap it. This is the production entry point.
    pub fn from_env() -> Result<Self, PluginError> {
        let raw = std::env::var("VEILAND_PLUGIN_SOCKET")
            .map_err(|_| PluginError::Env("VEILAND_PLUGIN_SOCKET not set"))?;
        let fd: RawFd = raw
            .parse()
            .map_err(|_| PluginError::Env("VEILAND_PLUGIN_SOCKET not a valid fd"))?;
        // SAFETY: the parent (host) opened this fd via socketpair + exec and
        // is contractually transferring ownership to us via the env var. Per
        // `docs/protocol.md` §2, this is the spawn contract. If the env var
        // lies, the next syscall will fail loudly — but the unsafety here is
        // the ownership claim, not a validity check.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(Self::from_fd(owned))
    }

    /// Negotiate the protocol version with the host. See `docs/protocol.md` §5.
    ///
    /// Writes our `PROTOCOL_VERSION` as four little-endian bytes, then reads
    /// four bytes back. Returns `Disconnected` if the host closed the socket
    /// without replying (the spec's signal for "version rejected"), or
    /// `VersionMismatch` if the host's version differs from ours.
    ///
    /// Must be called once, before any other method. Calling it twice or
    /// calling `send_*` before it produces undefined behavior at the host
    /// (which will close the socket; we will see it as `Disconnected` on the
    /// next `recv_event`).
    pub fn handshake(&mut self) -> Result<(), PluginError> {
        let mut version_out = Vec::new();
        write_version(&mut version_out);
        sendmsg::<()>(
            self.socket.as_raw_fd(),
            &[IoSlice::new(&version_out)],
            &[],
            MsgFlags::empty(),
            None,
        )?;

        let mut reply = [0u8; 4];
        let mut iov = [IoSliceMut::new(&mut reply)];
        let msg = recvmsg::<()>(self.socket.as_raw_fd(), &mut iov, None, MsgFlags::empty())?;
        match msg.bytes {
            0 => Err(PluginError::Disconnected),
            4 => {
                let host_version = read_version(&reply)?;
                if host_version != PROTOCOL_VERSION {
                    return Err(PluginError::VersionMismatch {
                        plugin: PROTOCOL_VERSION,
                        host: host_version,
                    });
                }
                Ok(())
            }
            _ => Err(PluginError::ProtocolViolation(
                "handshake reply was not 4 bytes",
            )),
        }
    }

    /// Encode and send one `ClientMessage` with no ancillary data. Shared
    /// implementation for the public `send_hello` / `send_buffer_destroy`
    /// methods. Private on purpose — the named methods are the API surface.
    fn send_message_no_fd(&mut self, msg: &ClientMessage) -> Result<(), PluginError> {
        let mut buf = Vec::new();
        msg.encode(&mut buf)?;
        sendmsg::<()>(
            self.socket.as_raw_fd(),
            &[IoSlice::new(&buf)],
            &[],
            MsgFlags::empty(),
            None,
        )?;
        Ok(())
    }

    /// Send the `Hello` handshake message. See `docs/protocol.md` §6.1.
    ///
    /// Must be sent exactly once, immediately after `handshake()` and before
    /// any other client message. The host treats any other message before
    /// `Hello` as a protocol violation.
    ///
    /// `plugin_name` is capped at 64 bytes, `plugin_version` at 32 (per spec);
    /// strings over the cap return `Protocol(StringTooLong)`.
    pub fn send_hello(
        &mut self,
        plugin_name: &str,
        plugin_version: &str,
    ) -> Result<(), PluginError> {
        let msg = ClientMessage::Hello(Hello {
            plugin_name: plugin_name.to_string(),
            plugin_version: plugin_version.to_string(),
        });

        self.send_message_no_fd(&msg)
    }

    /// Tell the host the plugin will no longer reuse this buffer id, so the
    /// host can drop any cached EGLImage / GL resources keyed on it. See
    /// `docs/protocol.md` §6.3.
    ///
    /// In v1 with a single-buffer plugin this is rarely needed — socket close
    /// at shutdown already prompts the host to free everything. Buffer-pool
    /// plugins (M5+) will use this more.
    pub fn send_buffer_destroy(&mut self, id: u32) -> Result<(), PluginError> {
        let msg = ClientMessage::BufferDestroy(BufferDestroy { id });

        self.send_message_no_fd(&msg)
    }

    /// Send a `Buffer` message with its dmabuf fd attached via `SCM_RIGHTS`.
    /// The fd is borrowed — the plugin keeps ownership of the underlying
    /// GBM bo and may re-send the same fd across frames.
    pub fn send_buffer(&mut self, buffer: &Buffer, fd: BorrowedFd<'_>) -> Result<(), PluginError> {
        let mut buf = Vec::new();
        let msg = ClientMessage::Buffer(buffer.clone());
        msg.encode(&mut buf)?;

        let fds = [fd.as_raw_fd()];
        let cmsgs = [ControlMessage::ScmRights(&fds)];

        sendmsg::<()>(
            self.socket.as_raw_fd(),
            &[IoSlice::new(&buf)],
            &cmsgs,
            MsgFlags::empty(),
            None,
        )?;
        Ok(())
    }

    /// Block until one `ServerMessage` arrives from the host, or the host
    /// disconnects. See `docs/protocol.md` §7 for the message set.
    ///
    /// Errors:
    /// - `Disconnected` — host closed the socket cleanly. The plugin's main
    ///   loop should treat this as the normal end-of-life signal, not a bug.
    /// - `ProtocolViolation("server message carried an fd")` — host attached
    ///   an fd to a message that should never carry one. v1 server messages
    ///   are all fd-less; any fd is a host bug. We close the unexpected fd
    ///   before returning to avoid leaking it.
    /// - `Protocol(_)` — bytes did not decode as a valid `ServerMessage`.
    /// - `Nix(_)` / `Io(_)` — syscall-layer failure.
    pub fn recv_event(&mut self) -> Result<ServerMessage, PluginError> {
        let mut buf = [0u8; 64 * 1024];
        let mut cmsg_buf = nix::cmsg_space!(RawFd);

        // Pull the byte count and any fds out of the recvmsg result, then let
        // `msg` and `iov` go out of scope before we re-read `buf` for decoding —
        // otherwise the iov's mutable borrow of `buf` would conflict.
        let (bytes, unexpected_fds) = {
            let mut iov = [IoSliceMut::new(&mut buf)];
            let msg = recvmsg::<()>(
                self.socket.as_raw_fd(),
                &mut iov,
                Some(&mut cmsg_buf),
                MsgFlags::empty(),
            )?;
            let mut fds = Vec::new();
            for cmsg in msg.cmsgs()? {
                if let ControlMessageOwned::ScmRights(scm_fds) = cmsg {
                    fds.extend_from_slice(&scm_fds);
                }
            }
            (msg.bytes, fds)
        };

        if bytes == 0 {
            // Zero bytes from recvmsg on a connected socket is the kernel's
            // signal for clean EOF, not "empty message".
            return Err(PluginError::Disconnected);
        }

        if !unexpected_fds.is_empty() {
            for fd in unexpected_fds {
                // SAFETY: the kernel handed us this fd via SCM_RIGHTS; we own
                // it and must close it to avoid leaking. Wrapping in OwnedFd
                // and letting it drop is the close.
                let _owned = unsafe { OwnedFd::from_raw_fd(fd) };
            }
            return Err(PluginError::ProtocolViolation(
                "server message carried an fd",
            ));
        }

        let payload = &buf[..bytes];
        let event = ServerMessage::decode(payload)?;
        Ok(event)
    }
}
