// SPDX-License-Identifier: GPL-3.0-or-later

//! End-to-end test of the `Connection` type against a real `socketpair`.
//!
//! One side is wrapped as a `veiland_plugin::Connection`; the other stays
//! as a raw `OwnedFd` driven by ad-hoc "fake host" helpers (`host_recv_*`,
//! `host_send_*`) that read/write bytes directly. When the host crate
//! exists, the fake-host helpers here are the seed for its production
//! recv/send code.
//!
//! Each test spawns a thread for the plugin side so the host side can
//! drive the protocol from the main thread without deadlocking on the
//! plugin's blocking `recvmsg` calls.

use std::io::{IoSlice, IoSliceMut};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::thread;

use nix::sys::socket::{
    AddressFamily, ControlMessage, ControlMessageOwned, MsgFlags, SockFlag, SockType, recvmsg,
    sendmsg, socketpair,
};

use veiland_plugin::{Connection, PluginError};
use veiland_protocol::{
    Buffer, ClientMessage, Configure, Fourcc, HOST_CAP_FENCE_FD, Hello, HostCapabilities, Modifier,
    PROTOCOL_VERSION, ServerMessage, read_version, write_host_capabilities,
};

// ---- Fake-host helpers ------------------------------------------------------

/// Read four LE bytes and parse as a protocol version. Blocks.
fn host_recv_version(fd: RawFd) -> u32 {
    let mut buf = [0u8; 4];
    let mut iov = [IoSliceMut::new(&mut buf)];
    let msg = recvmsg::<()>(fd, &mut iov, None, MsgFlags::empty()).expect("host recvmsg version");
    assert_eq!(msg.bytes, 4, "host expected 4 version bytes");
    read_version(&buf).expect("host read_version")
}

/// Write a u32 as four LE bytes (used to send the version reply).
fn host_send_version(fd: RawFd, version: u32) {
    let mut buf = Vec::new();
    write_version_value(&mut buf, version);
    sendmsg::<()>(fd, &[IoSlice::new(&buf)], &[], MsgFlags::empty(), None)
        .expect("host sendmsg version");
}

/// Write a specific u32 (not just PROTOCOL_VERSION) — for the
/// version-mismatch test. Mirrors `veiland_protocol::write_version`
/// but with a chosen value.
fn write_version_value(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

/// Send the host's capability word as the second handshake packet,
/// immediately after `host_send_version`. See protocol.md §5.1.
fn host_send_host_capabilities(fd: RawFd, caps: HostCapabilities) {
    let mut buf = Vec::new();
    write_host_capabilities(&mut buf, caps);
    sendmsg::<()>(fd, &[IoSlice::new(&buf)], &[], MsgFlags::empty(), None)
        .expect("host sendmsg host_capabilities");
}

/// Receive one `ClientMessage` from the plugin. Returns the message and
/// any fds that arrived via `SCM_RIGHTS`.
fn host_recv_client_message(fd: RawFd) -> (ClientMessage, Vec<OwnedFd>) {
    let mut buf = [0u8; 64 * 1024];
    let mut cmsg_buf = nix::cmsg_space!(RawFd);

    let (bytes, fds) = {
        let mut iov = [IoSliceMut::new(&mut buf)];
        let msg = recvmsg::<()>(fd, &mut iov, Some(&mut cmsg_buf), MsgFlags::empty())
            .expect("host recvmsg client");
        let mut fds: Vec<OwnedFd> = Vec::new();
        for cmsg in msg.cmsgs().expect("host cmsgs") {
            if let ControlMessageOwned::ScmRights(raw_fds) = cmsg {
                for raw in raw_fds {
                    // SAFETY: kernel handed us this fd via SCM_RIGHTS; we own it.
                    fds.push(unsafe { OwnedFd::from_raw_fd(raw) });
                }
            }
        }
        (msg.bytes, fds)
    };

    let cm = ClientMessage::decode(&buf[..bytes]).expect("host decode ClientMessage");
    (cm, fds)
}

/// Send a `ServerMessage` to the plugin (no fd).
fn host_send_server_message(fd: RawFd, msg: &ServerMessage) {
    let mut buf = Vec::new();
    msg.encode(&mut buf);
    sendmsg::<()>(fd, &[IoSlice::new(&buf)], &[], MsgFlags::empty(), None)
        .expect("host sendmsg server");
}

/// Send a server-direction tag with a single fd attached. Used to test the
/// "server message carried an fd → protocol violation" path. We construct a
/// `ServerMessage::FrameDone` (smallest payload) and stuff one fd in the
/// cmsg.
fn host_send_server_message_with_fd(fd: RawFd, msg: &ServerMessage, attached: RawFd) {
    let mut buf = Vec::new();
    msg.encode(&mut buf);
    let fds = [attached];
    let cmsgs = [ControlMessage::ScmRights(&fds)];
    sendmsg::<()>(fd, &[IoSlice::new(&buf)], &cmsgs, MsgFlags::empty(), None)
        .expect("host sendmsg server-with-fd");
}

// ---- Test fixture -----------------------------------------------------------

/// Create a socketpair, return (plugin_fd, host_fd) as OwnedFds.
fn pair() -> (OwnedFd, OwnedFd) {
    socketpair(
        AddressFamily::Unix,
        SockType::SeqPacket,
        None,
        SockFlag::SOCK_CLOEXEC,
    )
    .expect("socketpair")
}

// ---- Tests ------------------------------------------------------------------

#[test]
fn handshake_roundtrip() {
    let (plugin_fd, host_fd) = pair();

    let plugin = thread::spawn(move || {
        let mut conn = Connection::from_fd(plugin_fd);
        conn.handshake().expect("plugin handshake");
        // After handshake, the plugin should have stashed the host's caps.
        assert_eq!(conn.host_capabilities(), HOST_CAP_FENCE_FD);
        assert!(conn.host_supports_fence_fd());
    });

    let host_raw = host_fd.as_raw_fd();
    let plugin_version = host_recv_version(host_raw);
    assert_eq!(plugin_version, PROTOCOL_VERSION);
    host_send_version(host_raw, PROTOCOL_VERSION);
    host_send_host_capabilities(host_raw, HOST_CAP_FENCE_FD);

    plugin.join().expect("plugin thread");
}

#[test]
fn handshake_version_mismatch() {
    let (plugin_fd, host_fd) = pair();

    let plugin = thread::spawn(move || {
        let mut conn = Connection::from_fd(plugin_fd);
        let err = conn.handshake().expect_err("plugin handshake should fail");
        match err {
            PluginError::VersionMismatch { plugin, host } => {
                assert_eq!(plugin, PROTOCOL_VERSION);
                assert_eq!(host, 0x9999_9999);
            }
            other => panic!("expected VersionMismatch, got {:?}", other),
        }
    });

    let host_raw = host_fd.as_raw_fd();
    let _ = host_recv_version(host_raw);
    host_send_version(host_raw, 0x9999_9999);

    plugin.join().expect("plugin thread");
}

#[test]
fn hello_roundtrip() {
    let (plugin_fd, host_fd) = pair();

    let plugin = thread::spawn(move || {
        let mut conn = Connection::from_fd(plugin_fd);
        conn.handshake().expect("plugin handshake");
        conn.send_hello("gradient", "0.1")
            .expect("plugin send_hello");
    });

    let host_raw = host_fd.as_raw_fd();
    let _ = host_recv_version(host_raw);
    host_send_version(host_raw, PROTOCOL_VERSION);
    host_send_host_capabilities(host_raw, HOST_CAP_FENCE_FD);

    let (msg, fds) = host_recv_client_message(host_raw);
    assert!(fds.is_empty(), "Hello must not carry fds");
    match msg {
        ClientMessage::Hello(Hello {
            plugin_name,
            plugin_version,
        }) => {
            assert_eq!(plugin_name, "gradient");
            assert_eq!(plugin_version, "0.1");
        }
        other => panic!("expected Hello, got {:?}", other),
    }

    plugin.join().expect("plugin thread");
}

#[test]
fn buffer_with_fd_arrives_with_one_cmsg_fd() {
    let (plugin_fd, host_fd) = pair();

    // We send the plugin's *own* fd as the dmabuf payload, just because it's
    // a valid open fd we have handy. The kernel will allocate a *fresh* fd on
    // the receiving side — asserting that gives us a sanity check that
    // SCM_RIGHTS actually fired (rather than the integer being sent in-band).
    let buffer = Buffer {
        id: 42,
        width: 64,
        height: 64,
        format: Fourcc::ARGB8888,
        modifier: Modifier(0),
        stride: 256,
        offset: 0,
    };
    let expected = buffer.clone();

    let plugin = thread::spawn(move || {
        let mut conn = Connection::from_fd(plugin_fd);
        conn.handshake().expect("plugin handshake");
        // Open /dev/null as a stand-in dmabuf fd. We need *some* open fd to
        // send; what's on the other side of it doesn't matter for the
        // protocol layer.
        let devnull = std::fs::File::open("/dev/null").expect("open /dev/null");
        let borrowed = std::os::fd::AsFd::as_fd(&devnull);
        conn.send_buffer(&buffer, borrowed, None)
            .expect("plugin send_buffer");
        // Keep devnull alive until after send_buffer returns.
        drop(devnull);
    });

    let host_raw = host_fd.as_raw_fd();
    let _ = host_recv_version(host_raw);
    host_send_version(host_raw, PROTOCOL_VERSION);
    host_send_host_capabilities(host_raw, HOST_CAP_FENCE_FD);

    let (msg, fds) = host_recv_client_message(host_raw);
    assert_eq!(fds.len(), 1, "Buffer must arrive with exactly one fd");

    // The received fd is a fresh integer (kernel reallocated it on this side).
    // We just assert it's >= 0 (a valid fd number); the precise value is up
    // to the kernel.
    let received_fd = fds[0].as_raw_fd();
    assert!(
        received_fd >= 0,
        "received fd should be valid, got {}",
        received_fd
    );

    match msg {
        ClientMessage::Buffer(b) => assert_eq!(b, expected),
        other => panic!("expected Buffer, got {:?}", other),
    }

    plugin.join().expect("plugin thread");
}

#[test]
fn buffer_with_fence_arrives_with_two_cmsg_fds() {
    let (plugin_fd, host_fd) = pair();

    // Same trick as the single-fd test: any two open fds suffice for the
    // SCM_RIGHTS plumbing — the host doesn't try to interpret them as a
    // real dmabuf or fence at this layer. Step 7's host-side change is
    // where the host starts caring about the fd count; this test pins
    // down that the plugin-side serialisation produces a 2-fd cmsg when
    // asked to.
    let buffer = Buffer {
        id: 42,
        width: 64,
        height: 64,
        format: Fourcc::ARGB8888,
        modifier: Modifier(0),
        stride: 256,
        offset: 0,
    };
    let expected = buffer.clone();

    let plugin = thread::spawn(move || {
        let mut conn = Connection::from_fd(plugin_fd);
        conn.handshake().expect("plugin handshake");
        let dmabuf = std::fs::File::open("/dev/null").expect("open /dev/null (dmabuf stand-in)");
        let fence = std::fs::File::open("/dev/null").expect("open /dev/null (fence stand-in)");
        let dmabuf_borrowed = std::os::fd::AsFd::as_fd(&dmabuf);
        let fence_borrowed = std::os::fd::AsFd::as_fd(&fence);
        conn.send_buffer(&buffer, dmabuf_borrowed, Some(fence_borrowed))
            .expect("plugin send_buffer (2 fds)");
        drop(dmabuf);
        drop(fence);
    });

    let host_raw = host_fd.as_raw_fd();
    let _ = host_recv_version(host_raw);
    host_send_version(host_raw, PROTOCOL_VERSION);
    host_send_host_capabilities(host_raw, HOST_CAP_FENCE_FD);

    let (msg, fds) = host_recv_client_message(host_raw);
    assert_eq!(
        fds.len(),
        2,
        "Buffer with Some(fence) must arrive with exactly two fds"
    );
    for (i, owned) in fds.iter().enumerate() {
        let raw = owned.as_raw_fd();
        assert!(raw >= 0, "fd {} should be valid, got {}", i, raw);
    }

    match msg {
        ClientMessage::Buffer(b) => assert_eq!(b, expected),
        other => panic!("expected Buffer, got {:?}", other),
    }

    plugin.join().expect("plugin thread");
}

#[test]
fn recv_event_configure_roundtrip() {
    let (plugin_fd, host_fd) = pair();

    let plugin = thread::spawn(move || {
        let mut conn = Connection::from_fd(plugin_fd);
        conn.handshake().expect("plugin handshake");
        let event = conn.recv_event().expect("plugin recv_event");
        match event {
            ServerMessage::Configure(c) => {
                assert_eq!(c.region_x, 100);
                assert_eq!(c.region_y, 200);
                assert_eq!(c.region_w, 800);
                assert_eq!(c.region_h, 600);
                assert_eq!(c.scale_120, 120);
            }
            other => panic!("expected Configure, got {:?}", other),
        }
    });

    let host_raw = host_fd.as_raw_fd();
    let _ = host_recv_version(host_raw);
    host_send_version(host_raw, PROTOCOL_VERSION);
    host_send_host_capabilities(host_raw, HOST_CAP_FENCE_FD);

    let configure = ServerMessage::Configure(Configure {
        region_x: 100,
        region_y: 200,
        region_w: 800,
        region_h: 600,
        scale_120: 120,
        time_unix_seconds: 1_700_000_000,
        time_tz_offset_seconds: 3600,
        output_name: "DP-1".to_string(),
    });
    host_send_server_message(host_raw, &configure);

    plugin.join().expect("plugin thread");
}

#[test]
fn recv_event_rejects_unexpected_fd() {
    let (plugin_fd, host_fd) = pair();

    let plugin = thread::spawn(move || {
        let mut conn = Connection::from_fd(plugin_fd);
        conn.handshake().expect("plugin handshake");
        let err = conn.recv_event().expect_err("recv_event must reject fd");
        match err {
            PluginError::ProtocolViolation(_) => { /* ok */ }
            other => panic!("expected ProtocolViolation, got {:?}", other),
        }
    });

    let host_raw = host_fd.as_raw_fd();
    let _ = host_recv_version(host_raw);
    host_send_version(host_raw, PROTOCOL_VERSION);
    host_send_host_capabilities(host_raw, HOST_CAP_FENCE_FD);

    // Send FrameDone (no fd in the spec), but attach an fd anyway. This is
    // the case that protocol §6.2 / §9 says is a protocol error.
    let devnull = std::fs::File::open("/dev/null").expect("open /dev/null");
    host_send_server_message_with_fd(host_raw, &ServerMessage::FrameDone, devnull.as_raw_fd());

    plugin.join().expect("plugin thread");
}

#[test]
fn recv_event_disconnect_on_clean_eof() {
    let (plugin_fd, host_fd) = pair();

    let plugin = thread::spawn(move || {
        let mut conn = Connection::from_fd(plugin_fd);
        conn.handshake().expect("plugin handshake");
        let err = conn.recv_event().expect_err("recv_event must report EOF");
        match err {
            PluginError::Disconnected => { /* ok */ }
            other => panic!("expected Disconnected, got {:?}", other),
        }
    });

    let host_raw = host_fd.as_raw_fd();
    let _ = host_recv_version(host_raw);
    host_send_version(host_raw, PROTOCOL_VERSION);
    host_send_host_capabilities(host_raw, HOST_CAP_FENCE_FD);

    // Drop the host end — the kernel closes it, plugin's next recv sees EOF.
    drop(host_fd);

    plugin.join().expect("plugin thread");
}
