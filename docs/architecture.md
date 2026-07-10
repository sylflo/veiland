<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Veiland architecture (maintainer reference)

Audience: contributors modifying veiland-core internals. Plugin authors
and users do not need this document — see `plugin-api.md` and
`config.md` respectively.

## Module map

```
veiland-core  (binary)
  main.rs           AppData definition, main(), calloop event loop
  app/mod.rs        AppData inherent methods: spawn, hotplug, repaint,
                    Configure tick, plugin-socket driver
  app/compositor.rs CompositorHandler (frame callback)
  app/input.rs      KeyboardHandler (password input, PAM trigger)
  app/lock.rs       SessionLockHandler (lock acquired/denied,
                    per-surface configure, unlock)
  app/output.rs     OutputHandler (new output, output destroyed)
  auth/mod.rs       PAM session, mlock'd password buffer
  config.rs         Config load + validation (TOML → typed structs)
  region.rs         Pixel region → clip-space rect conversion
  renderer.rs       Renderer struct: EGL/GL handles, compositor program,
                    indicator program, box program, placeholder FBO
  plugin/
    mod.rs          Re-exports + shared helpers
    slot.rs         PluginSlot: static config metadata + PluginState
    state.rs        PluginState: per-frame state (texture, connection)
                    + handle_message dispatch
    connection.rs   HostConnection: SCM_RIGHTS send/recv, handshake,
                    protocol framing
    dmabuf.rs       import_dmabuf / release_texture (EGLImage ↔ GL texture)
    spawn.rs        spawn_plugin: socketpair + Command (fork + exec)
    host_spawn.rs   try_spawn_one: config → spawn_plugin + HostConnection
    sync.rs         EGL fence import / wait / release

veiland-plugin  (library, linked by plugins)
  socket.rs         Connection: plugin-side socket framing, send/recv
  lifecycle.rs      Connection::connect, wait_for_configure, FramePacer
  buffer.rs         DmaBuffer: GBM bo + EGLImage + GL FBO
  gl.rs             GbmEgl: render node open, EGL ctx, GBM device
  render.rs         GbmEgl impl (construction, resize helpers)
  sync.rs           Plugin-side fence export

veiland-protocol  (library, linked by both)
  codec.rs          Byte-level read/write primitives, version handshake
  client.rs         ClientMessage encode/decode (Hello, Buffer, BufferDestroy)
  server.rs         ServerMessage encode/decode (Configure, FrameDone,
                    BufferReleased, Shutdown)
  types.rs          Fourcc, Modifier opaque wrappers
  error.rs          ProtocolError
```

## The index-correspondence invariant

This is the most load-bearing non-obvious invariant in the codebase.
Getting it wrong silently composites the wrong plugin onto the wrong
monitor.

`AppData` holds two parallel `Vec`s:

```
lock_surfaces: Vec<Option<LockSurface>>   // one entry per connected output
plugins:       Vec<Vec<Option<PluginSlot>>> // outer index = same output
```

**The contract:** `lock_surfaces[i]` and `plugins[i]` always refer to
the same output. The renderer paints `plugins[i]` onto
`lock_surfaces[i]`.

How it is maintained:

- **Startup:** for each output, `create_lock_surface_for_output` returns
  an index `i`, then `spawn_plugins_for_output(i, …)` fills `plugins[i]`.
  Both helpers are called in sequence, in the same loop, before the
  calloop sources are registered.
- **Hotplug-in** (`process_pending_hotplug`): same two-step sequence,
  same order.
- **Hotplug-out:** `output_destroyed` sets `lock_surfaces[i] = None` and
  tears down `plugins[i]`. The slot becomes a `None` sentinel rather than
  being removed, so all indices above it stay valid.
- **Hotplug-in after hotplug-out:** `create_lock_surface_for_output`
  prefers the first `None` slot (reuse); `spawn_plugins_for_output` then
  fills the same index. Indices are stable across plug/unplug cycles for
  the lifetime of the process.

**What breaks it:** inserting into or removing from either `Vec` without
doing the same to the other at the same index. The renderer loop assumes
`plugins.len() <= lock_surfaces.len()` and iterates without bounds
checks; a length mismatch causes out-of-bounds access.

## The calloop event loop

`main()` owns one `calloop::EventLoop<AppData>`. All Wayland events and
plugin socket events run on the same thread. The one worker thread is the
auth worker: the blocking PAM call runs there so a wrong password (which
`pam_unix` delays ~2s) doesn't stall the loop. `KeyboardHandler::key`
copies the password out of the mlock'd buffer, sends it to the worker, and
sets `AuthState::Checking`; the verdict comes back over a `calloop::channel`
source and the unlock decision is committed on this thread (see below).

```
EventLoop::dispatch(16ms timeout, &mut AppData)
  └─ WaylandSource          → dispatches wl_display events
       └─ SCTK delegate handlers on AppData
            SessionLockHandler::configure  → EGL surface setup, repaint
            CompositorHandler::frame       → repaint (frame callback)
            KeyboardHandler::key           → password buffer; on Enter,
                                             send attempt to auth worker
            OutputHandler::new_output      → push to pending_outputs_arrived
            OutputHandler::output_destroyed→ hotplug-out teardown
  └─ Generic(plugin_fd)     → drive_plugin(output_idx, plugin_idx)
       └─ HostConnection::recv_message
            → PluginState::handle_message
                 → import_dmabuf (on Buffer)
                 → repaint_lock_surfaces (after new texture)
  └─ Channel(auth verdict)  → handle_auth_verdict(ok)
       → on Ok: take session lock, unlock, UnlockedCleanly
       → on Err: AuthState::Failed + 1500ms reset timer

After dispatch returns:
  process_pending_hotplug()  → drain pending_outputs_arrived,
                               create surfaces + spawn plugins
  process_periodic_tick()    → re-send Configure with updated time
                               to every live plugin every 30s
```

Plugin sockets are registered as `calloop::Generic` sources via
`register_plugin_source(output_idx, plugin_idx)`. On `EPOLLIN` the
handler calls `drive_plugin`, which reads one message and calls
`handle_message`. On EOF or protocol violation it returns
`PostAction::Remove` — calloop removes the source, and the plugin is
marked dead. The next `process_pending_hotplug` does not revive it;
the region falls back to the lock-surface clear color.

The 16 ms dispatch timeout is a worst-case wake interval. In practice
the loop is driven by frame callbacks (CompositorHandler::frame) and
plugin socket events; the timer only matters when both are quiet (a
static lockscreen with no input and no plugins).

## The trust boundary in code

```
                         process boundary
                              |
  veiland-core (trusted)      |   plugin process (untrusted)
  ─────────────────────────   |   ──────────────────────────
  auth/mod.rs                 |   veiland-plugin/
    PAM, password buffer,     |     socket.rs
    unlock decision           |     lifecycle.rs
                              |     buffer.rs
  plugin/connection.rs        |     gl.rs
    HostConnection::recv_      |
    message                   |
    validates every field     |
    before passing to EGL     |   SCM_RIGHTS (dmabuf fd)
                              |──────────────────────────>
  plugin/dmabuf.rs            |
    import_dmabuf             |
    eglCreateImage is the     |
    final modifier/format     |
    validation gate           |
```

The password buffer (`auth::Session`) is `mlock`'d and lives entirely
in trusted process memory. No plugin message maps to "unlock" — the
unlock decision path is keyboard event → password buffer → PAM → state
change. Plugins receive no keyboard events; the API surface is absent
by protocol design, not runtime filter.

The process boundary is a *protocol* boundary, not a same-UID security
boundary: plugins run as the same user as the core, so `mlock` (which
stops swapping, not reading) does not stop a hostile same-UID plugin
from `PTRACE_ATTACH`-ing the core or reading `/proc/<pid>/mem` when
`ptrace_scope=0`. The core mitigates this with `prctl(PR_SET_DUMPABLE, 0)`
at startup (`main.rs` §0; opt out with `VEILAND_ALLOW_DUMP=1`), which
denies same-UID ptrace/proc-mem and suppresses core dumps of the buffer.
That is defense-in-depth, not an absolute wall — there is no seccomp or
landlock sandbox yet, so a determined hostile *third-party* plugin is a
residual risk. First-party plugins are reviewed in-tree; third-party
plugins are same-user code the user chose to trust.

## Diagrams

- [`diagrams/component.md`](diagrams/component.md) — ownership and data
  flow between components (box-and-arrow).
- [`diagrams/buffer-lifecycle.md`](diagrams/buffer-lifecycle.md) —
  sequence diagram for the Buffer handshake: spawn through steady-state
  render loop, both fast and slow sync paths.
