use std::{
    io::{IoSlice, IoSliceMut},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
        unix::net::UnixStream,
    },
};

use nix::sys::socket::{ControlMessageOwned, MsgFlags, recvmsg, sendmsg};

use veiland_protocol::{
    BufferReleased, ClientMessage, Configure, PROTOCOL_VERSION, ServerMessage, read_version,
    write_version,
};

use super::HostError;

pub struct HostConnection {
    socket: UnixStream,
}

impl HostConnection {
    pub fn from_fd(fd: OwnedFd) -> Self {
        Self {
            socket: UnixStream::from(fd),
        }
    }

    pub fn handshake(&mut self) -> Result<(), HostError> {
        // 1. Read plugin's version first (server side reads, then writes).
        let mut version_in = [0u8; 4];
        let mut iov = [IoSliceMut::new(&mut version_in)];
        let msg = recvmsg::<()>(self.socket.as_raw_fd(), &mut iov, None, MsgFlags::empty())?;

        match msg.bytes {
            0 => return Err(HostError::PluginDisconnected),
            4 => {}
            _ => {
                return Err(HostError::ProtocolViolation(
                    "handshake request was not 4 bytes",
                ));
            }
        }

        let plugin_version = read_version(&version_in)?;

        // 2. On mismatch, return without writing — the spec's "close without
        //    replying" signal. The caller drops us, which closes the socket.
        if plugin_version != PROTOCOL_VERSION {
            return Err(HostError::VersionMismatch {
                host: PROTOCOL_VERSION,
                plugin: plugin_version,
            });
        }

        // 3. Match: echo our version back to complete the handshake.
        let mut version_out = Vec::new();
        write_version(&mut version_out);
        sendmsg::<()>(
            self.socket.as_raw_fd(),
            &[IoSlice::new(&version_out)],
            &[],
            MsgFlags::empty(),
            None,
        )?;

        Ok(())
    }

    /// Encode and send one `ServerMessage`. All v1 server messages are
    /// fd-less, so this is the shared implementation behind every public
    /// `send_*` method.
    fn send_message_no_fd(&mut self, msg: &ServerMessage) -> Result<(), HostError> {
        let mut buf = Vec::new();
        msg.encode(&mut buf);
        sendmsg::<()>(
            self.socket.as_raw_fd(),
            &[IoSlice::new(&buf)],
            &[],
            MsgFlags::empty(),
            None,
        )?;
        Ok(())
    }

    /// Send `Configure`. See `docs/protocol.md` §7.1.
    pub fn send_configure(&mut self, c: Configure) -> Result<(), HostError> {
        self.send_message_no_fd(&ServerMessage::Configure(c))
    }

    /// Cue the plugin to render the next frame. See §7.2.
    pub fn send_frame_done(&mut self) -> Result<(), HostError> {
        self.send_message_no_fd(&ServerMessage::FrameDone)
    }

    /// Tell the plugin we're done sampling buffer `id`. See §7.3.
    /// M3 single-buffer code path doesn't use this; M5 buffer pool will.
    #[allow(dead_code)]
    pub fn send_buffer_released(&mut self, id: u32) -> Result<(), HostError> {
        self.send_message_no_fd(&ServerMessage::BufferReleased(BufferReleased { id }))
    }

    /// Ask the plugin to exit. See §7.4. Used by the unlock cleanup
    /// path in step 7.
    #[allow(dead_code)]
    pub fn send_shutdown(&mut self) -> Result<(), HostError> {
        self.send_message_no_fd(&ServerMessage::Shutdown)
    }

    /// Block until one `ClientMessage` arrives, or the plugin disconnects.
    /// `Buffer` messages carry exactly one fd via `SCM_RIGHTS`; all other
    /// variants carry none. Any deviation is a `ProtocolViolation` and the
    /// caller should treat the plugin as dead.
    pub fn recv_message(&mut self) -> Result<(ClientMessage, Option<OwnedFd>), HostError> {
        let mut buf = [0u8; 64 * 1024];
        let mut cmsg_buf = nix::cmsg_space!(RawFd);

        // Pull bytes + fds out, then drop iov/msg so we can re-read buf
        // for decoding without a borrow conflict. Wrap fds in OwnedFd
        // immediately so they can't leak — even the unexpected ones.
        let (bytes, received_fds): (usize, Vec<OwnedFd>) = {
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
                    for raw in scm_fds {
                        // SAFETY: the kernel handed us this fd via SCM_RIGHTS;
                        // we are the sole owner and must close it. Wrapping in
                        // OwnedFd makes Drop handle that for us, even on the
                        // error paths below.
                        fds.push(unsafe { OwnedFd::from_raw_fd(raw) });
                    }
                }
            }
            (msg.bytes, fds)
        };

        if bytes == 0 {
            return Err(HostError::PluginDisconnected);
        }
        let message = ClientMessage::decode(&buf[..bytes])?;

        // Variant ↔ fd-count invariant. Buffer carries one fd; nothing
        // else carries any. Any deviation is a wire-level violation the
        // codec couldn't catch.
        match (&message, received_fds.len()) {
            (ClientMessage::Buffer(_), 1) => {
                let mut fds = received_fds;
                let fd = fds.pop().expect("len == 1 just checked");
                Ok((message, Some(fd)))
            }
            (ClientMessage::Buffer(_), 0) => {
                Err(HostError::ProtocolViolation("Buffer message without fd"))
            }
            (ClientMessage::Buffer(_), _) => Err(HostError::ProtocolViolation(
                "Buffer message with extra fds",
            )),
            (_, 0) => Ok((message, None)),
            (_, _) => Err(HostError::ProtocolViolation(
                "fd attached to fd-less message",
            )),
        }
    }

    pub fn as_fd(&self) -> BorrowedFd<'_> {
        self.socket.as_fd()
    }
}

#[cfg(test)]
mod tests {
    //! End-to-end tests of `HostConnection` against a real `socketpair`.
    //!
    //! One side is `HostConnection`; the other is a "fake plugin" driven
    //! by ad-hoc `plugin_send_*` / `plugin_recv_*` helpers that read and
    //! write raw bytes. Mirror of `veiland-plugin/tests/loopback.rs`,
    //! with the roles swapped.
    //!
    //! Each test spawns a thread for the *host* (which uses
    //! `HostConnection`'s blocking calls) and drives the plugin side
    //! from the main thread.

    use super::*;
    use std::thread;

    use nix::sys::socket::{AddressFamily, ControlMessage, SockFlag, SockType, socketpair};

    use veiland_protocol::{Buffer, Fourcc, Hello, Modifier};

    // ---- Fake-plugin helpers --------------------------------------------------

    /// Write a u32 as four LE bytes — plugin's handshake request, or
    /// a deliberately-wrong version for the mismatch test.
    fn plugin_send_version(fd: RawFd, version: u32) {
        let bytes = version.to_le_bytes();
        sendmsg::<()>(fd, &[IoSlice::new(&bytes)], &[], MsgFlags::empty(), None)
            .expect("plugin sendmsg version");
    }

    /// Read four LE bytes — host's handshake reply. Returns None if
    /// the host closed without replying (the spec's mismatch signal).
    fn plugin_recv_version(fd: RawFd) -> Option<u32> {
        let mut buf = [0u8; 4];
        let mut iov = [IoSliceMut::new(&mut buf)];
        let msg =
            recvmsg::<()>(fd, &mut iov, None, MsgFlags::empty()).expect("plugin recvmsg version");
        if msg.bytes == 0 {
            None
        } else {
            assert_eq!(msg.bytes, 4, "plugin expected 4 version bytes");
            Some(read_version(&buf).expect("read_version"))
        }
    }

    /// Send one `ClientMessage` to the host, no fd attached.
    fn plugin_send_client_message(fd: RawFd, msg: &ClientMessage) {
        let mut buf = Vec::new();
        msg.encode(&mut buf).expect("encode ClientMessage");
        sendmsg::<()>(fd, &[IoSlice::new(&buf)], &[], MsgFlags::empty(), None)
            .expect("plugin sendmsg client");
    }

    /// Send one `ClientMessage` to the host with one fd via SCM_RIGHTS.
    /// Used for the Buffer test and the "fd attached to fd-less message"
    /// violation test.
    fn plugin_send_client_message_with_fd(fd: RawFd, msg: &ClientMessage, attached: RawFd) {
        let mut buf = Vec::new();
        msg.encode(&mut buf).expect("encode ClientMessage");
        let fds = [attached];
        let cmsgs = [ControlMessage::ScmRights(&fds)];
        sendmsg::<()>(fd, &[IoSlice::new(&buf)], &cmsgs, MsgFlags::empty(), None)
            .expect("plugin sendmsg client-with-fd");
    }

    // ---- Fixture --------------------------------------------------------------

    /// Create a socketpair, return (host_fd, plugin_fd). Convention:
    /// first is the host's end.
    fn pair() -> (OwnedFd, OwnedFd) {
        socketpair(
            AddressFamily::Unix,
            SockType::SeqPacket,
            None,
            SockFlag::SOCK_CLOEXEC,
        )
        .expect("socketpair")
    }

    // ---- Tests ---------------------------------------------------------------

    #[test]
    fn handshake_roundtrip() {
        let (host_fd, plugin_fd) = pair();

        let host = thread::spawn(move || {
            let mut conn = HostConnection::from_fd(host_fd);
            conn.handshake().expect("host handshake");
        });

        let plugin_raw = plugin_fd.as_raw_fd();
        plugin_send_version(plugin_raw, PROTOCOL_VERSION);
        let host_version =
            plugin_recv_version(plugin_raw).expect("host should reply on version match");
        assert_eq!(host_version, PROTOCOL_VERSION);

        host.join().expect("host thread");
    }

    #[test]
    fn handshake_version_mismatch() {
        let (host_fd, plugin_fd) = pair();

        let host = thread::spawn(move || {
            let mut conn = HostConnection::from_fd(host_fd);
            let err = conn.handshake().expect_err("host handshake should fail");
            match err {
                HostError::VersionMismatch { host, plugin } => {
                    assert_eq!(host, PROTOCOL_VERSION);
                    assert_eq!(plugin, 0x9999_9999);
                }
                other => panic!("expected VersionMismatch, got {:?}", other),
            }
            // Dropping conn closes the socket — the spec's "rejected" signal.
        });

        let plugin_raw = plugin_fd.as_raw_fd();
        plugin_send_version(plugin_raw, 0x9999_9999);
        // Host must close without replying.
        assert!(
            plugin_recv_version(plugin_raw).is_none(),
            "host must close socket without replying on mismatch"
        );

        host.join().expect("host thread");
    }

    /// Drive a full handshake then send Hello; host should receive it.
    #[test]
    fn recv_hello() {
        let (host_fd, plugin_fd) = pair();

        let host = thread::spawn(move || {
            let mut conn = HostConnection::from_fd(host_fd);
            conn.handshake().expect("handshake");
            let (msg, fd) = conn.recv_message().expect("recv");
            assert!(fd.is_none(), "Hello must arrive without an fd");
            match msg {
                ClientMessage::Hello(h) => {
                    assert_eq!(h.plugin_name, "test");
                    assert_eq!(h.plugin_version, "0.1");
                }
                other => panic!("expected Hello, got {:?}", other),
            }
        });

        let plugin_raw = plugin_fd.as_raw_fd();
        plugin_send_version(plugin_raw, PROTOCOL_VERSION);
        let _ = plugin_recv_version(plugin_raw).expect("host version");
        plugin_send_client_message(
            plugin_raw,
            &ClientMessage::Hello(Hello {
                plugin_name: "test".into(),
                plugin_version: "0.1".into(),
            }),
        );

        host.join().expect("host thread");
    }

    /// Send a Buffer with an fd attached; host should see both metadata
    /// and the fd. Use /dev/null as a stand-in fd — any open fd suffices
    /// for testing the SCM_RIGHTS plumbing.
    #[test]
    fn recv_buffer_with_fd() {
        let (host_fd, plugin_fd) = pair();

        let host = thread::spawn(move || {
            let mut conn = HostConnection::from_fd(host_fd);
            conn.handshake().expect("handshake");
            let (msg, fd) = conn.recv_message().expect("recv");
            let fd = fd.expect("Buffer must carry an fd");
            match msg {
                ClientMessage::Buffer(b) => {
                    assert_eq!(b.id, 7);
                    assert_eq!(b.width, 1920);
                    assert_eq!(b.height, 1080);
                }
                other => panic!("expected Buffer, got {:?}", other),
            }
            // fd drops here; OwnedFd closes the dup that the kernel made.
            drop(fd);
        });

        let plugin_raw = plugin_fd.as_raw_fd();
        plugin_send_version(plugin_raw, PROTOCOL_VERSION);
        let _ = plugin_recv_version(plugin_raw).expect("host version");

        // Open /dev/null as a throwaway fd to pass via SCM_RIGHTS.
        let devnull = std::fs::File::open("/dev/null").expect("open /dev/null");
        let buf = Buffer {
            id: 7,
            width: 1920,
            height: 1080,
            format: Fourcc::ARGB8888,
            modifier: Modifier(0), // DRM LINEAR
            stride: 1920 * 4,
            offset: 0,
        };
        plugin_send_client_message_with_fd(
            plugin_raw,
            &ClientMessage::Buffer(buf),
            devnull.as_raw_fd(),
        );

        host.join().expect("host thread");
    }

    /// Send a Hello with an fd attached. Host must reject as
    /// ProtocolViolation and close (well, drop) the unexpected fd.
    #[test]
    fn recv_hello_with_fd_is_violation() {
        let (host_fd, plugin_fd) = pair();

        let host = thread::spawn(move || {
            let mut conn = HostConnection::from_fd(host_fd);
            conn.handshake().expect("handshake");
            let err = conn.recv_message().expect_err("must fail");
            match err {
                HostError::ProtocolViolation(_) => {}
                other => panic!("expected ProtocolViolation, got {:?}", other),
            }
        });

        let plugin_raw = plugin_fd.as_raw_fd();
        plugin_send_version(plugin_raw, PROTOCOL_VERSION);
        let _ = plugin_recv_version(plugin_raw).expect("host version");

        let devnull = std::fs::File::open("/dev/null").expect("open /dev/null");
        plugin_send_client_message_with_fd(
            plugin_raw,
            &ClientMessage::Hello(Hello {
                plugin_name: "test".into(),
                plugin_version: "0.1".into(),
            }),
            devnull.as_raw_fd(),
        );

        host.join().expect("host thread");
    }
}
