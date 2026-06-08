// SPDX-License-Identifier: GPL-3.0-or-later

mod app;
mod auth;
mod config;
mod plugin;
mod region;
mod renderer;

use app::LockSurface;
use renderer::Renderer;

use std::{process::ExitCode, time::Duration};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{EventLoop, LoopHandle},
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{
        Capability, SeatHandler, SeatState,
        keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
    },
    session_lock::{
        SessionLock, SessionLockHandler, SessionLockState, SessionLockSurface,
        SessionLockSurfaceConfigure,
    },
};

use khronos_egl as egl;
use wayland_client::{
    Connection, Proxy, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_keyboard, wl_output, wl_seat, wl_surface},
};

use veiland_protocol::{ClientMessage, Configure, HOST_CAP_FENCE_FD, HostCapabilities};

use plugin::{HostConnection, PluginSlot, PluginState, spawn_plugin};

/// Whether the locker is still running, unlocked cleanly, or was
/// refused by the compositor. `main()` reads the final value to pick
/// the process exit code; the handlers and key path drive transitions.
#[derive(Default, PartialEq)]
pub(crate) enum RunState {
    #[default]
    Running,
    UnlockedCleanly,
    Refused,
}

pub(crate) struct AppData {
    conn: Connection,
    compositor_state: CompositorState,
    output_state: OutputState,
    registry_state: RegistryState,
    seat_state: SeatState,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    session_lock_state: SessionLockState,
    session_lock: Option<SessionLock>,
    lock_surfaces: Vec<Option<LockSurface>>,
    run: RunState,
    /// All host-side EGL + GL state (the shared context plus the two
    /// draw programs). Lives behind one field; access is
    /// `self.renderer.*`.
    renderer: Renderer,
    plugins: Vec<Vec<Option<PluginSlot>>>,
    auth: auth::Session,
    modifiers: Modifiers,
    /// Calloop handle for registering new plugin sockets on hotplug.
    /// Cloned once at startup; the original handle stays with the
    /// EventLoop in main().
    loop_handle: LoopHandle<'static, AppData>,
    /// Wayland queue handle for creating new lock surfaces on hotplug.
    /// Cloned once at startup.
    qh: QueueHandle<AppData>,
    /// The plugin config — owned here so the spawn helper can read it
    /// without re-plumbing. Read-only after startup. Also consulted
    /// by the hotplug-in path (`process_pending_hotplug`).
    config: config::Config,
    /// EGL fence-fd capability bit, computed once at startup. Stored
    /// on AppData so the hotplug-in path can pass it to
    /// `spawn_plugins_for_output` for newly-arrived monitors.
    host_capabilities: HostCapabilities,
    /// Outputs whose `new_output` fired during the last dispatch
    /// batch. Drained after `event_loop.dispatch()` returns, when
    /// SCTK's `OutputState` has fully processed all events from the
    /// batch (so `xdg_output.name` and friends are populated). See
    /// `process_pending_hotplug` and the M8 retrospective in
    /// docs/m8-plan.md.
    pending_outputs_arrived: Vec<(wl_output::WlOutput, String)>,
    /// Outputs whose `wl_output` proxy was rebound mid-flight
    /// (Hyprland fast-replug pattern: global_remove + global on
    /// the same local id within one dispatch batch). We need to
    /// destroy the lock surface tied to the old proxy and create
    /// a fresh one against the new proxy. Carries the new proxy.
    pending_outputs_rebound: Vec<(wl_output::WlOutput, String)>,
    /// Last time the periodic Configure tick fired. Initialised at
    /// startup; `process_periodic_tick` re-sends Configure to every
    /// alive plugin when 30s have elapsed since this. The tick is
    /// what keeps the clock plugin's display current — every Configure
    /// carries a fresh `time_unix_seconds`.
    last_time_tick: std::time::Instant,
}

fn main() -> ExitCode {
    println!("veiland-core");

    #[cfg(feature = "debug-unlock")]
    eprintln!("veiland-core: WARNING: debug-unlock feature enabled — Escape unlocks without auth");

    // --- 1. Load plugin config ----------------------------------------------
    // Plugins are declared in $VEILAND_CONFIG (dev override) or
    // $XDG_CONFIG_HOME/veiland/config.toml. See docs/config.md.
    let config = match config::load() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("veiland-core: {}; refusing to start", e);
            return ExitCode::FAILURE;
        }
    };
    if config.plugins.is_empty() {
        eprintln!(
            "veiland-core: no plugins configured; locker will show \
            only the clear color. (See docs/config.md.)"
        );
    }

    // --- 2. Wayland connection + event loop ---------------------------------
    let conn = Connection::connect_to_env()
        .expect("failed to connect to Wayland display (is WAYLAND_DISPLAY set?)");
    let (globals, mut event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();
    let mut event_loop: EventLoop<AppData> = EventLoop::try_new().expect("calloop event loop");

    // --- 3. EGL setup (host's GL context, shared across plugin imports) -----
    let egl = egl::Instance::new(egl::Static);
    let display_ptr = conn.backend().display_ptr();
    // SAFETY: display_ptr came from a live wayland_client::Connection.
    let egl_display =
        unsafe { egl.get_display(display_ptr as *mut std::ffi::c_void) }.expect("get EGL display");
    egl.initialize(egl_display)
        .expect("egl failed to initialize");
    let has_fence_fd = egl
        .query_string(Some(egl_display), egl::EXTENSIONS)
        .expect("query EGL extensions")
        .to_str()
        .expect("EGL extensions string is not UTF-8")
        .split(' ')
        .any(|ext| ext == "EGL_ANDROID_native_fence_sync");
    let host_capabilities: HostCapabilities = if has_fence_fd { HOST_CAP_FENCE_FD } else { 0 };
    if !has_fence_fd {
        eprintln!("veiland-core: EGL_ANDROID_native_fence_sync not available — falling back");
        eprintln!("veiland-core: to M3 sync model (one frame at a time). Locker works but");
        eprintln!("veiland-core: animated plugins may stutter on heavy workloads. Cause:");
        eprintln!("veiland-core: GPU driver lacks the extension (old NVIDIA, software");
        eprintln!("veiland-core: rasterizer, etc.).");
    }
    egl.bind_api(egl::OPENGL_ES_API)
        .expect("Failed to bind OPENGL_ES_API");
    gl::load_with(|name| {
        egl.get_proc_address(name)
            .map(|p| p as *const _)
            .unwrap_or(std::ptr::null())
    });
    let config_attribs = [
        egl::SURFACE_TYPE,
        egl::WINDOW_BIT,
        egl::RENDERABLE_TYPE,
        egl::OPENGL_ES2_BIT,
        egl::RED_SIZE,
        8,
        egl::GREEN_SIZE,
        8,
        egl::BLUE_SIZE,
        8,
        egl::ALPHA_SIZE,
        8,
        egl::NONE,
    ];
    let egl_config = egl
        .choose_first_config(egl_display, &config_attribs)
        .expect("choose EGL config")
        .expect("no matching EGL config");
    let context_attribs = [egl::CONTEXT_CLIENT_VERSION, 2, egl::NONE];
    let egl_context = egl
        .create_context(egl_display, egl_config, None, &context_attribs)
        .expect("create EGL context");
    // Surfaceless current — we need the context current before the first
    // dmabuf import (which happens before any lock-surface configure).
    egl.make_current(egl_display, None, None, Some(egl_context))
        .expect("eglMakeCurrent (surfaceless)");

    // Build both GL programs and bundle all EGL/GL handles into the
    // Renderer. The context is already current (surfaceless) above,
    // which the program build requires.
    let renderer = Renderer::new(egl, egl_display, egl_config, egl_context);

    let auth = match auth::Session::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("veiland-core: failed to allocate password buffer: {}", e);
            eprintln!("veiland-core: check RLIMIT_MEMLOCK (ulimit -l)");
            return ExitCode::FAILURE;
        }
    };

    // --- 5. AppData and lock surface ----------------------------------------
    let mut state = AppData {
        conn: conn.clone(),
        compositor_state: CompositorState::bind(&globals, &qh)
            .expect("wl_compositor not advertised"),
        output_state: OutputState::new(&globals, &qh),
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        keyboard: None,
        session_lock_state: SessionLockState::new(&globals, &qh),
        session_lock: None,
        lock_surfaces: Vec::new(),
        run: RunState::Running,
        renderer,
        plugins: Vec::new(),
        auth,
        loop_handle: event_loop.handle(),
        qh: qh.clone(),
        config: config.clone(),
        modifiers: Modifiers::default(),
        host_capabilities,
        pending_outputs_arrived: Vec::new(),
        pending_outputs_rebound: Vec::new(),
        last_time_tick: std::time::Instant::now(),
    };

    // xdg_output.name arrives async after registry bind; without a roundtrip
    // here we'd hit the create-lock-surface loop below before SCTK has the
    // names and log "<unnamed>" for every output. One sync round-trip is
    // enough — all pending output events have been dispatched by the time
    // it returns.
    event_queue
        .roundtrip(&mut state)
        .expect("roundtrip for output names");

    let output_names: Vec<String> = state
        .output_state
        .outputs()
        .map(|o| {
            state
                .output_state
                .info(&o)
                .and_then(|i| i.name)
                .unwrap_or_else(|| "<unnamed>".into())
        })
        .collect();

    // Warn about monitors entries that didn't match any connected output.
    // A typo'd name shouldn't fail the locker, but the user wants to know
    // their config is silently doing nothing.
    for entry in &config.plugins {
        let Some(requested) = &entry.monitors else {
            continue;
        };
        for requested_name in requested {
            if !output_names.iter().any(|name| name == requested_name) {
                eprintln!(
                    "veiland-core: plugin {:?} requested output {:?} \
                    but no connected output has that name (typo? \
                    check `hyprctl monitors` / `swaymsg -t get_outputs`). \
                    Spawning zero instances for this output.",
                    entry.name, requested_name
                );
            }
        }
    }

    let session_lock = state
        .session_lock_state
        .lock(&qh)
        .expect("ext-session-lock not supported");
    state.session_lock = Some(session_lock);

    // --- 4. Spawn plugins + create lock surfaces per output ------------------
    // Create lock surface first (returns its index), then spawn plugins at
    // the matching index. Same call pattern as `process_pending_hotplug`'s
    // arrival path — startup and runtime use identical plumbing.
    // Collect outputs into an owned vec first because the helpers take
    // &mut self and SCTK's outputs() iterator borrows output_state.
    let initial_outputs: Vec<(wl_output::WlOutput, String)> = state
        .output_state
        .outputs()
        .map(|o| {
            let name = state
                .output_state
                .info(&o)
                .and_then(|i| i.name)
                .unwrap_or_else(|| "<unnamed>".to_string());
            (o, name)
        })
        .collect();
    for (output, name) in &initial_outputs {
        if let Some(idx) = state.create_lock_surface_for_output(output, name.clone()) {
            state.spawn_plugins_for_output(idx, name);
        }
    }
    // Discard anything `new_output` collected during the startup
    // roundtrip — we've already handled those outputs explicitly
    // here. The drain-after-dispatch path is for *real* hotplug.
    state.pending_outputs_arrived.clear();
    state.pending_outputs_rebound.clear();

    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .unwrap();

    // --- 6. Register each plugin's socket as a calloop event source ---------
    // The helper handles None slots by no-oping; we just blanket-call.
    // On hotplug, update_output uses the same helper for newly-arrived
    // plugins (step 5c). On plugin death drive_plugin returns
    // PostAction::Remove and the source self-cleans.
    for o in 0..state.plugins.len() {
        for p in 0..state.plugins[o].len() {
            state.register_plugin_source(o, p);
        }
    }

    // --- 7. Main loop --------------------------------------------------------
    while matches!(state.run, RunState::Running) {
        event_loop
            .dispatch(Duration::from_millis(16), &mut state)
            .expect("event loop dispatch");
        // Drain any topology changes the just-finished dispatch
        // collected. By now SCTK's OutputState has fully processed
        // them, so creating/recreating lock surfaces here is safe.
        // See AppData::process_pending_hotplug.
        state.process_pending_hotplug();
        // Re-send Configure with a fresh `time_unix_seconds` if 30 s
        // have elapsed. Keeps clock plugins current without a
        // dedicated TimeTick message. See M11 step 2.
        state.process_periodic_tick();
    }

    // --- 8. Plugin teardown -------------------------------------------------
    // Polite shutdown sequence per plugin: ask, wait, SIGTERM, wait, SIGKILL.
    // Send Shutdown to every live plugin first, then wait per-plugin — for
    // N plugins this caps total teardown at one grace period, not N.
    for per_output in state.plugins.iter_mut() {
        for slot_opt in per_output.iter_mut() {
            if let Some(slot) = slot_opt {
                if let Err(e) = slot.state.connection.send_shutdown() {
                    eprintln!(
                        "teardown: plugin {:?} send_shutdown failed: {} (continuing)",
                        slot.name, e
                    );
                }
            }
        }
    }

    for per_output in state.plugins.iter_mut() {
        for slot_opt in per_output.iter_mut() {
            if let Some(slot) = slot_opt.take() {
                teardown_one_plugin(slot);
            }
        }
    }

    match state.run {
        RunState::Running => unreachable!(),
        RunState::UnlockedCleanly => {
            println!("unlocked, exiting");
            ExitCode::SUCCESS
        }
        RunState::Refused => ExitCode::FAILURE,
    }
}

/// Does this plugin entry's `monitors` filter (if any) admit the
/// given output? `None` means "any output"; `Some(list)` means
/// "exactly the names in this list" (case-sensitive, exact match).
pub(crate) fn entry_matches_output(entry: &config::PluginEntry, output_name: &str) -> bool {
    match &entry.monitors {
        None => true,
        Some(list) => list.iter().any(|name| name == output_name),
    }
}

pub(crate) fn try_spawn_one(
    entry: &config::PluginEntry,
    output_name: &str,
    scale: u32,
    host_capabilities: HostCapabilities,
    egl: &egl::Instance<egl::Static>,
    display: egl::Display,
) -> Result<PluginSlot, plugin::HostError> {
    // Serialise the plugin's [plugin.config] table to JSON if present.
    // Failure here is a host bug (every TOML value round-trips through
    // serde_json cleanly); we log and proceed with no config rather
    // than refusing to spawn the plugin, since the plugin may have
    // sensible defaults.
    let config_json = entry
        .config
        .as_ref()
        .and_then(|v| match serde_json::to_string(v) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!(
                    "veiland-core: plugin {:?}: failed to serialise [plugin.config] to JSON: {} \
                     — spawning without VEILAND_PLUGIN_CONFIG",
                    entry.name, e
                );
                None
            }
        });
    let process = spawn_plugin(&entry.binary, &entry.name, config_json.as_deref())?;
    let mut connection = HostConnection::from_fd(process.socket, host_capabilities);
    connection.handshake()?;
    eprintln!("plugin {:?}: handshake ok", entry.name);

    // recv_message has already enforced "Hello carries no fds" at the
    // wire layer (any fd on a non-Buffer message is ProtocolViolation
    // there). We just need to reject a misbehaving plugin that sent
    // the wrong variant as its first message — never panic, plugin
    // input must not crash the locker.
    let (msg, _fds) = connection.recv_message()?;
    let (hello_name, hello_version) = match msg {
        ClientMessage::Hello(h) => (h.plugin_name, h.plugin_version),
        _ => {
            return Err(plugin::HostError::ProtocolViolation(
                "first message was not Hello",
            ));
        }
    };
    eprintln!(
        "plugin {:?}: says hello: {} v{}",
        entry.name, hello_name, hello_version
    );
    // Build PluginState and feed the Hello through handle_message so
    // the state machine records name/version through the canonical path.
    let mut state = PluginState::new(connection);
    state.handle_message(
        ClientMessage::Hello(veiland_protocol::Hello {
            plugin_name: hello_name.clone(),
            plugin_version: hello_version.clone(),
        }),
        plugin::ReceivedFds::None,
        egl,
        display,
    )?;

    // Initial Configure. Region is still hardcoded full-screen
    // 1920x1080 here; step 3 makes this region-aware.
    let (time_unix_seconds, time_tz_offset_seconds) = current_time_for_configure();
    let initial_configure = Configure {
        region_x: 0,
        region_y: 0,
        region_w: 1920,
        region_h: 1080,
        scale,
        time_unix_seconds,
        time_tz_offset_seconds,
        output_name: output_name.to_string(),
    };
    state.connection.send_configure(initial_configure.clone())?;
    state.connection.send_frame_done()?;

    Ok(PluginSlot {
        state,
        pid: process.child_pid,
        name: entry.name.clone(),
        binary: entry.binary.clone(),
        z_index: entry.z_index,
        region: entry.region.clone(),
        output_name: output_name.to_string(),
        last_configure: Some(initial_configure),
    })
}

/// Snapshot the wall clock into `(unix_seconds, tz_offset_seconds)`,
/// the two fields a plugin needs to render a localised clock without
/// reading the system time itself. Computing `time_tz_offset_seconds`
/// from `chrono::Local` honours DST transitions automatically — the
/// plugin doesn't need to know the timezone, just the offset that
/// was in effect at this instant.
pub(crate) fn current_time_for_configure() -> (i64, i32) {
    let now = chrono::Local::now();
    (now.timestamp(), now.offset().local_minus_utc())
}

/// Wind down the plugin: send Shutdown, give it ~250ms to exit on its own,
/// then SIGTERM, then SIGKILL. Reaps the zombie. Best-effort — if any step
/// fails we log and continue, because at this point the host is exiting
/// anyway and refusing to exit would be worse than a leaked plugin.
pub(crate) fn teardown_one_plugin(slot: PluginSlot) {
    use nix::sys::signal::{Signal, kill};
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};

    // Shutdown was already sent in the loop above; here we just wait.

    // Grace period. The spec says "implementation-defined"; 250ms is
    //    enough for a well-behaved plugin to exit and short enough that
    //    a session-unlock doesn't feel laggy.
    let grace = Duration::from_millis(250);
    let deadline = std::time::Instant::now() + grace;
    loop {
        match waitpid(slot.pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {
                if std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(_) => {
                eprintln!("teardown: plugin {:?} exited cleanly", slot.name);
                return;
            }
            Err(e) => {
                eprintln!(
                    "teardown: plugin {:?} waitpid failed: {} (continuing)",
                    slot.name, e
                );
                return;
            }
        }
    }

    // 3. SIGTERM, brief wait.
    eprintln!(
        "teardown: plugin {:?} did not exit in {}ms, sending SIGTERM",
        slot.name,
        grace.as_millis()
    );
    let _ = kill(slot.pid, Signal::SIGTERM);
    std::thread::sleep(Duration::from_millis(100));
    if let Ok(status) = waitpid(slot.pid, Some(WaitPidFlag::WNOHANG))
        && !matches!(status, WaitStatus::StillAlive)
    {
        eprintln!("teardown: plugin {:?} reaped after SIGTERM", slot.name);
        return;
    }

    // 4. SIGKILL, reap, done.
    eprintln!(
        "teardown: plugin {:?} still alive, sending SIGKILL",
        slot.name
    );
    let _ = kill(slot.pid, Signal::SIGKILL);
    let _ = waitpid(slot.pid, None);
}

impl SessionLockHandler for AppData {
    fn locked(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _session_lock: SessionLock) {
        println!("locked. Press Escape to unlock.");
    }

    fn finished(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _session_lock: SessionLock,
    ) {
        eprintln!("Compositor refused the lock");
        self.run = RunState::Refused;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        session_lock_surface: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _serial: u32,
    ) {
        let (width, height) = configure.new_size;
        eprintln!(
            "[M8-TRACE] configure ENTER: wl_surface={:?} size=({}x{})",
            session_lock_surface.wl_surface().id(),
            width,
            height,
        );

        let target = session_lock_surface.wl_surface();
        // Look up the surface in our vec. Returning None here is *not*
        // a panic-worthy case: hotplug-out can put a Configure event
        // for the now-departed surface ahead of our output_destroyed
        // handler in the queue, so by the time we see the Configure
        // the slot is already None. (Or the compositor sent Configure
        // for a surface we never tracked — also not our problem to die
        // on.) Log and skip; the matching surface, if it ever existed,
        // is being torn down through the proper path.
        let Some(output_idx) = self.lock_surfaces.iter().position(|ls| {
            ls.as_ref()
                .map(|ls| ls.lock_surface.wl_surface() == target)
                .unwrap_or(false)
        }) else {
            eprintln!(
                "veiland-core: ignoring Configure for unknown/departed lock surface \
                ({}x{}) — the matching output was probably just unplugged",
                width, height
            );
            return;
        };
        let entry = self.lock_surfaces[output_idx]
            .as_mut()
            .expect("just matched Some");
        if entry.egl_window.is_none() {
            let egl_window =
                wayland_egl::WlEglSurface::new(target.id(), width as i32, height as i32)
                    .expect("WlEglSurface::new");

            let egl_surface = unsafe {
                self.renderer.egl.create_window_surface(
                    self.renderer.egl_display,
                    self.renderer.egl_config,
                    egl_window.ptr() as egl::NativeWindowType,
                    None,
                )
            }
            .expect("create_window_surface");
            entry.egl_window = Some(egl_window);
            entry.egl_surface = Some(egl_surface);
            println!(
                " -> [{}] created EGL surface ({}x{})",
                entry.name, width, height
            );
        } else {
            entry
                .egl_window
                .as_ref()
                .unwrap()
                .resize(width as i32, height as i32, 0, 0);
            println!(
                " -> [{}] resized EGL surface ({}x{})",
                entry.name, width, height
            );
        }

        // Copy the egl::Surface out so `entry`'s mutable borrow
        // ends here; we need an immutable self borrow later for
        // draw_password_indicator. egl::Surface is Copy (the rest
        // of this function already deref-copies it via *egl_surface).
        let egl_surface = *entry.egl_surface.as_ref().unwrap();
        self.renderer.egl
            .make_current(
                self.renderer.egl_display,
                Some(egl_surface),
                Some(egl_surface),
                Some(self.renderer.egl_context),
            )
            .expect("eglMakeCurrent");

        // Bootstrap paint: solid black, no plugin composition. Plugins
        // typically haven't shipped a Buffer yet at this point, so
        // sampling their (empty) slots here would show a frame with
        // gaps that the next paint then fills in — visible as
        // flicker / see-through against partially-transparent plugins
        // (vignette especially). Keeping the bootstrap content-free
        // means the *first* real paint is the one with all plugins,
        // not the second.
        //
        // We still need to swap once so the compositor has a frame to
        // show and our `wl_surface.frame` request has something to
        // attach to.
        unsafe {
            gl::Viewport(0, 0, width as i32, height as i32);
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }

        // Request a frame callback so the cadence loop starts from
        // frame one. Without this, the surface only gets repainted
        // when something else dirties it (first plugin Buffer, key
        // press) — and until then the compositor double-buffer
        // alternates between this bootstrap frame and whatever
        // garbage was in the back buffer. Same ordering rule as
        // repaint_lock_surface: request BEFORE swap_buffers, because
        // swap commits.
        let target_clone = target.clone();
        target_clone.frame(&self.qh, target_clone.clone());
        if let Some(entry) = self.lock_surfaces[output_idx].as_mut() {
            entry.frame_callback_pending = true;
            // Leave needs_paint = true so the first frame-callback
            // firing immediately repaints with plugin content.
        }

        self.renderer.egl
            .swap_buffers(self.renderer.egl_display, egl_surface)
            .expect("eglSwapBuffers");
    }
}

impl CompositorHandler for AppData {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        // SCTK demuxes wl_callback.done events for every surface we
        // requested .frame() on. We match the surface back to its
        // output by linear scan — at most ≤8 outputs in practice, and
        // the comparison style matches the M8 hotplug-rebind detection
        // elsewhere in this file.
        let output_idx = self.lock_surfaces.iter().position(|entry| {
            entry
                .as_ref()
                .is_some_and(|e| e.lock_surface.wl_surface() == surface)
        });
        let Some(output_idx) = output_idx else {
            // Stale callback for a destroyed lock surface (hotplug-out
            // race). Not an error — drop it.
            return;
        };

        // The callback we requested is consumed. repaint_lock_surface
        // requests a fresh one if it actually paints.
        if let Some(entry) = self.lock_surfaces[output_idx].as_mut() {
            entry.frame_callback_pending = false;
        }

        // Paint only if dirty. When nothing is animating we don't
        // request another callback — a static lockscreen stops
        // burning 60Hz no-op repaints.
        self.repaint_lock_surface(output_idx);
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for AppData {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // Defer hotplug-in to `process_pending_hotplug` (called after
        // each `event_loop.dispatch()`). Doing EGL/session_lock work
        // synchronously inside the handler is unsafe: SCTK is mid-way
        // through processing the current event batch and may not have
        // finished binding the new wl_output globally yet. The
        // deferred drain runs after SCTK's internal state has
        // settled. See the M8 retrospective in docs/m8-plan.md
        // for the trace evidence and design rationale.
        //
        // Hyprland twist: when a monitor is unplugged, Hyprland
        // sometimes re-advertises the *surviving* monitor's wl_output
        // under a new global. SCTK fires `new_output` for that new
        // global, with a name that matches our existing entry. The
        // server-side state is now tied to the new global; using
        // the old wl_output proxy on the next commit produces
        // "invalid object N". Detection: if `name` already has a
        // lock surface, route this to the rebound queue instead of
        // the arrival queue. The drain's rebound path destroys
        // the affected lock surface and creates a fresh one against
        // the new proxy. (Sway doesn't re-advertise, so this branch
        // never fires there and behavior is unchanged.)
        let name = self
            .output_state
            .info(&output)
            .and_then(|i| i.name)
            .unwrap_or_else(|| "<unnamed>".to_string());
        let already_have = self
            .lock_surfaces
            .iter()
            .any(|s| s.as_ref().map(|ls| ls.name == name).unwrap_or(false));
        if already_have {
            eprintln!(
                "[M8-TRACE] new_output fired: {:?} (REBIND of existing name, queued)",
                name
            );
            self.pending_outputs_rebound.push((output, name));
        } else {
            eprintln!("[M8-TRACE] new_output fired: {:?} (queued)", name);
            self.pending_outputs_arrived.push((output, name));
        }
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // `update_output` fires for two distinct reasons we have to
        // distinguish:
        //
        // (a) Mode/scale change on a still-alive output. The wl_output
        //     proxy identity is the same; nothing to do here (a future
        //     M-step may want to re-send Configure to plugins with the
        //     new size).
        //
        // (b) SCTK rebound the wl_output global after a topology event
        //     (Hyprland fast-replug pattern: global_remove + global on
        //     the same local id within one batch). The proxy identity
        //     is DIFFERENT from what we have stored on `LockSurface`,
        //     and we need to destroy + recreate our lock surface
        //     against the fresh proxy. Otherwise the next commit on
        //     the still-alive surface trips "invalid object" because
        //     dmabuf-feedback / scanout state references the rebound
        //     identity.
        //
        // Discriminator: `output.id() != stored.wl_output.id()`.
        // ObjectId equality is Arc-based on the proxy's alive flag,
        // not on the wire-level id — so even when SCTK re-uses local
        // id 12 for the new binding, the new proxy's ObjectId is
        // distinct from the released one.
        let name = self
            .output_state
            .info(&output)
            .and_then(|i| i.name)
            .unwrap_or_else(|| "<unnamed>".to_string());
        let rebound = self
            .lock_surfaces
            .iter()
            .filter_map(|s| s.as_ref())
            .find(|ls| ls.name == name)
            .map(|ls| ls.wl_output.id() != output.id())
            .unwrap_or(false);
        if rebound {
            eprintln!(
                "[M8-TRACE] update_output fired: {:?} (REBOUND, queued for recreate)",
                name
            );
            self.pending_outputs_rebound.push((output, name));
        } else {
            eprintln!(
                "[M8-TRACE] update_output fired: {:?} (mode/scale change, no-op)",
                name
            );
        }
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // The compositor has already torn down the server-side
        // SessionLockSurface for this output; touching its EGL surface
        // from here on would block in eglSwapBuffers waiting on a
        // commit no one will ever ack. We:
        //   1. Resolve the departed output's name (so we can find our
        //      matching slot — names are the only stable identity we
        //      keep on LockSurface).
        //   2. Tear down every plugin instance for that output via the
        //      existing Shutdown → grace → SIGTERM → SIGKILL sequence.
        //      Plugin calloop sources self-remove via PostAction::Remove
        //      when drive_plugin next sees EOF on the dropped socket.
        //   3. Replace both the lock_surfaces and plugins slots with
        //      None sentinels — preserves the (o, p) indices captured
        //      in surviving calloop closures.
        // Defensive throughout: this is compositor-driven input, never
        // crash on it.
        let name = match self.output_state.info(&output).and_then(|i| i.name) {
            Some(n) => n,
            None => {
                eprintln!(
                    "veiland-core: output_destroyed fired for an output \
                    with no cached name; skipping teardown (would not \
                    know which lock surface to tear down)"
                );
                eprintln!("[M8-TRACE] output_destroyed RETURNING (no name)");
                return;
            }
        };
        eprintln!("[M8-TRACE] output_destroyed ENTER: {:?}", name);

        let output_idx = self
            .lock_surfaces
            .iter()
            .position(|opt| opt.as_ref().map(|ls| ls.name == name).unwrap_or(false));
        let Some(output_idx) = output_idx else {
            eprintln!(
                "veiland-core: output_destroyed for {:?} but no matching \
                lock surface; nothing to tear down",
                name
            );
            eprintln!(
                "[M8-TRACE] output_destroyed RETURNING (no matching surface): {:?}",
                name
            );
            return;
        };

        eprintln!(
            "veiland-core: output {} disconnected, tearing down {} plugin instance(s)",
            name,
            self.plugins[output_idx]
                .iter()
                .filter(|s| s.is_some())
                .count()
        );

        // Phase 1: send Shutdown to every live plugin on this output.
        // Errors are non-fatal — the next phase will SIGTERM/SIGKILL
        // anything that's not already dying.
        for slot_opt in self.plugins[output_idx].iter_mut() {
            if let Some(slot) = slot_opt {
                if let Err(e) = slot.state.connection.send_shutdown() {
                    eprintln!(
                        "veiland-core: hotplug teardown: plugin {:?} \
                        send_shutdown failed: {} (continuing)",
                        slot.name, e
                    );
                }
            }
        }

        // Phase 2: take each slot and run the per-plugin teardown
        // (grace period, escalate to SIGTERM/SIGKILL, reap zombie).
        for slot_opt in self.plugins[output_idx].iter_mut() {
            if let Some(slot) = slot_opt.take() {
                teardown_one_plugin(slot);
            }
        }

        // Phase 3: tear down EGL bits *before* dropping the LockSurface.
        // wayland_egl::WlEglSurface holds an internal reference to the
        // wl_surface; explicit destroy first keeps EGL from sending
        // commits to a dying surface.
        if let Some(surface_ref) = self.lock_surfaces[output_idx].as_mut() {
            if let Some(egl_surface) = surface_ref.egl_surface.take() {
                if let Err(e) = self.renderer.egl.destroy_surface(self.renderer.egl_display, egl_surface) {
                    eprintln!(
                        "veiland-core: eglDestroySurface for {:?} failed: {:?} (continuing)",
                        surface_ref.name, e
                    );
                }
            }
            // WlEglSurface drops via the take() leaving None.
            surface_ref.egl_window = None;
        }
        // Phase 4: drop the SessionLockSurface, letting SCTK's
        // SessionLockSurfaceInner::Drop send ext_session_lock_surface_v1
        // .destroy() to the compositor. SCTK runs *our* output_destroyed
        // handler BEFORE releasing the wl_output (SCTK 0.20
        // src/output.rs lines 909-918), so destroying the lock surface
        // here happens while the wl_output is still alive — matching
        // swaylock / hyprlock's destruction order:
        //
        //     ext_session_lock_surface_v1.destroy()  ← our Drop here
        //     wl_surface.destroy()                   ← chained Drops
        //     wl_output.release()                    ← SCTK after we return
        //
        // The earlier `mem::forget` was a M7-debug-cycle empirical
        // workaround that traded a Drop-time crash for a later
        // swap-time crash on the surviving monitor (Hyprland) and
        // stranded keyboard focus on the destroyed surface (Sway —
        // the compositor doesn't re-route until the surface object
        // is actually destroyed). Both researched references just
        // drop the surface — see hyprlock's
        // src/core/hyprlock.cpp setGlobalRemove and swaylock-plugin's
        // main.c destroy_surface.
        self.lock_surfaces[output_idx] = None;
        eprintln!(
            "veiland-core: output {} teardown complete; slot is now None",
            name
        );
        eprintln!("[M8-TRACE] output_destroyed RETURNING (normal): {:?}", name);
    }
}

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

impl SeatHandler for AppData {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            println!("Set keyboard capability");
            let keyboard = self
                .seat_state
                .get_keyboard(qh, &seat, None)
                .expect("Failed to create keyboard");
            self.keyboard = Some(keyboard);
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_some() {
            println!("Unset keyboard capability");
            self.keyboard.take().unwrap().release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for AppData {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.handle_key(&event);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.handle_key(&event);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }
}

// SCTK delegate macros — must come after the *Handler impls they delegate to.
smithay_client_toolkit::delegate_compositor!(AppData);
smithay_client_toolkit::delegate_output!(AppData);
smithay_client_toolkit::delegate_seat!(AppData);
smithay_client_toolkit::delegate_keyboard!(AppData);
smithay_client_toolkit::delegate_registry!(AppData);
smithay_client_toolkit::delegate_session_lock!(AppData);
wayland_client::delegate_noop!(AppData: ignore wl_buffer::WlBuffer);
