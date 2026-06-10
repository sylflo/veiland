<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Component diagram

Ownership and data flow between veiland's runtime components.
Arrows show data/control flow direction; containment shows ownership.

```
┌─────────────────────────────────────────────────────────────────────┐
│  veiland-core process  (trusted)                                    │
│                                                                     │
│  ┌──────────────────────────────────────────────────────────────┐  │
│  │  AppData                                                     │  │
│  │                                                              │  │
│  │  ┌─────────────┐   ┌───────────────────────────────────┐    │  │
│  │  │  auth::     │   │  Renderer                         │    │  │
│  │  │  Session    │   │                                   │    │  │
│  │  │             │   │  EGL display / context / config   │    │  │
│  │  │  mlock'd    │   │  compositor GL program            │    │  │
│  │  │  password   │   │  indicator GL program             │    │  │
│  │  │  buffer     │   │  box GL program                   │    │  │
│  │  │             │   │  placeholder FBO (per surface sz) │    │  │
│  │  │  PAM handle │   └──────────────┬────────────────────┘    │  │
│  │  └──────┬──────┘                  │ GL draw calls            │  │
│  │         │ PAM auth                │                          │  │
│  │         ▼                         ▼                          │  │
│  │  ┌──────────────────────────────────────────────────────┐    │  │
│  │  │  lock_surfaces[i]: Option<LockSurface>               │    │  │
│  │  │                                                      │    │  │
│  │  │  wl_output  lock_surface  egl_window  egl_surface    │◄───┼──┼── compositor sends
│  │  │  surface_size  needs_paint  frame_callback_pending   │    │  │   wl_surface.frame
│  │  └──────────────────────────────────────────────────────┘    │  │
│  │         ▲                                                     │  │
│  │         │  index i must match ──────────────────────┐        │  │
│  │         │                                           │        │  │
│  │  ┌──────────────────────────────────────────────────▼────┐   │  │
│  │  │  plugins[i][j]: Option<PluginSlot>                    │   │  │
│  │  │                                                       │   │  │
│  │  │  name  binary  z_index  region  output_name           │   │  │
│  │  │  last_configure  pid                                  │   │  │
│  │  │                                                       │   │  │
│  │  │  ┌──────────────────────────────────────────────┐    │   │  │
│  │  │  │  PluginState                                 │    │   │  │
│  │  │  │                                              │    │   │  │
│  │  │  │  ┌──────────────┐   ┌────────────────────┐  │    │   │  │
│  │  │  │  │ HostConnection│  │ GlTexture (option) │  │    │   │  │
│  │  │  │  │               │  │                    │  │    │   │  │
│  │  │  │  │ UnixStream    │  │ EGLImage           │  │    │   │  │
│  │  │  │  │ (SeqPacket)   │  │ GL texture name    │  │    │   │  │
│  │  │  │  └──────┬────────┘  └────────┬───────────┘  │    │   │  │
│  │  │  │         │                    │               │    │   │  │
│  │  │  └─────────┼────────────────────┼───────────────┘    │   │  │
│  │  └────────────┼────────────────────┼───────────────────-┘   │  │
│  └───────────────┼────────────────────┼────────────────────────┘  │
│                  │                    │                             │
│    calloop       │ SCM_RIGHTS         │ sampled each               │
│    EPOLLIN       │ (dmabuf fd)        │ repaint                    │
│                  │                    │                             │
└──────────────────┼────────────────────┼─────────────────────────────┘
                   │  AF_UNIX           │ GPU memory
                   │  SOCK_SEQPACKET    │ (DMA-BUF, zero-copy)
                   │                   │
┌──────────────────┼────────────────────┼─────────────────────────────┐
│  plugin process  │  (untrusted)       │                             │
│                  │                    │                             │
│  ┌───────────────▼────────────────────▼───────────────────────┐    │
│  │  DmaBuffer                                                  │    │
│  │                                                             │    │
│  │  GBM buffer object ──export──► dmabuf fd ─► SCM_RIGHTS ──► │    │
│  │       ▲                                                     │    │
│  │       │ render into                                         │    │
│  │  GL FBO (plugin's own EGL context)                          │    │
│  └─────────────────────────────────────────────────────────────┘    │
│                                                                     │
│  Connection (veiland-plugin)   FramePacer                          │
│  ── version handshake                                              │
│  ── Hello / Buffer / BufferDestroy                                 │
│  ◄─ Configure / FrameDone / BufferReleased / Shutdown              │
└─────────────────────────────────────────────────────────────────────┘
```

## Key data flows

| Flow | Path |
|---|---|
| Lock surface repaint | `CompositorHandler::frame` → `repaint_lock_surface` → `PluginState::composite` (GL draw) → `eglSwapBuffers` |
| Plugin buffer import | calloop `EPOLLIN` → `drive_plugin` → `HostConnection::recv_message` → `PluginState::handle_message` → `import_dmabuf` → `GlTexture` stored on `PluginState` |
| Password unlock | `KeyboardHandler::key` → `auth::Session::push_char` → on Enter: `auth::Session::verify` (PAM) → `AppData::run = UnlockedCleanly` |
| Time tick | `process_periodic_tick` (every 30s) → `HostConnection::send_configure` to every live plugin with updated `time_unix_seconds` |
| Monitor hotplug-in | `OutputHandler::new_output` → `pending_outputs_arrived.push` → after dispatch: `process_pending_hotplug` → `create_lock_surface_for_output(i)` + `spawn_plugins_for_output(i)` + `register_plugin_source` |

## Multi-monitor layout

For a config with N `[[plugin]]` entries and M connected outputs:

```
plugins[0]  →  output 0 (e.g. HDMI-A-1)
  [0]: Option<PluginSlot>  ← plugin entry 0, output 0
  [1]: Option<PluginSlot>  ← plugin entry 1, output 0
  ...

plugins[1]  →  output 1 (e.g. DP-1)
  [0]: Option<PluginSlot>  ← plugin entry 0, output 1
  [1]: Option<PluginSlot>  ← plugin entry 1, output 1
  ...
```

`plugins[i][j]` is `None` when entry `j` has a `monitors` filter that
excludes output `i`, or when the spawn failed. The renderer iterates
`plugins[i].iter().flatten()` — `None` slots are silently skipped.
