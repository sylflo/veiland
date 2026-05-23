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
  `sendmsg`/`recvmsg`. Only the `Buffer` message carries fds; all other
  messages carry zero. `Buffer` always carries a dmabuf fd, and optionally a
  sync-fence fd as a second `SCM_RIGHTS` fd when the host has advertised
  `HOST_CAP_FENCE_FD` (see §5.1) and the plugin opted into the fast path.
  A message arriving with an fd count outside what the capability state and
  message tag allow is a protocol error.
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

## 5. Protocol version and host capabilities

Before any tagged message, the two sides negotiate a protocol version and the
host advertises its capabilities, in sequence:

```
1. client → server:  u32 client_version
2. server → client:  u32 server_version       (only if it accepts client_version)
3. server → client:  u32 host_capabilities    (immediately after server_version)
```

For v1, both version values are `1`. If the host does not recognize the client
version, it closes the socket without replying. The plugin, having already
sent its version, sees the socket close before reading a server version and
infers a version mismatch. If the plugin reads a server version it does not
recognize, it closes the socket.

The handshake is *not* a `ClientMessage`/`ServerMessage` — it is raw
little-endian u32s on each side, sent before the tagged-message stream begins.
This keeps version mismatch outside the variant-decoding path: a future codec
that changes how tags are encoded can still negotiate cleanly with the
existing peer.

### 5.1 Host capabilities

`host_capabilities` is a bitfield declaring which optional protocol features
the host supports. The plugin reads it after `server_version` and uses it to
decide which path to take for features whose support is host-dependent.

| Bit  | Name                  | Meaning                                                                                                                         |
| ---- | --------------------- | ------------------------------------------------------------------------------------------------------------------------------- |
| 0    | `HOST_CAP_FENCE_FD`   | Host accepts a sync-fence fd attached as a second `SCM_RIGHTS` fd on `Buffer` messages, and will wait on it before sampling.    |
| 1–31 | reserved              | MUST be zero. A plugin that sees any reserved bit set MUST treat the handshake as a protocol violation and close the socket.    |

The reserved-MUST-be-zero rule is forward-compatibility hygiene: future
capability bits are added by introducing new bit positions, never by
redefining bits the spec currently marks reserved. A plugin built against
this spec can therefore trust that bits it doesn't understand mean the host
is doing something the plugin wasn't designed for, and fail closed rather
than guess.

A host MUST NOT set a capability bit it does not implement. A plugin SHOULD
NOT use a capability without checking the corresponding bit; doing so risks
a protocol violation if the host has it disabled (e.g. `HOST_CAP_FENCE_FD`
off and the plugin attaches a fence fd anyway — the host will see two fds
on `Buffer`, expect one, and close the socket).

How a host decides its capability bits is host-policy and out of scope for
this spec. For `HOST_CAP_FENCE_FD` the natural rule is "set iff the host's
EGL display exposes `EGL_ANDROID_native_fence_sync`."

## 6. Client messages (plugin → host)

### 6.1 `Hello` — tag `0x0001`

Sent exactly once, immediately after the version handshake. Sending any other
message before `Hello` is a protocol error.

```
str   plugin_name        (max 64 bytes)
str   plugin_version     (max 32 bytes)
```

### 6.2 `Buffer` — tag `0x0002`

Carries one dmabuf fd via `SCM_RIGHTS`, optionally followed by a sync-fence fd
when both sides are on the fast path (see "Sync fence" below).

```
u32   id            (plugin-assigned; referenced in later BufferReleased and BufferDestroy)
u32   width         (1..=8192)
u32   height        (1..=8192)
u32   format        (DRM FourCC; accepted iff host's GL can import it)
u64   modifier      (DRM modifier; accepted iff host's GL can import it)
u32   stride        (must be >= width)
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

**Validation, host side.** Validation happens in two layers:

- **Codec layer** (in `veiland-protocol`, no I/O):
  - `width` and `height` in `[1, 8192]`.
  - `stride >= width`. Loose lower bound — the codec doesn't know bytes-per-pixel
    for an arbitrary format. Pathological strides fail in the EGL layer below.
  - Fd count was 1 (default) or 2 (if `HOST_CAP_FENCE_FD` was advertised and
    the plugin opted in). Enforced at the socket layer, not the codec — see
    "Sync fence" below.
- **EGL import layer** (in `veiland-core`, has hardware context):
  - `format` and `modifier` are acceptable iff `eglCreateImage` with the dmabuf
    fd succeeds. The set of accepted formats and modifiers depends on the host's
    GL stack and the plugin's allocator — for example NVIDIA's proprietary
    userspace driver produces vendor-private tiling modifiers like
    `0x0300000000e08014` that Mesa-only setups will not see. The codec cannot
    know what the GL stack will accept; only the GL stack itself can.
  - `stride` and `offset` consistency with the fd's underlying buffer size is
    checked implicitly by EGL.

Any failure at either layer: log, close the plugin's socket, treat as plugin
death. The lock surface falls back to its clear color in the affected region.

**Sync fence (optional second fd).** When the host advertised
`HOST_CAP_FENCE_FD` in the handshake (§5.1), the plugin MAY attach a second
`SCM_RIGHTS` fd to its `Buffer` messages. The two fds appear in the cmsg in
this order, matched by arrival order:

1. **Dmabuf fd** (always present) — references the GPU buffer the host imports
   as an `EGLImage`.
2. **Fence fd** (present iff fast-path) — a dma-fence fd, produced by the
   plugin via `eglDupNativeFenceFDANDROID` after the render commands targeting
   the dmabuf were flushed. The host imports it as an EGL sync object and
   waits on it before sampling the dmabuf — without that wait, the host might
   sample a half-rendered frame.

The plugin chooses fast or slow path **once at startup**, after reading the
handshake, based on `HOST_CAP_FENCE_FD` AND its own EGL display's support for
`EGL_ANDROID_native_fence_sync`. Both must be true for the fast path. The
choice is fixed for the connection's lifetime — plugins do not switch paths
per frame.

- **Fast path (M5a):** every `Buffer` carries 2 fds (dmabuf + fence). Plugin
  flushes its GL command stream, exports a fence fd, sends both. Host waits
  on the fence before sampling.
- **Slow path (M3 fallback):** every `Buffer` carries 1 fd (dmabuf only).
  Plugin calls `gl::Finish` before `send_buffer` to ensure the dmabuf is
  GPU-complete on the wire. Host samples without waiting.

The host's fd-count expectation is determined by what *it* advertised plus
what it observes the plugin doing on the first `Buffer`:

- Host advertised `HOST_CAP_FENCE_FD` AND plugin's first `Buffer` has 2 fds →
  fast path locked in; every subsequent `Buffer` MUST also carry 2 fds.
- Host advertised `HOST_CAP_FENCE_FD` AND plugin's first `Buffer` has 1 fd →
  slow path locked in; every subsequent `Buffer` MUST also carry 1 fd.
- Host did NOT advertise `HOST_CAP_FENCE_FD` AND plugin sends `Buffer` with
  2 fds → protocol violation; socket closes.

A plugin that flip-flops between 1-fd and 2-fd across messages is a protocol
violation. The path is a connection-level decision, not a per-message one.

> **Modifier `INVALID` (`u64::MAX`)** is a special DRM sentinel meaning
> "unknown / unspecified" — what `gbm_bo_create` returns when called without
> explicit modifier negotiation. It's a legitimate value plugins may send;
> whether the host can import it is again EGL's decision.

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
str   output_name        (max 64 bytes)
```

Scale is an integer in v1. Fractional scaling is a future extension that will
add a new field or message variant; v1 plugins that only handle integer scale
remain correct.

`output_name` is the `xdg_output.name` string for the output this plugin
instance is rendering for (e.g. `"DP-1"`, `"HDMI-A-1"`, `"eDP-1"`). Each
`[[plugin]]` entry in the user's config is instantiated once per matching
output (see `docs/config.md` §5); each instance is its own process and each
gets its own `Configure` with the corresponding `output_name`.

Plugins that don't care about per-output behaviour ignore the field. Plugins
that do care (a wallpaper rendering different images per monitor, a clock
showing a different timezone per monitor) use `output_name` as the key for
per-instance state — it is the only protocol-level signal that distinguishes
"the DP-1 instance of plugin X" from "the HDMI-A-1 instance of plugin X."

The 64-byte cap matches `Hello.plugin_name`'s cap. Real Wayland output names
are short (typically 4–10 bytes); 64 is wildly generous and exists to bound
the decoder's allocation. Empty `output_name` is accepted on the wire — it
represents the rare case where the compositor has not yet delivered an
`xdg_output.name` event for this output (transient, expected to resolve
within one roundtrip).

### 7.2 `FrameDone` — tag `0x0002`

Cue to render the next frame. Empty payload. Modelled on Wayland's frame
callbacks: the host throttles rendering by withholding `FrameDone`.

### 7.3 `BufferReleased` — tag `0x0003`

```
u32   id
```

Host is done sampling the buffer with this id; plugin may reuse it.

- **Fast path (host advertised `HOST_CAP_FENCE_FD` and plugin opted in):**
  host MUST send `BufferReleased` after it finishes sampling each buffer.
  Plugins use this to gate the next render — overwriting the dmabuf before
  the release arrives races the host's GPU read. Step 10 of M5a specifies
  how the host knows sampling is complete (host-side egress fence).
- **Slow path (no `HOST_CAP_FENCE_FD`, or plugin chose `glFinish`):** host
  MAY omit `BufferReleased`. The plugin's `glFinish` before `send_buffer`
  makes the buffer GPU-stable on send, and the single-buffer model rewrites
  unconditionally on the next `FrameDone`. Plugins on the slow path MUST
  tolerate not receiving `BufferReleased`.

Buffer-pool plugins (M5b+, if it lands) will track release per-id so the
pool's free-list reflects host-side completion. M5a's single-buffer plugin
uses `BufferReleased` purely as a wait point, not as an id-keyed structure.

### 7.4 `Shutdown` — tag `0x0004`

Empty payload. Plugin SHOULD exit cleanly within a short grace period
(implementation-defined, e.g. 1 second). After the grace period the host will
`SIGTERM` the plugin.

## 8. Handshake and lifecycle

```
 1. Host spawns plugin (socketpair + exec, fd 3, VEILAND_PLUGIN_SOCKET=3).
 2. Plugin sends u32 client_version = 1.
 3. Host sends u32 server_version = 1.
 4. Host sends u32 host_capabilities (bitfield; see §5.1).
 5. Plugin sends Hello.
 6. Host sends Configure.
 7. Host sends FrameDone.
 8. Plugin renders, sends Buffer (with dmabuf fd, plus a fence fd if
    fast-path; see §6.2).
 9. Host imports dmabuf, waits on fence (if present), composites,
    sends BufferReleased (if fast-path), eventually sends FrameDone again.
10. Repeat from 7.
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

## 11. Open questions (resolve before the relevant milestone)

- **§6.2 — explicit fd count (resolved in M5a).** Originally framed as
  "fd_count byte in the payload" vs "one fd per message tag, ever." M5a
  picked a third option that emerged from the capability handshake design:
  **implicit per-tag, validated by the host using the negotiated capability
  state**. No `fd_count` byte on the wire. For `Buffer` specifically, the
  rule has two levels: the tag determines the *maximum* (1 or 2 fds), and
  the capability advertisement plus the plugin's first-Buffer behaviour
  determine the *required* count for the connection's lifetime. The rule is
  spelled out in §6.2 "Sync fence" and §2 "Transport — File descriptors."
  Other fd-carrying messages added in the future will follow the same
  pattern: per-tag implicit rule, capability-gated where optionality is
  needed.
- **§6.2 — format and modifier validation (resolved in M3).** Originally the
  codec rejected anything outside `format ∈ {ARGB8888}` and
  `modifier ∈ {LINEAR, INVALID}`. NVIDIA's proprietary driver returns
  vendor-private tiling modifiers (e.g. `0x0300000000e08014`) and the gradient
  plugin uses `XRGB8888` (`Fourcc(0x34325258)`) — both round-trip cleanly
  through EGL on the same machine but tripped the allowlists. Resolution:
  both checks moved out of the codec into the host's `eglCreateImage` import
  path. Codec now accepts any `Fourcc` and any `Modifier` value; the EGL call
  is the validation. `INVALID` is documented inline in §6.2 as a legitimate
  sentinel (`gbm_bo_create` without explicit modifier negotiation returns
  it). Landed atomically across `veiland-protocol`, `veiland-core`, and this
  doc as part of the M3 commit.

## 12. Rendering conventions

These describe how plugins should produce pixel data so the host's
compositor blends multiple plugins correctly. They are not wire-format
constraints — the wire bytes are unchanged — but the host assumes them.

### 12.1 Straight (non-pre-multiplied) alpha

Plugins emit pixels with straight alpha: the RGB channels store the
colour as-is, and the alpha channel separately stores opacity. A red
pixel at 50% opacity is `(1.0, 0.0, 0.0, 0.5)`, not
`(0.5, 0.0, 0.0, 0.5)`.

The host's compositor uses
`glBlendFunc(GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA)`, the natural blend
function for straight alpha. Plugins that pre-multiply alpha will
appear washed-out where their regions overlap lower-z plugins.

This convention was chosen over pre-multiplied alpha because the
natural shape of writing a fragment shader
(`gl_FragColor = vec4(rgb, a)`) produces straight alpha;
pre-multiplied would force every plugin author to remember to
multiply, and the failure mode is subtle. See `docs/m6-plan.md` Q4.

If pre-multiplied alpha becomes needed for a specific plugin in the
future, it should be opted into via a flag in `Hello`, not made the
default.
