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
    compositor::CompositorState,
    output::OutputState,
    reexports::{
        calloop::{EventLoop, LoopHandle},
        calloop_wayland_source::WaylandSource,
    },
    registry::{ProvidesRegistryState, RegistryState},
    registry_handlers,
    seat::{SeatState, keyboard::Modifiers},
    session_lock::{SessionLock, SessionLockState},
};

use khronos_egl as egl;
use wayland_client::{
    Connection, QueueHandle,
    globals::registry_queue_init,
    protocol::{wl_buffer, wl_keyboard, wl_output},
};

use veiland_protocol::{HOST_CAP_FENCE_FD, HostCapabilities};

use plugin::{PluginSlot, teardown_one_plugin};

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
    let renderer = match Renderer::new(egl, egl_display, egl_config, egl_context) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("veiland-core: GL program build failed: {e}");
            eprintln!("veiland-core: check EGL context and driver shader compiler");
            return ExitCode::FAILURE;
        }
    };

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
        for slot in per_output.iter_mut().flatten() {
            if let Err(e) = slot.state.connection.send_shutdown() {
                eprintln!(
                    "teardown: plugin {:?} send_shutdown failed: {} (continuing)",
                    slot.name, e
                );
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

impl ProvidesRegistryState for AppData {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }
    registry_handlers![OutputState, SeatState];
}

// SCTK delegate macros — must come after the *Handler impls they delegate to.
smithay_client_toolkit::delegate_compositor!(AppData);
smithay_client_toolkit::delegate_output!(AppData);
smithay_client_toolkit::delegate_seat!(AppData);
smithay_client_toolkit::delegate_keyboard!(AppData);
smithay_client_toolkit::delegate_registry!(AppData);
smithay_client_toolkit::delegate_session_lock!(AppData);
wayland_client::delegate_noop!(AppData: ignore wl_buffer::WlBuffer);
