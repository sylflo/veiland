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

use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1;

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

/// Visual state of the password field. Drives colour overrides in the renderer.
/// `Checking` is set while the auth worker runs the PAM call off the event
/// loop; today it renders like `Idle` (the win is that scenes keep animating).
#[derive(Default, PartialEq, Clone, Copy)]
pub(crate) enum AuthState {
    #[default]
    Idle,
    Checking,
    Failed,
}

/// One password attempt handed to the auth worker thread. The mlock'd
/// buffer stays behind on the main thread; only this owned CString
/// crosses over (see `auth::Session::take_password`).
pub(crate) struct AuthRequest {
    user: String,
    password: std::ffi::CString,
}

/// The worker's verdict for one attempt, sent back over a calloop
/// channel and handled on the main thread. `true` = authenticated.
pub(crate) type AuthOutcome = bool;

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
    auth_state: AuthState,
    /// All host-side EGL + GL state (the shared context plus the two
    /// draw programs). Lives behind one field; access is
    /// `self.renderer.*`.
    renderer: Renderer,
    plugins: Vec<Vec<Option<PluginSlot>>>,
    auth: auth::Session,
    /// Sends password attempts to the long-lived auth worker thread.
    /// The worker runs the blocking PAM call off the event loop; its
    /// verdict comes back over a calloop channel (registered in main()).
    auth_tx: std::sync::mpsc::Sender<AuthRequest>,
    /// True while a verdict is pending. Locks the keyboard handler
    /// (handle_key early-returns) so only one PAM attempt runs at a
    /// time and the buffer can't be mutated mid-flight.
    is_checking: bool,
    modifiers: Modifiers,
    /// Calloop handle for registering new plugin sockets on hotplug.
    /// Cloned once at startup; the original handle stays with the
    /// EventLoop in main().
    loop_handle: LoopHandle<'static, AppData>,
    /// Monotonic mint for plugin-source tenancy serials. See
    /// `register_plugin_source` and `PluginSlot::source_serial`.
    next_plugin_source_serial: u64,
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
    /// batch. Carries `(proxy, registry_id, name)`. Drained after
    /// `event_loop.dispatch()` returns, when SCTK's `OutputState`
    /// has fully processed all events from the batch (so
    /// `xdg_output.name` and friends are populated). See
    /// `process_pending_hotplug`.
    pending_outputs_arrived: Vec<(wl_output::WlOutput, u32, String)>,
    /// Last time the periodic Configure tick fired. Initialised at
    /// startup; `process_periodic_tick` re-sends Configure to every
    /// alive plugin when 30s have elapsed since this. The tick is
    /// what keeps the clock plugin's display current — every Configure
    /// carries a fresh `time_unix_seconds`.
    last_time_tick: std::time::Instant,
    /// `wp_fractional_scale_manager_v1` global, bound from the registry.
    /// `None` when the compositor does not advertise the protocol
    /// (older compositors); the core falls back to `wl_output.scale * 120`.
    fractional_scale_manager: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
}

fn main() -> ExitCode {
    println!("veiland-core");

    #[cfg(feature = "debug-unlock")]
    eprintln!("veiland-core: WARNING: debug-unlock feature enabled — Escape unlocks without auth");

    // --- 0. Harden the core process against same-UID inspection -------------
    // Plugins run as the same UID as the core, so the process boundary is not
    // by itself a wall against hostile same-user code: on a system with
    // ptrace_scope=0 a plugin could PTRACE_ATTACH the core or read
    // /proc/<pid>/mem, and mlock only prevents swapping, not reading. Marking
    // the core non-dumpable changes the ownership of its /proc/<pid>/{mem,maps,
    // environ} to root and denies same-UID ptrace, and suppresses core dumps of
    // the mlock'd password buffer. This runs before any plugin is spawned.
    //
    // It is defense-in-depth, not an absolute boundary — root, a kernel bug, or
    // a debugger started with privileges still wins. Set VEILAND_ALLOW_DUMP=1 to
    // skip it when you need to attach a debugger or collect a core dump. Only
    // the exact value 1 opts out; 0 or unset keeps the hardening, and anything
    // else warns and fails closed — this is the security escape hatch, so a
    // typo must not widen it.
    let allow_dump = match std::env::var_os("VEILAND_ALLOW_DUMP") {
        Some(v) if v == "1" => true,
        Some(v) if v == "0" => false,
        Some(v) => {
            eprintln!(
                "veiland-core: WARNING: VEILAND_ALLOW_DUMP={v:?} is not 0 or 1 — \
                ignoring it and keeping the non-dumpable hardening"
            );
            false
        }
        None => false,
    };
    if allow_dump {
        eprintln!(
            "veiland-core: WARNING: VEILAND_ALLOW_DUMP=1 — core is dumpable; the \
            password buffer is readable via ptrace/proc-mem by same-UID code"
        );
    } else if let Err(e) = nix::sys::prctl::set_dumpable(false) {
        // Best-effort: if the kernel refuses, log and continue rather than
        // refuse to lock the screen. The lock is more important than the
        // hardening, and the rest of the threat model still holds.
        eprintln!(
            "veiland-core: WARNING: prctl(PR_SET_DUMPABLE, 0) failed ({e}); core \
            remains dumpable and readable via ptrace/proc-mem by same-UID code"
        );
    }

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

    // eglGetPlatformDisplay (EGL 1.5), not the legacy eglGetDisplay. The
    // legacy call takes a bare pointer and leaves the driver to *guess* which
    // platform it belongs to. That guess holds on the common stacks (Intel,
    // NVIDIA) and fails under virgl: Mesa takes its DRI2 path, looks for a DRM
    // fd it was never given, and eglInitialize dies with EGL_BAD_ALLOC
    // ("MESA-LOADER: failed to fstat fd" on fd -1). The core then limps on a
    // display that never initialized, every eglChooseConfig fails, and the
    // lock screen is black while the plugins render happily into buffers
    // nobody can composite. Stating the platform removes the guess.
    //
    // EGL_KHR_platform_wayland's enum; khronos-egl doesn't re-export it.
    const EGL_PLATFORM_WAYLAND_KHR: egl::Enum = 0x31D8;
    // SAFETY: display_ptr came from a live wayland_client::Connection.
    let egl_display = unsafe {
        egl.get_platform_display(
            EGL_PLATFORM_WAYLAND_KHR,
            display_ptr as *mut std::ffi::c_void,
            // Must be ATTRIB_NONE-terminated; the crate rejects an unterminated list.
            &[egl::ATTRIB_NONE],
        )
        // Result -> Option, so the legacy call (which returns Option) can
        // chain as the fallback.
        .ok()
        // A stack without the EGL 1.5 entry point falls back to the legacy
        // call, i.e. to exactly the behaviour that shipped before this.
        .or_else(|| egl.get_display(display_ptr as *mut std::ffi::c_void))
    }
    .expect("get EGL display");
    egl.initialize(egl_display)
        .expect("egl failed to initialize");
    // Dev hook: VEILAND_SIMULATE_NO_FENCE=1 pretends the EGL display lacks
    // EGL_ANDROID_native_fence_sync, so a fence-capable dev box can exercise
    // the no-fence host path: capability word 0, plugins take the glFinish
    // slow path. The egress fence is unaffected — it uses the core
    // SYNC_FENCE type (see sync.rs). Pair with VEILAND_SIMULATE_NO_SYNC_FENCE
    // to also exercise the egress glFinish fallback.
    let simulate_no_fence = std::env::var_os("VEILAND_SIMULATE_NO_FENCE").is_some();
    let has_fence_fd = !simulate_no_fence
        && egl
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

    let fractional_scale_manager = globals
        .bind::<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, _, _>(&qh, 1..=1, ())
        .ok();
    if fractional_scale_manager.is_none() {
        eprintln!(
            "veiland-core: wp_fractional_scale_manager_v1 not advertised — \
            fractional scaling unavailable, falling back to wl_output.scale"
        );
    }

    let auth = match auth::Session::new() {
        Ok(s) => s,
        Err(e) => {
            eprintln!("veiland-core: failed to allocate password buffer: {}", e);
            eprintln!("veiland-core: check RLIMIT_MEMLOCK (ulimit -l)");
            return ExitCode::FAILURE;
        }
    };

    // --- Auth worker -------------------------------------------------------
    // The PAM call blocks (~2s on failure, pam_unix FAIL_DELAY). Run it on
    // a dedicated worker thread so the event loop keeps dispatching and
    // plugins keep animating. Requests go out on a plain mpsc channel;
    // verdicts come back on a calloop channel so the reply lands as an
    // event-loop wakeup on the main thread, where the lock object lives.
    // One long-lived thread: libpam FFI is not reentrant and the fail
    // delay is deliberate, so attempts are serialized, not parallelized.
    use smithay_client_toolkit::reexports::calloop::channel as calloop_channel;
    let (auth_tx, auth_rx) = std::sync::mpsc::channel::<AuthRequest>();
    let (verdict_tx, verdict_rx) = calloop_channel::channel::<AuthOutcome>();
    std::thread::Builder::new()
        .name("veiland-auth".into())
        .spawn(move || {
            // Exits when auth_tx (held by AppData) drops at shutdown.
            while let Ok(req) = auth_rx.recv() {
                let ok = auth::verify("veiland", &req.user, req.password).is_ok();
                // If the main thread is gone, the receiver is dropped and
                // send fails — nothing left to tell, so just stop.
                if verdict_tx.send(ok).is_err() {
                    break;
                }
            }
        })
        .expect("spawn auth worker thread");

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
        auth_state: AuthState::default(),
        renderer,
        plugins: Vec::new(),
        auth,
        auth_tx,
        is_checking: false,
        loop_handle: event_loop.handle(),
        next_plugin_source_serial: 0,
        qh: qh.clone(),
        config: config.clone(),
        modifiers: Modifiers::default(),
        host_capabilities,
        pending_outputs_arrived: Vec::new(),
        last_time_tick: std::time::Instant::now(),
        fractional_scale_manager,
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
    let initial_outputs: Vec<(wl_output::WlOutput, u32, String)> = state
        .output_state
        .outputs()
        .map(|o| {
            let info = state.output_state.info(&o);
            let id = info.as_ref().map(|i| i.id).unwrap_or(0);
            let name = info
                .and_then(|i| i.name)
                .unwrap_or_else(|| "<unnamed>".to_string());
            (o, id, name)
        })
        .collect();
    for (output, id, name) in &initial_outputs {
        if let Some(idx) = state.create_lock_surface_for_output(output, *id, name.clone()) {
            state.spawn_plugins_for_output(idx, name);
        }
    }
    // Discard anything `new_output` collected during the startup
    // roundtrip — we've already handled those outputs explicitly
    // here. The drain-after-dispatch path is for *real* hotplug.
    state.pending_outputs_arrived.clear();

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

    // Register the auth worker's reply channel as an event source. Each
    // verdict wakes the loop on the main thread; handle_auth_verdict acts
    // on it (unlock or mark failed) where the Wayland lock object lives.
    event_loop
        .handle()
        .insert_source(verdict_rx, |event, _, state: &mut AppData| {
            match event {
                calloop_channel::Event::Msg(ok) => state.handle_auth_verdict(ok),
                // All senders dropped mid-run: the worker thread died. It
                // only exits on its own when auth_tx drops at shutdown, so
                // this means a panic inside the PAM call. Without this arm
                // a pending attempt would leave is_checking set forever and
                // the keyboard dead on a locked screen. Treat it as a
                // failed attempt: is_checking clears, the fail indicator
                // shows, and typing works again; later attempts fail fast
                // in the send path (the worker is gone for good).
                calloop_channel::Event::Closed => {
                    eprintln!(
                        "veiland-core: auth worker thread died; treating the \
                        pending attempt as failed"
                    );
                    state.handle_auth_verdict(false);
                }
            }
        })
        .expect("register auth verdict channel with calloop");

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
wayland_client::delegate_noop!(AppData: ignore wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1);
