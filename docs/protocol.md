<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Veiland plugin protocol — v1 (M3 draft)

This document is the source of truth for the wire protocol between veiland-core
(the host) and a plugin process. The Rust types in `veiland-protocol` are *an*
implementation of this spec; other languages may implement it directly. If the
Rust code and this document disagree, the document wins and the code is a bug.

## 1. Non-goals

- **Plugins do not know what host they are running under.** A plugin draws
  pixels and reacts to the events it is sent. Whether the host is a screen
  locker, a future login manager, or anything else is the host's concern, not
  the plugin's. There is no host-type field. Capability is expressed by which
  messages the host sends, not by self-declaration.
- **The protocol is not a flat record with optional fields.** Every message is
  a tag plus a payload whose shape is determined by the tag. Parsers dispatch
  on the tag.
- **The protocol is not bound to Rust.** This document defines the wire bytes
  exactly. Any language with a Unix-socket binding can implement it.
- **v1 carries dmabuf buffers only.** Plugins MUST be able to allocate dmabufs.
- **Plugins never receive keyboard input.** No protocol message carries
  keystrokes, key codes, or modifier state in any direction. This is a
  property of the protocol, not a runtime filter.

## 2. Transport

- **Socket type:** `AF_UNIX`, `SOCK_SEQPACKET`. Message boundaries are
  preserved by the kernel; one `recvmsg()` returns exactly one protocol
  message. There is no length prefix and no in-band framing — the socket type
  *is* the framing.
- **Spawning:** the host creates a `socketpair(AF_UNIX, SOCK_SEQPACKET, 0)`,
  forks, and `exec`'s the plugin with one end of the pair as fd 3. The
  environment variable `VEILAND_PLUGIN_SOCKET=3` tells the plugin which fd to
  use. No filesystem socket path is involved.
- **File descriptors:** carried out-of-band via `SCM_RIGHTS` ancillary data on
  `sendmsg`/`recvmsg`. Only the `Buffer` message carries an fd in v1; all
  other messages carry zero fds. A message arriving with the wrong number of
  fds is a protocol error.
- **Byte order:** all multi-byte integers are little-endian.
- **Maximum message size:** 64 KiB payload. The host MUST reject larger
  messages and close the plugin's socket. Plugins SHOULD never need to send
  anything close to this — typical messages are tens of bytes.

## 3. Encoding primitives

| Type    | Encoding                                                       |
| ------- | -------------------------------------------------------------- |
| `u8`    | 1 byte                                                          |
| `u16`   | 2 bytes, little-endian                                          |
| `u32`   | 4 bytes, little-endian                                          |
| `i32`   | 4 bytes, little-endian, two's complement                       |
| `u64`   | 8 bytes, little-endian                                          |
| `i64`   | 8 bytes, little-endian, two's complement                       |
| `str`   | `u16` length in bytes, then that many UTF-8 bytes. Per-field max declared at the field site. |
| `enum`  | `u8` tag, then the variant's payload (may be empty)             |

Strings are validated as UTF-8 on receipt. Invalid UTF-8 is a protocol error.

## 4. Frame layout

Each `recvmsg` returns one frame:

```
+--------+-----------------+
| u16    | variant payload |
| tag    | (tag-dependent) |
+--------+-----------------+
```

Tags are namespaced by direction. The host knows it is reading client messages
and decodes against the `ClientMessage` tag table; the plugin knows it is
reading server messages and decodes against the `ServerMessage` tag table.
Sending a server-direction tag in the client direction (or vice versa) is a
protocol error.

## 5. Protocol version

Before any tagged message, the two sides negotiate a protocol version in
sequence:

```
1. client → server:  u32 client_version
2. server → client:  u32 server_version  (only if it accepts client_version)
```

For v1, both values are `1`. If the host does not recognize the client
version, it closes the socket without replying. The plugin, having already
sent its version, sees the socket close before reading a server version and
infers a version mismatch. If the plugin reads a server version it does not
recognize, it closes the socket.

The handshake is *not* a `ClientMessage`/`ServerMessage` — it is four raw
little-endian bytes on each side, sent before the tagged-message stream begins.
This keeps version mismatch outside the variant-decoding path: a v2 codec that
changes how tags are encoded can still negotiate cleanly with a v1 peer.

## 6. Client messages (plugin → host)

### 6.1 `Hello` — tag `0x0001`

Sent exactly once, immediately after the version handshake. Sending any other
message before `Hello` is a protocol error.

```
str   plugin_name        (max 64 bytes)
str   plugin_version     (max 32 bytes)
```

### 6.2 `Buffer` — tag `0x0002`

Carries one dmabuf fd via `SCM_RIGHTS`.

```
u32   id            (plugin-assigned; referenced in later BufferReleased and BufferDestroy)
u32   width         (1..=8192)
u32   height        (1..=8192)
u32   format        (DRM FourCC, e.g. 0x34325241 for ARGB8888)
u64   modifier      (DRM modifier; LINEAR or INVALID for v1)
u32   stride        (must be >= width * bytes_per_pixel(format))
u32   offset        (typically 0; nonzero for sub-allocation)
```

**Id semantics.** The `id` is plugin-assigned and scoped to this connection
(the host's bookkeeping is per-socket; another plugin's identical id is
unrelated and never seen). A plugin MAY re-send the same `id` after receiving
the corresponding `BufferReleased`, either with the same fd (signaling
"buffer contents have changed, re-sample") or with a different fd (replacing
the buffer). Sending a `Buffer` with an `id` the host is currently using
(i.e. not yet released) is a protocol violation.

**Choosing ids (non-normative).** The `id` is opaque to the host. Plugins MAY
use any `u32` scheme; the simplest is `id = 0` for a single-buffer plugin,
or small sequential ids for a buffer pool. The host does not require
sequential, dense, or stable ids — only that the plugin uses the same id for
the same buffer when re-sending it.

**Validation, host side.** Before the fd is passed to EGL/GBM:
- `width` and `height` in `[1, 8192]`.
- `format` is in the host's allowlist (v1: `ARGB8888`).
- `modifier` is in the host's allowlist (v1: `LINEAR` or `INVALID`).
- `stride >= width * bpp(format)`.
- Exactly one fd was attached.

Any failure: log, close the plugin's socket, treat as plugin death.

### 6.3 `BufferDestroy` — tag `0x0003`

Plugin tells the host it will no longer reuse this buffer id. Host may release
any cached EGLImage/GL resources keyed on the id.

```
u32   id
```

> **v1 implementation note.** With v1's single-buffer model the plugin has no
> reason to send `BufferDestroy` before shutdown — at shutdown, socket close
> already prompts the host to free everything. `BufferDestroy` is on the wire
> from v1 so that buffer-pool plugins (M5+) and any pre-launch reference
> plugins that grow a pool can use it without a protocol bump.

## 7. Server messages (host → plugin)

### 7.1 `Configure` — tag `0x0001`

Sent at least once before the first `FrameDone`. May be sent again at any time
(e.g. on output resize, time tick coarsely).

```
i32   region_x
i32   region_y
u32   region_w           (1..=8192)
u32   region_h           (1..=8192)
u32   scale              (integer scale factor; 1, 2, 3)
i64   time_unix_seconds
i32   time_tz_offset_seconds
```

Scale is an integer in v1. Fractional scaling is a future extension that will
add a new field or message variant; v1 plugins that only handle integer scale
remain correct.

### 7.2 `FrameDone` — tag `0x0002`

Cue to render the next frame. Empty payload. Modelled on Wayland's frame
callbacks: the host throttles rendering by withholding `FrameDone`.

### 7.3 `BufferReleased` — tag `0x0003`

```
u32   id
```

Host is done sampling the buffer with this id; plugin may reuse it. In v1 with
a single buffer and `glFinish` on the plugin side, the host MAY omit this
message (the plugin reuses its single buffer unconditionally). Plugins MUST
tolerate not receiving it.

### 7.4 `Shutdown` — tag `0x0004`

Empty payload. Plugin SHOULD exit cleanly within a short grace period
(implementation-defined, e.g. 1 second). After the grace period the host will
`SIGTERM` the plugin.

## 8. Handshake and lifecycle

```
1. Host spawns plugin (socketpair + exec, fd 3, VEILAND_PLUGIN_SOCKET=3).
2. Plugin sends u32 client_version = 1.
3. Host sends u32 server_version = 1.
4. Plugin sends Hello.
5. Host sends Configure.
6. Host sends FrameDone.
7. Plugin renders, sends Buffer (with fd via SCM_RIGHTS).
8. Host samples buffer, composites, eventually sends FrameDone again.
   (Host MAY send BufferReleased; plugins MUST tolerate its absence in v1.)
9. Repeat from 6.
```

The list above is the success path. If any step fails (socket close, version
mismatch, validation failure), the lifecycle ends and the surviving side
cleans up. Either side may send `Shutdown` (host) or close the socket
(plugin) at any time. Socket EOF on either side means the peer is gone; the
other side cleans up.

## 9. Error handling

The host treats *any* protocol violation as plugin death, including but not
limited to:

- Unknown tag.
- Payload shorter or longer than the tag's schema requires.
- Invalid UTF-8 in a `str`.
- Out-of-range field (width, height, scale, etc.).
- Wrong number of fds attached.
- Message arriving before its prerequisite (`Buffer` before `Hello`, etc.).
- Reuse of an in-use resource id (see §6.2).

On violation: log the plugin name and the violation, close the socket. What
the host does after that (draw a fallback, leave the region as-is, etc.) is
host policy and out of scope for the protocol. Never panic. Never `assert`
on plugin input. This rule is load-bearing for the threat model — see
`CLAUDE.md` §"Threat model".

Plugins SHOULD treat any protocol violation from the host as a fatal error
and exit. Hosts are trusted; a buggy host is a host bug worth surfacing.

## 10. Forward compatibility

- New message variants get new tag values. Old peers that see an unknown tag
  treat it as a protocol error (see §9) — this is intentional. Forward-compat
  via "ignore unknown" requires a capability negotiation we have not designed
  yet; v1 is strict.
- Fields are never appended to existing variants in place. To add a field,
  introduce a new variant with a new tag, or bump the protocol version. This
  is the rule for evolving the protocol at any version, not just v1.
- The version handshake (§5) is the escape hatch for incompatible changes.
