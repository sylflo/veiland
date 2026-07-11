// SPDX-License-Identifier: GPL-3.0-or-later

//! Errors returned by the host-side plugin module. Mirror of
//! `veiland_plugin::PluginError` from the other direction: same
//! wrapping pattern, different variant set (we have `Spawn` because
//! the host forks plugins; we have `PluginDisconnected` because the
//! host outlives any given plugin and treats its exit as routine).

use veiland_protocol::ProtocolError;

#[derive(Debug)]
pub enum HostError {
    /// Syscall-layer error already wrapped as `std::io::Error`.
    Io(std::io::Error),

    /// `recvmsg` / `sendmsg` / `socketpair` error from `nix`. Distinct
    /// from `Io` because `nix::Error` is not an `io::Error`.
    Nix(nix::Error),

    /// Decode failure from `veiland-protocol`. Bytes did not parse.
    Protocol(ProtocolError),

    /// Bytes decoded but the result violates a protocol invariant
    /// the codec can't check (wrong fd count on a message, Hello
    /// after Hello, modifier the GL stack rejects at import time).
    ProtocolViolation(&'static str),

    /// Version handshake disagreement. Both sides recorded so the
    /// log line is informative.
    VersionMismatch { host: u32, plugin: u32 },

    /// EGL import / GL setup failed on the host side after a
    /// plugin handed us a buffer.
    Render(&'static str),

    /// Plugin closed the socket cleanly (zero-byte recvmsg). A
    /// normal lifecycle event — log at info, not error.
    PluginDisconnected,

    /// A bare `binary` name in the config resolved to nothing: not found
    /// beside the locker and not on `$PATH`. Non-fatal — the plugin's
    /// layer stays empty and the locker keeps running. Carries the name
    /// that failed to resolve so the log line is actionable.
    BinaryNotFound(String),

    /// A read deadline expired while waiting for the plugin to speak.
    /// Only produced during the spawn window: `try_spawn_one` sets
    /// `SO_RCVTIMEO` around the handshake and `Hello` reads — the one
    /// place the host reads without calloop readiness, on the main
    /// thread. A plugin that opens the socket but never writes must
    /// fail the spawn, not freeze the locker.
    RecvTimeout,

    /// A non-blocking runtime read (`MSG_DONTWAIT`) found nothing to read.
    /// Not an error condition — it means a calloop source fired on this
    /// fd but no message was queued (a spurious/stale readiness fire, e.g.
    /// the hotplug slot-reuse path). The caller treats it as "nothing to
    /// do" and keeps the plugin alive, unlike every other recv error.
    /// Only produced once `runtime_reads` is on (post-handshake).
    WouldBlock,

    /// A write deadline expired while sending a server message to the
    /// plugin. `SO_SNDTIMEO` is set after the handshake and stays on for
    /// the life of the plugin: unlike reads (which calloop only issues
    /// when the fd is readable), the host writes `FrameDone` /
    /// `BufferReleased` / `Configure` from the main thread with no
    /// readiness gate. A plugin that stops draining its socket fills the
    /// kernel send buffer; without this deadline the next `sendmsg` would
    /// block the calloop thread forever, taking keyboard input with it.
    /// Treated as plugin death — the socket is closed and the region
    /// falls back, exactly like any other send failure.
    SendTimeout,
}

impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            HostError::Io(e) => write!(f, "io: {}", e),
            HostError::Nix(e) => write!(f, "nix: {}", e),
            HostError::Protocol(e) => write!(f, "protocol decode: {}", e),
            HostError::ProtocolViolation(msg) => write!(f, "protocol violation: {}", msg),
            HostError::VersionMismatch { host, plugin } => write!(
                f,
                "protocol version mismatch: host speaks v{}, plugin speaks v{}",
                host, plugin
            ),
            HostError::Render(msg) => write!(f, "render setup: {}", msg),
            HostError::PluginDisconnected => write!(f, "plugin disconnected"),
            HostError::BinaryNotFound(name) => write!(
                f,
                "binary {:?} not found beside the locker or on $PATH \
                 (write a path with a '/' to point at an exact file)",
                name
            ),
            HostError::RecvTimeout => {
                write!(f, "plugin stayed silent past the handshake deadline")
            }
            HostError::WouldBlock => {
                write!(f, "runtime read found nothing queued (spurious readiness)")
            }
            HostError::SendTimeout => {
                write!(
                    f,
                    "plugin stopped draining its socket past the send deadline"
                )
            }
        }
    }
}

impl std::error::Error for HostError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            HostError::Io(e) => Some(e),
            HostError::Nix(e) => Some(e),
            HostError::Protocol(e) => Some(e),
            _ => None,
        }
    }
}

impl From<std::io::Error> for HostError {
    fn from(e: std::io::Error) -> Self {
        HostError::Io(e)
    }
}

impl From<nix::Error> for HostError {
    fn from(e: nix::Error) -> Self {
        HostError::Nix(e)
    }
}

impl From<ProtocolError> for HostError {
    fn from(e: ProtocolError) -> Self {
        HostError::Protocol(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time check that every variant has a Display arm.
    /// If a new variant is added without updating Display, this fails to build.
    #[test]
    fn display_renders_every_variant() {
        let _ = format!("{}", HostError::Io(std::io::Error::other("x")));
        let _ = format!("{}", HostError::Nix(nix::Error::EINVAL));
        let _ = format!("{}", HostError::Protocol(ProtocolError::Truncated));
        let _ = format!("{}", HostError::ProtocolViolation("test"));
        let _ = format!("{}", HostError::VersionMismatch { host: 1, plugin: 2 });
        let _ = format!("{}", HostError::Render("test"));
        let _ = format!("{}", HostError::PluginDisconnected);
        let _ = format!("{}", HostError::BinaryNotFound("test".into()));
        let _ = format!("{}", HostError::RecvTimeout);
        let _ = format!("{}", HostError::WouldBlock);
        let _ = format!("{}", HostError::SendTimeout);
    }
}
