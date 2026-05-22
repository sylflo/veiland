use std::{
    io::{IoSlice, IoSliceMut},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd},
        unix::net::UnixStream,
    },
};

use nix::sys::socket::{ControlMessageOwned, MsgFlags, recvmsg, sendmsg};

use veiland_protocol::{
    BufferReleased, ClientMessage, Configure, HostCapabilities, PROTOCOL_VERSION, ServerMessage,
    read_version, write_host_capabilities, write_version,
};
// HOST_CAP_FENCE_FD is only consumed by tests right now; main.rs computes
// the capability bits itself and threads them in via from_fd. Pulled into
// scope in the #[cfg(test)] module below.

use super::HostError;

/// SCM_RIGHTS fds extracted from a single `ClientMessage`. The variant
/// matches what the protocol says the message tag can carry; mismatches
/// (a `Buffer` with no dmabuf, a `Hello` with an fd, etc.) are caught
/// in `HostConnection::recv_message` and returned as protocol violations.
///
/// Fds are wrapped in `OwnedFd` so they close on drop — `handle_message`
/// can drop the variant after use and we never leak.
#[derive(Debug)]
pub enum ReceivedFds {
    /// No SCM_RIGHTS fds attached. `Hello`, `BufferDestroy`.
    None,
    /// `Buffer` message. Dmabuf is always present; fence is `Some` on the
    /// M5a fast path, `None` on the slow path. See `docs/protocol.md` §6.2.
    Buffer {
        dmabuf: OwnedFd,
        fence: Option<OwnedFd>,
    },
}

pub struct HostConnection {
    socket: UnixStream,
    host_capabilities: HostCapabilities,
}

impl HostConnection {
    pub fn from_fd(fd: OwnedFd, host_capabilities: HostCapabilities) -> Self {
        Self {
            socket: UnixStream::from(fd),
            host_capabilities,
        }
    }

    pub fn handshake(&mut self) -> Result<(), HostError> {
        // 1. Read plugin's version first (server side reads, then writes).
        let mut version_in = [0u8; 4];
        let mut iov = [IoSliceMut::new(&mut version_in)];
        let msg = recvmsg::<()>(self.socket.as_raw_fd(), &mut iov, None, MsgFlags::empty())?;

        // TODO magic number waht do they mean ?
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

        // 4. Send our capability bitfield as a second packet. See protocol.md §5.1.
        let mut caps_out = Vec::new();
        write_host_capabilities(&mut caps_out, self.host_capabilities);
        sendmsg::<()>(
            self.socket.as_raw_fd(),
            &[IoSlice::new(&caps_out)],
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
    /// `Buffer` carries 1 fd (slow path) or 2 fds (fast path: dmabuf + fence);
    /// `Hello` and `BufferDestroy` carry no fds. Any other combination is a
    /// `ProtocolViolation` and the caller should treat the plugin as dead.
    pub fn recv_message(&mut self) -> Result<(ClientMessage, ReceivedFds), HostError> {
        let mut buf = [0u8; 64 * 1024];
        // Reserve cmsg space for an ScmRights with up to 2 fds (dmabuf +
        // optional fence). `cmsg_space!` uses the payload type's size to
        // pick the reservation; `[RawFd; 2]` is the "two raw fds" payload.
        let mut cmsg_buf = nix::cmsg_space!([RawFd; 2]);

        // Pull bytes + fds out, then drop iov/msg so we can re-read buf
        // for decoding without a borrow conflict. Wrap fds in OwnedFd
        // immediately so they can't leak — even the unexpected ones.
        let (bytes, received_fds): (usize, Vec<OwnedFd>) = {
            let mut iov = [IoSliceMut::new(&mut buf)];
            // ENOBUFS here means "sender attached more cmsg data than our
            // fixed-size cmsg_buf can hold." For our protocol that maps
            // 1:1 to "plugin attached more than 2 fds to a single message,"
            // which the spec forbids. Translate to ProtocolViolation so
            // the caller treats it the same as any other fd-count violation
            // (plugin death) rather than as an opaque Nix error.
            let msg = match recvmsg::<()>(
                self.socket.as_raw_fd(),
                &mut iov,
                Some(&mut cmsg_buf),
                MsgFlags::empty(),
            ) {
                Ok(m) => m,
                Err(nix::errno::Errno::ENOBUFS) => {
                    return Err(HostError::ProtocolViolation(
                        "message attached more fds than the protocol allows (max 2 on Buffer, 0 elsewhere)",
                    ));
                }
                Err(e) => return Err(e.into()),
            };
            // `cmsgs()` returns ENOBUFS when the kernel set MSG_CTRUNC,
            // i.e. the sender attached more cmsg data than our buffer
            // could hold. Same protocol cause as ENOBUFS from recvmsg
            // itself (Linux can surface the truncation at either site);
            // same translation. Note that the kernel may have *partially*
            // delivered fds before truncating — the cmsg iterator becomes
            // unsafe to use after MSG_CTRUNC, so we drop the message
            // without trying to recover any of them. The kernel closes
            // the rest server-side.
            let cmsgs_iter = match msg.cmsgs() {
                Ok(it) => it,
                Err(nix::errno::Errno::ENOBUFS) => {
                    return Err(HostError::ProtocolViolation(
                        "message attached more fds than the protocol allows (max 2 on Buffer, 0 elsewhere)",
                    ));
                }
                Err(e) => return Err(e.into()),
            };
            let mut fds = Vec::new();
            for cmsg in cmsgs_iter {
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

        // Variant ↔ fd-count invariant. Buffer carries 1 or 2 fds (dmabuf,
        // optional fence); everything else carries none. Any deviation is a
        // wire-level violation the codec couldn't catch. Order in the cmsg
        // is dmabuf first, fence second (protocol.md §6.2). All fds are
        // already wrapped in OwnedFd above, so the violation arms drop
        // them automatically — no manual close needed.
        let fds = match (&message, received_fds.len()) {
            (ClientMessage::Buffer(_), 1) => {
                let mut v = received_fds;
                let dmabuf = v.pop().expect("len == 1 just checked");
                ReceivedFds::Buffer {
                    dmabuf,
                    fence: None,
                }
            }
            (ClientMessage::Buffer(_), 2) => {
                let mut v = received_fds.into_iter();
                let dmabuf = v.next().expect("len == 2 just checked");
                let fence = v.next().expect("len == 2 just checked");
                ReceivedFds::Buffer {
                    dmabuf,
                    fence: Some(fence),
                }
            }
            (ClientMessage::Buffer(_), 0) => {
                return Err(HostError::ProtocolViolation("Buffer message without fd"));
            }
            (ClientMessage::Buffer(_), _) => {
                return Err(HostError::ProtocolViolation(
                    "Buffer message with too many fds (expected 1 or 2)",
                ));
            }
            (_, 0) => ReceivedFds::None,
            (_, _) => {
                return Err(HostError::ProtocolViolation(
                    "fd attached to fd-less message",
                ));
            }
        };
        Ok((message, fds))
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

    use veiland_protocol::{Buffer, Fourcc, HOST_CAP_FENCE_FD, Hello, Modifier};

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

    /// Read the four LE bytes the host sends as its capability word,
    /// immediately after `server_version`. See protocol.md §5.1.
    fn plugin_recv_host_capabilities(fd: RawFd) -> HostCapabilities {
        use veiland_protocol::read_host_capabilities;
        let mut buf = [0u8; 4];
        let mut iov = [IoSliceMut::new(&mut buf)];
        let msg = recvmsg::<()>(fd, &mut iov, None, MsgFlags::empty())
            .expect("plugin recvmsg host_capabilities");
        assert_eq!(msg.bytes, 4, "plugin expected 4 host_capabilities bytes");
        read_host_capabilities(&buf).expect("read_host_capabilities")
    }

    /// Send one `ClientMessage` to the host, no fd attached.
    fn plugin_send_client_message(fd: RawFd, msg: &ClientMessage) {
        let mut buf = Vec::new();
        msg.encode(&mut buf).expect("encode ClientMessage");
        sendmsg::<()>(fd, &[IoSlice::new(&buf)], &[], MsgFlags::empty(), None)
            .expect("plugin sendmsg client");
    }

    /// Send one `ClientMessage` to the host with one fd via SCM_RIGHTS.
    /// Used for the slow-path Buffer test and the "fd attached to
    /// fd-less message" violation test.
    fn plugin_send_client_message_with_fd(fd: RawFd, msg: &ClientMessage, attached: RawFd) {
        plugin_send_client_message_with_fds(fd, msg, &[attached]);
    }

    /// Send one `ClientMessage` to the host with N fds via SCM_RIGHTS.
    /// Used for the fast-path Buffer test (2 fds) and the too-many-fds
    /// violation test (3 fds).
    fn plugin_send_client_message_with_fds(fd: RawFd, msg: &ClientMessage, attached: &[RawFd]) {
        let mut buf = Vec::new();
        msg.encode(&mut buf).expect("encode ClientMessage");
        let cmsgs = [ControlMessage::ScmRights(attached)];
        sendmsg::<()>(fd, &[IoSlice::new(&buf)], &cmsgs, MsgFlags::empty(), None)
            .expect("plugin sendmsg client-with-fds");
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
            let mut conn = HostConnection::from_fd(host_fd, HOST_CAP_FENCE_FD);
            conn.handshake().expect("host handshake");
        });

        let plugin_raw = plugin_fd.as_raw_fd();
        plugin_send_version(plugin_raw, PROTOCOL_VERSION);
        let host_version =
            plugin_recv_version(plugin_raw).expect("host should reply on version match");
        assert_eq!(host_version, PROTOCOL_VERSION);
        let caps = plugin_recv_host_capabilities(plugin_raw);
        assert_eq!(caps, HOST_CAP_FENCE_FD);

        host.join().expect("host thread");
    }

    #[test]
    fn handshake_version_mismatch() {
        let (host_fd, plugin_fd) = pair();

        let host = thread::spawn(move || {
            let mut conn = HostConnection::from_fd(host_fd, HOST_CAP_FENCE_FD);
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
            let mut conn = HostConnection::from_fd(host_fd, HOST_CAP_FENCE_FD);
            conn.handshake().expect("handshake");
            let (msg, fds) = conn.recv_message().expect("recv");
            assert!(
                matches!(fds, ReceivedFds::None),
                "Hello must arrive without fds"
            );
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
        let _ = plugin_recv_host_capabilities(plugin_raw);
        plugin_send_client_message(
            plugin_raw,
            &ClientMessage::Hello(Hello {
                plugin_name: "test".into(),
                plugin_version: "0.1".into(),
            }),
        );

        host.join().expect("host thread");
    }

    /// Slow path: Buffer with 1 fd (dmabuf only). Host should see the
    /// dmabuf in `ReceivedFds::Buffer` and `fence` = None.
    #[test]
    fn recv_buffer_with_fd() {
        let (host_fd, plugin_fd) = pair();

        let host = thread::spawn(move || {
            let mut conn = HostConnection::from_fd(host_fd, HOST_CAP_FENCE_FD);
            conn.handshake().expect("handshake");
            let (msg, fds) = conn.recv_message().expect("recv");
            let (dmabuf, fence) = match fds {
                ReceivedFds::Buffer { dmabuf, fence } => (dmabuf, fence),
                other => panic!("expected ReceivedFds::Buffer, got {:?}", other),
            };
            assert!(fence.is_none(), "slow-path Buffer must not carry a fence");
            match msg {
                ClientMessage::Buffer(b) => {
                    assert_eq!(b.id, 7);
                    assert_eq!(b.width, 1920);
                    assert_eq!(b.height, 1080);
                }
                other => panic!("expected Buffer, got {:?}", other),
            }
            // OwnedFd drops here, closing the dup the kernel made.
            drop(dmabuf);
        });

        let plugin_raw = plugin_fd.as_raw_fd();
        plugin_send_version(plugin_raw, PROTOCOL_VERSION);
        let _ = plugin_recv_version(plugin_raw).expect("host version");
        let _ = plugin_recv_host_capabilities(plugin_raw);

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

    /// Fast path: Buffer with 2 fds (dmabuf + fence). Host should see
    /// both in `ReceivedFds::Buffer { dmabuf, fence: Some(_) }`. Order
    /// matters — dmabuf first, fence second.
    #[test]
    fn recv_buffer_with_two_fds() {
        let (host_fd, plugin_fd) = pair();

        let host = thread::spawn(move || {
            let mut conn = HostConnection::from_fd(host_fd, HOST_CAP_FENCE_FD);
            conn.handshake().expect("handshake");
            let (msg, fds) = conn.recv_message().expect("recv");
            match (msg, fds) {
                (
                    ClientMessage::Buffer(b),
                    ReceivedFds::Buffer {
                        dmabuf,
                        fence: Some(fence),
                    },
                ) => {
                    assert_eq!(b.id, 11);
                    // Sanity: distinct fd integers on the receiving side
                    // (kernel reallocates per cmsg fd).
                    assert!(dmabuf.as_raw_fd() >= 0);
                    assert!(fence.as_raw_fd() >= 0);
                    assert_ne!(dmabuf.as_raw_fd(), fence.as_raw_fd());
                }
                (msg, fds) => panic!(
                    "expected fast-path Buffer with 2 fds, got msg={:?} fds={:?}",
                    msg, fds
                ),
            }
        });

        let plugin_raw = plugin_fd.as_raw_fd();
        plugin_send_version(plugin_raw, PROTOCOL_VERSION);
        let _ = plugin_recv_version(plugin_raw).expect("host version");
        let _ = plugin_recv_host_capabilities(plugin_raw);

        // Two throwaway fds standing in for dmabuf + fence.
        let dmabuf_file = std::fs::File::open("/dev/null").expect("open /dev/null (dmabuf)");
        let fence_file = std::fs::File::open("/dev/null").expect("open /dev/null (fence)");
        let buf = Buffer {
            id: 11,
            width: 64,
            height: 64,
            format: Fourcc::ARGB8888,
            modifier: Modifier(0),
            stride: 256,
            offset: 0,
        };
        plugin_send_client_message_with_fds(
            plugin_raw,
            &ClientMessage::Buffer(buf),
            &[dmabuf_file.as_raw_fd(), fence_file.as_raw_fd()],
        );

        host.join().expect("host thread");
    }

    /// Buffer with 3 fds → protocol violation. The wire spec allows 1
    /// (slow path) or 2 (fast path); anything else is a plugin bug.
    #[test]
    fn recv_buffer_with_too_many_fds_is_violation() {
        let (host_fd, plugin_fd) = pair();

        let host = thread::spawn(move || {
            let mut conn = HostConnection::from_fd(host_fd, HOST_CAP_FENCE_FD);
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
        let _ = plugin_recv_host_capabilities(plugin_raw);

        let f1 = std::fs::File::open("/dev/null").expect("open /dev/null");
        let f2 = std::fs::File::open("/dev/null").expect("open /dev/null");
        let f3 = std::fs::File::open("/dev/null").expect("open /dev/null");
        let buf = Buffer {
            id: 13,
            width: 64,
            height: 64,
            format: Fourcc::ARGB8888,
            modifier: Modifier(0),
            stride: 256,
            offset: 0,
        };
        plugin_send_client_message_with_fds(
            plugin_raw,
            &ClientMessage::Buffer(buf),
            &[f1.as_raw_fd(), f2.as_raw_fd(), f3.as_raw_fd()],
        );

        host.join().expect("host thread");
    }

    /// Send a Hello with an fd attached. Host must reject as
    /// ProtocolViolation and close (well, drop) the unexpected fd.
    #[test]
    fn recv_hello_with_fd_is_violation() {
        let (host_fd, plugin_fd) = pair();

        let host = thread::spawn(move || {
            let mut conn = HostConnection::from_fd(host_fd, HOST_CAP_FENCE_FD);
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
        let _ = plugin_recv_host_capabilities(plugin_raw);

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
