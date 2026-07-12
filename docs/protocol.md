<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Veiland plugin protocol — v1

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
  use. If the user declared a `[plugin.config]` table for this plugin (see
  `docs/config.md` §3), the host also exports `VEILAND_PLUGIN_CONFIG` set to
  the JSON-serialised table; otherwise it is unset. Plugins parse the JSON
  themselves — the protocol does not interpret it. No filesystem socket path
  is involved.
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
  anything close to this — typical messages are tens of bytes. (Not yet
  enforced explicitly by the reference host — see §13; it relies on kernel
  truncation + decode failure, which disconnects the plugin all the same.)

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
| `enum`  | `u16` tag (little-endian), then the variant's payload (may be empty) |

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

Timing is host policy: the host MAY apply a deadline to the handshake and to
the first `ClientMessage` (the reference host uses 2 seconds) and treat
expiry as a failed spawn — the plugin is killed and its layer stays empty. A
plugin SHOULD therefore complete the handshake and send `Hello` before any
expensive setup of its own (GPU init, asset decode); the reference SDK's
`Connection::connect` does this ordering naturally.

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
off and the plugin attaches a fence fd anyway — §6.2 makes that a
violation, though the reference host does not yet enforce it; see §13).

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
(i.e. not yet released) is a protocol violation. (Not yet enforced by the
reference host — see §13; it is single-buffer and replaces the texture on
each `Buffer` regardless of id, so reuse is currently tolerated.)

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

- **Fast path:** every `Buffer` carries 2 fds (dmabuf + fence). Plugin
  flushes its GL command stream, exports a fence fd, sends both. Host waits
  on the fence before sampling.
- **Slow path (fallback):** every `Buffer` carries 1 fd (dmabuf only).
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
(The lock-in and the "2 fds with `HOST_CAP_FENCE_FD` off" rejection are not
yet enforced by the reference host — see §13; it accepts 1 or 2 fds on every
`Buffer` and does not consult the negotiated capability after the handshake.)

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
> from v1 so that future buffer-pool plugins and any reference
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
u32   scale_120          (output scale in 120ths; 1..=9999)
i64   time_unix_seconds
i32   time_tz_offset_seconds
str   output_name        (max 64 bytes)
```

`scale_120` carries the output scale in 120ths, matching
`wp_fractional_scale_v1`'s encoding: 120 = 1×, 180 = 1.5×, 240 = 2×.
Convert to a float multiplier with `scale_120 as f32 / 120.0`. Values
outside `1..=9999` are rejected at decode.

The region dimensions (`region_w`, `region_h`) are already in physical pixels;
plugins do not multiply them by `scale_120`. `scale_120` is for converting
*plugin-internal* logical sizes (font sizes, shadow radii, asset selection)
into physical pixels. The host sources it from
`wp_fractional_scale_v1.preferred_scale` where the compositor advertises that
protocol, and falls back to the integer `wl_output.scale` × 120 otherwise;
both paths clamp on the host side so the encoder never produces an
out-of-range Configure.

A re-`Configure` may arrive at any time with a different `scale_120` (e.g. the
user changes their monitor's scale factor in the compositor settings). The
plugin should latch the new value and use it on the next `FrameDone` — there
is no separate "scale-changed" message and no requirement to re-render
immediately on receipt. The particle plugins (`veiland-particles`,
`veiland-sakura`, `veiland-snow`, ...) are the reference shape for this:
`scale_120` is stored on plugin state at every `Configure` and each render
multiplies logical-pixel config values (`radius_px`, drift) by the current
scale factor. (Text plugins like `veiland-label` instead size by fraction
of the physical surface and never read `scale_120`; `position` is a surface
fraction in every plugin and is deliberately not scaled. Both conventions
are described in `docs/plugin-api.md` §HiDPI.)

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

The host MUST send `BufferReleased` after it finishes sampling each
buffer, on both sync paths. Plugins use it to gate the next render —
overwriting the dmabuf before the release arrives races the host's GPU
read. The host uses a host-side egress fence to know when sampling is
complete.

- **Fast path (host advertised `HOST_CAP_FENCE_FD` and plugin opted in):**
  the release is the only signal that the host is done with the buffer;
  everything else in the frame loop is asynchronous.
- **Slow path (no `HOST_CAP_FENCE_FD`, or plugin chose `glFinish`):** the
  plugin's `glFinish` before `send_buffer` makes the buffer GPU-stable on
  send (the host may sample without waiting), but the release is still
  required: the reference SDK's `FramePacer` waits for it before rendering
  the next frame on both paths, and gating the rewrite on it keeps the
  plugin from overwriting the dmabuf while the host's GPU is still
  sampling it.

> An earlier revision of this spec let the host omit `BufferReleased` on
> the slow path ("plugins MUST tolerate not receiving it"). That
> permission is withdrawn: the reference host has always sent the release
> on both paths and the reference SDK has always required it, so no
> conforming implementation relied on the omission.

Future buffer-pool plugins will track release per-id so the
pool's free-list reflects host-side completion. The current single-buffer
plugin uses `BufferReleased` purely as a wait point, not as an id-keyed
structure.

### 7.4 `Shutdown` — tag `0x0004`

Empty payload. Plugin SHOULD exit cleanly within a short grace period
(implementation-defined — the reference host allows a few hundred
milliseconds). After the grace period the host will `SIGTERM` the plugin.

## 8. Handshake and lifecycle

```
 1. Host spawns plugin (socketpair + exec, fd 3, VEILAND_PLUGIN_SOCKET=3,
    optionally VEILAND_PLUGIN_CONFIG=<json> from [plugin.config]; see §2).
 2. Plugin sends u32 client_version = 1.
 3. Host sends u32 server_version = 1.
 4. Host sends u32 host_capabilities (bitfield; see §5.1).
 5. Plugin sends Hello.
 6. Host sends Configure.
 7. Host sends FrameDone.
 8. Plugin renders, sends Buffer (with dmabuf fd, plus a fence fd if
    fast-path; see §6.2).
 9. Host imports dmabuf, waits on fence (if present), composites,
    sends BufferReleased, eventually sends FrameDone again.
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
- Out-of-range field (width, height, scale_120, etc.).
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

## 11. Design decisions (resolved)

- **§6.2 — explicit fd count.** Originally framed as
  "fd_count byte in the payload" vs "one fd per message tag, ever." The
  resolution was a third option that emerged from the capability handshake design:
  **implicit per-tag, validated by the host using the negotiated capability
  state**. No `fd_count` byte on the wire. For `Buffer` specifically, the
  rule has two levels: the tag determines the *maximum* (1 or 2 fds), and
  the capability advertisement plus the plugin's first-Buffer behaviour
  determine the *required* count for the connection's lifetime. The rule is
  spelled out in §6.2 "Sync fence" and §2 "Transport — File descriptors."
  Other fd-carrying messages added in the future will follow the same
  pattern: per-tag implicit rule, capability-gated where optionality is
  needed.
- **§6.2 — format and modifier validation.** Originally the
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
  doc in a single commit.

## 12. Rendering conventions

These describe how plugins should produce pixel data so the host's
compositor blends multiple plugins correctly. They are not wire-format
constraints — the wire bytes are unchanged — but the host assumes them.

### 12.1 Premultiplied alpha

Plugins emit pixels with premultiplied alpha: the RGB channels store
the colour already scaled by alpha. A red pixel at 50% opacity is
`(0.5, 0.0, 0.0, 0.5)`, not `(1.0, 0.0, 0.0, 0.5)`.

The host's compositor uses
`glBlendFunc(GL_ONE, GL_ONE_MINUS_SRC_ALPHA)`, the correct blend
function for premultiplied alpha. Plugins that emit straight alpha
(RGB not scaled by alpha) will appear too bright where their regions
overlap lower-z plugins.

This convention was chosen because `veiland-text` (the glyph atlas
renderer used by the clock and label plugins) emits premultiplied
alpha — glyph coverage composites correctly only once under
`ONE / ONE_MINUS_SRC_ALPHA`. Straight alpha with that blend function
double-applied coverage and produced a halo around text edges.
Premultiplying in the fragment shader is one extra multiply per pixel
and the failure mode (washed-out blending) is visible immediately.

## 13. Known deviations (reference host)

The reference host (`veiland-core`) is intentionally more lenient than
this spec in three places. Each is called out inline above; they are
collected here as the backlog. All three are **lenient** — the host
accepts input the spec says to reject — so none can crash the locker,
leak, or affect the unlock decision, and the reference SDK never triggers
any of them (it picks one fd-path at connect time, is single-buffer, and
sends only tiny messages). Enforcing them touches the untrusted IPC hot
path, so it is deferred rather than rushed. Until it lands, a plugin
author MUST NOT rely on the host closing the socket in these cases.

- **Fd-count capability lock-in (§6.2).** The host accepts 1 or 2 fds on
  every `Buffer` and does not consult the negotiated `HOST_CAP_FENCE_FD`
  after the handshake, so it neither locks in the fast/slow path from the
  first `Buffer` nor rejects a 2-fd `Buffer` when the capability is off.
  A flip-flopping plugin is tolerated. (`veiland-core/src/plugin/connection.rs`,
  the `recv_message` variant/fd-count match.)

- **In-use buffer-id reuse (§6.2).** The host tracks `current_buffer_id`
  but does not reject a `Buffer` whose id is still in use; it imports and
  replaces the texture regardless. Benign under the single-buffer model.
  (`veiland-core/src/plugin/state.rs`, the `Buffer` arm of `handle_message`.)

- **Explicit >64 KiB rejection (§2).** The host does not check `MSG_TRUNC`;
  an oversized `SOCK_SEQPACKET` message is truncated by the kernel into the
  fixed 64 KiB receive buffer and then fails to decode, which disconnects
  the plugin all the same. This is correct for every message shape defined
  today (the largest legal message is a ~104-byte `Hello`), but it is
  truncation + decode-failure, not an explicit size check — and would need
  revisiting if a large message type is ever added.
  (`veiland-core/src/plugin/connection.rs`, the fixed-size `recvmsg` buffer.)
