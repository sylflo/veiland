<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Buffer lifecycle sequence diagram

Covers: spawn through steady-state render loop, for both the fast sync
path (EGL fence) and the slow sync path (glFinish). See
`docs/protocol.md` §6.2 and §8 for the wire-format details.

## Spawn and handshake

```
  veiland-core                          plugin process
  (HostConnection)                      (Connection)
       │                                      │
       │  socketpair() + fork() + exec()      │
       │─────────────────────────────────────►│
       │                                      │
       │◄── u32 client_version = 1 ───────────│
       │                                      │
       │─── u32 server_version = 1 ──────────►│
       │─── u32 host_capabilities ───────────►│
       │    (HOST_CAP_FENCE_FD if             │
       │     EGL_ANDROID_native_fence_sync    │
       │     is available on host)            │
       │                                      │
       │◄── Hello(plugin_name, version) ──────│
       │                                      │
       │─── Configure(region, scale,         ►│
       │              time, output_name)      │
       │                                      │  allocate DmaBuffer
       │                                      │  (GBM bo, FBO)
       │                                      │
       │─── FrameDone ───────────────────────►│
       │                                      │
```

`Configure` is sent before the first `FrameDone` so the plugin knows
its region size before it allocates its `DmaBuffer`. On a fresh lock,
the compositor may not have sent a surface size yet — the plugin
starts with a 1080p fallback and receives a second `Configure` with
the true size once `SessionLockHandler::configure` fires.

## Steady-state loop — fast path (HOST_CAP_FENCE_FD + plugin opted in)

Both sides have `EGL_ANDROID_native_fence_sync`. Every `Buffer`
carries 2 fds: the dmabuf and a sync fence.

```
  veiland-core                          plugin process
       │                                      │
       │─── FrameDone ───────────────────────►│
       │                                      │  gl draw calls
       │                                      │  eglDupNativeFenceFDANDROID
       │                                      │  → fence_fd
       │                                      │
       │◄── Buffer(id, w, h, fmt, mod,        │
       │           stride, offset)            │
       │    SCM_RIGHTS: [dmabuf_fd, fence_fd] │
       │                                      │
       │    egl wait on fence_fd              │
       │    import_dmabuf → GlTexture         │
       │    repaint_lock_surfaces             │
       │      composite all plugins           │
       │      draw password field             │
       │    eglSwapBuffers                    │
       │                                      │
       │─── BufferReleased(id) ──────────────►│
       │                                      │  (can reuse DmaBuffer now)
       │─── FrameDone ───────────────────────►│
       │                                      │  gl draw calls ...
       │                  (repeats)           │
```

The fence ensures the host's GPU has finished with the dmabuf before
the plugin overwrites it on the next frame. `BufferReleased` tells the
plugin when the host is done; the plugin must not render into the buffer
again until it arrives.

## Steady-state loop — slow path (no HOST_CAP_FENCE_FD, or plugin chose glFinish)

No fence fd. The plugin calls `gl::Finish()` before sending the buffer
to guarantee GPU completion on the send side. The host samples
immediately on receipt. `BufferReleased` may or may not be sent; the
plugin must tolerate not receiving it.

```
  veiland-core                          plugin process
       │                                      │
       │─── FrameDone ───────────────────────►│
       │                                      │  gl draw calls
       │                                      │  gl::Finish()
       │                                      │  (blocks until GPU done)
       │                                      │
       │◄── Buffer(id, w, h, fmt, mod,        │
       │           stride, offset)            │
       │    SCM_RIGHTS: [dmabuf_fd]           │
       │    (1 fd only)                       │
       │                                      │
       │    import_dmabuf → GlTexture         │
       │    repaint_lock_surfaces             │
       │    eglSwapBuffers                    │
       │                                      │
       │─── FrameDone ───────────────────────►│
       │                                      │  (BufferReleased not sent;
       │                                      │   plugin rewrites unconditionally
       │                                      │   on next FrameDone)
       │                  (repeats)           │
```

Single-buffer plugins on the slow path rewrite the same buffer each
frame. The `gl::Finish` before send makes this safe: the dmabuf is
GPU-complete by the time the host receives the fd.

## FramePacer state machine (plugin side)

`FramePacer` (in `veiland-plugin::lifecycle`) encapsulates which host
events translate to "render now." The two modes:

```
  SelfPaced (animated plugins: particles, sakura)

  initial state: buffer_released=true, frame_pending=false

  FrameDone arrives:
    if buffer_released → emit Render, set frame_pending=true
    else               → set frame_pending=true (deferred)

  BufferReleased arrives:
    set buffer_released=true
    if frame_pending → emit Render (continuous redraw)

  ─────────────────────────────────────────────────────

  OnDemand (static plugins: clock, wallpaper, label, vignette)

  initial state: buffer_released=true, frame_pending=false

  FrameDone arrives:
    if buffer_released → emit Render
    else               → set frame_pending=true (deferred)

  BufferReleased arrives:
    set buffer_released=true
    if frame_pending → emit Render, clear frame_pending
                       (does NOT self-schedule next render)
```

## Protocol error handling

Any violation on the host side (wrong fd count, bad dimensions,
unknown tag, buffer before Hello, etc.) triggers:

```
  HostConnection::recv_message → Err(HostError::ProtocolViolation)
       │
       ▼
  drive_plugin → PostAction::Remove  (calloop removes the socket source)
       │
       ▼
  plugin slot marked dead (PluginSlot dropped)
  child process gets SIGTERM at teardown
  lock_surfaces[i] repaint falls back to clear color for that region
```

The locker never panics on plugin input. The affected region goes black;
other plugins and the password field continue unaffected.

## Teardown

```
  veiland-core                          plugin process
       │                                      │
       │  (RunState transitions to            │
       │   UnlockedCleanly or Refused)        │
       │                                      │
       │─── Shutdown ────────────────────────►│
       │    (sent to every live plugin)       │
       │                                      │  clean exit within grace period
       │                                      │  (implementation-defined, ~1s)
       │                                      │
       │    waitpid (per plugin, sequential)  │
       │    SIGTERM if grace period expires   │
       │    SIGKILL if still alive            │
       │                                      │
       │    process exits                     │
```

`Shutdown` messages are sent to all plugins before any `waitpid` so
total teardown time is one grace period, not N.
