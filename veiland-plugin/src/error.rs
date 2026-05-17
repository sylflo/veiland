// SPDX-License-Identifier: GPL-3.0-or-later

//! Errors returned by `veiland-plugin`. One variant per recovery
//! decision a plugin author might make; see the module docs in
//! `lib.rs` for the canonical event loop that consumes these.

use veiland_protocol::ProtocolError;

#[derive(Debug)]
pub enum PluginError {
    /// Required environment variable missing or malformed.
    /// Carries a short static name of what was wrong.
    Env(&'static str),

    /// Syscall-layer error already wrapped as `std::io::Error`.
    Io(std::io::Error),

    /// `recvmsg` / `sendmsg` error from `nix`. Distinct from `Io`
    /// because `nix::Error` is not an `io::Error`.
    Nix(nix::Error),

    /// Decode failure from `veiland-protocol`. Bytes did not parse.
    Protocol(ProtocolError),

    /// Bytes decoded but the result violates a protocol invariant
    /// the codec can't check (e.g. fd attached to a fd-less message).
    ProtocolViolation(&'static str),

    /// Version handshake disagreement. Both sides recorded so the
    /// log line is informative.
    VersionMismatch { plugin: u32, host: u32 },

    /// EGL / GBM / GL setup failed. Deliberately a short string —
    /// plugin authors rarely care which EGL call was unhappy.
    Render(&'static str),

    /// Host closed the socket cleanly (zero-byte recvmsg). A normal
    /// lifecycle end-state, not really an error.
    Disconnected,
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginError::Env(name) => write!(f, "environment: {}", name),
            PluginError::Io(e) => write!(f, "io: {}", e),
            PluginError::Nix(e) => write!(f, "nix: {}", e),
            PluginError::Protocol(e) => write!(f, "protocol decode: {}", e),
            PluginError::ProtocolViolation(msg) => write!(f, "protocol violation: {}", msg),
            PluginError::VersionMismatch { plugin, host } => write!(
                f,
                "protocol version mismatch: plugin speaks v{}, host speaks v{}",
                plugin, host
            ),
            PluginError::Render(msg) => write!(f, "render setup: {}", msg),
            PluginError::Disconnected => write!(f, "host disconnected"),
        }
    }
}

impl std::error::Error for PluginError {}

impl From<std::io::Error> for PluginError {
    fn from(e: std::io::Error) -> Self {
        PluginError::Io(e)
    }
}

impl From<nix::Error> for PluginError {
    fn from(e: nix::Error) -> Self {
        PluginError::Nix(e)
    }
}

impl From<ProtocolError> for PluginError {
    fn from(e: ProtocolError) -> Self {
        PluginError::Protocol(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compile-time check that every variant has a Display arm.
    /// If a new variant is added without updating Display, this fails to build.
    #[test]
    fn display_renders_every_variant() {
        let _ = format!("{}", PluginError::Env("VEILAND_PLUGIN_SOCKET"));
        let _ = format!(
            "{}",
            PluginError::Io(std::io::Error::new(std::io::ErrorKind::Other, "x"))
        );
        let _ = format!("{}", PluginError::Nix(nix::Error::EINVAL));
        let _ = format!("{}", PluginError::Protocol(ProtocolError::Truncated));
        let _ = format!("{}", PluginError::ProtocolViolation("test"));
        let _ = format!("{}", PluginError::VersionMismatch { plugin: 1, host: 2 });
        let _ = format!("{}", PluginError::Render("test"));
        let _ = format!("{}", PluginError::Disconnected);
    }
}
