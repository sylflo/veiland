// SPDX-License-Identifier: GPL-3.0-or-later

mod auth;
mod config;
mod plugin;
mod region;

use std::{process::ExitCode, time::Duration};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    output::{OutputHandler, OutputState},
    reexports::{
        calloop::{EventLoop, Interest, LoopHandle, Mode, PostAction, generic::Generic},
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

use nix::unistd::{User, getuid};

use veiland_protocol::{ClientMessage, Configure, HOST_CAP_FENCE_FD, HostCapabilities};

use plugin::{HostConnection, PluginSlot, PluginState, spawn_plugin};

#[derive(Default, PartialEq)]
enum RunState {
    #[default]
    Running,
    UnlockedCleanly,
    Refused,
}

struct LockSurface {
    name: String,
    /// The `wl_output` proxy this lock surface was created against.
    /// Kept so `update_output` can detect a rebind: if SCTK rebinds
    /// the global on a topology change, the new `WlOutput` will have
    /// a different `id()` and we know to destroy + recreate this
    /// surface against the fresh proxy. Comparing by id (not by
    /// equality of the WlOutput value) is the protocol-correct way
    /// to detect identity change.
    wl_output: wl_output::WlOutput,
    lock_surface: SessionLockSurface,
    egl_window: Option<wayland_egl::WlEglSurface>,
    egl_surface: Option<egl::Surface>,
}

struct AppData {
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
    egl: egl::Instance<egl::Static>,
    egl_display: egl::Display,
    egl_config: egl::Config,
    egl_context: egl::Context,
    plugins: Vec<Vec<Option<PluginSlot>>>,
    compositor_program: gl::types::GLuint,
    compositor_vbo: gl::types::GLuint,
    compositor_sampler_loc: gl::types::GLint,
    compositor_rect_loc: gl::types::GLint,
    indicator_program: gl::types::GLuint,
    indicator_vbo: gl::types::GLuint,
    indicator_centre_loc: gl::types::GLint,
    indicator_radius_loc: gl::types::GLint,
    indicator_color_loc: gl::types::GLint,
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

unsafe fn compile_shader(kind: gl::types::GLenum, src: &[u8]) -> gl::types::GLuint {
    unsafe {
        let shader = gl::CreateShader(kind);
        let src_ptr = src.as_ptr() as *const _;
        gl::ShaderSource(shader, 1, &src_ptr, std::ptr::null());
        gl::CompileShader(shader);
        let mut ok: gl::types::GLint = 0;
        gl::GetShaderiv(shader, gl::COMPILE_STATUS, &mut ok);
        if ok == 0 {
            let mut log = [0u8; 1024];
            let mut len: gl::types::GLsizei = 0;
            gl::GetShaderInfoLog(
                shader,
                log.len() as i32,
                &mut len,
                log.as_mut_ptr() as *mut _,
            );
            panic!(
                "shader compile failed: {}",
                std::str::from_utf8(&log[..len as usize]).unwrap_or("<invalid utf8>")
            );
        }
        shader
    }
}

unsafe fn link_program(vs: gl::types::GLuint, fs: gl::types::GLuint) -> gl::types::GLuint {
    unsafe {
        let program = gl::CreateProgram();
        gl::AttachShader(program, vs);
        gl::AttachShader(program, fs);
        gl::LinkProgram(program);
        let mut ok: gl::types::GLint = 0;
        gl::GetProgramiv(program, gl::LINK_STATUS, &mut ok);
        if ok == 0 {
            let mut log = [0u8; 1024];
            let mut len: gl::types::GLsizei = 0;
            gl::GetProgramInfoLog(
                program,
                log.len() as i32,
                &mut len,
                log.as_mut_ptr() as *mut _,
            );
            panic!(
                "program link failed: {}",
                std::str::from_utf8(&log[..len as usize]).unwrap_or("<invalid utf8>")
            );
        }
        program
    }
}

unsafe fn build_compositor_program() -> (
    gl::types::GLuint,
    gl::types::GLuint,
    gl::types::GLint,
    gl::types::GLint,
) {
    let vs_src = b"#version 100\n\
        attribute vec2 a_pos;\n\
        uniform vec4 u_rect;\n\
        varying vec2 v_uv;\n\
        void main() {\n\
            // a_pos is the unit quad in [-1, 1]\xB2. Remap to [0, 1]\n\
            // (= 'normalised quad'), then place inside the target\n\
            // clip-space rect u_rect = (x, y, w, h).\n\
            vec2 unit01 = a_pos * 0.5 + 0.5;\n\
            vec2 clip = u_rect.xy + unit01 * u_rect.zw;\n\
            gl_Position = vec4(clip.x, clip.y, 0.0, 1.0);\n\
    \n\
            // UV samples the plugin's dmabuf edge-to-edge regardless\n\
            // of where the quad lands on screen. Y is flipped because\n\
            // the dmabuf is top-down but GL samples bottom-up.\n\
            v_uv = vec2(unit01.x, 1.0 - unit01.y);\n\
        }\n\0";

    let fs_src = b"#version 100\n\
        precision mediump float;\n\
        varying vec2 v_uv;\n\
        uniform sampler2D u_tex;\n\
        void main() {\n\
            gl_FragColor = texture2D(u_tex, v_uv);\n\
        }\n\0";

    unsafe {
        let vs = compile_shader(gl::VERTEX_SHADER, vs_src);
        let fs = compile_shader(gl::FRAGMENT_SHADER, fs_src);
        let program = link_program(vs, fs);

        let quad: [f32; 12] = [
            -1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0,
        ];

        let mut vbo: gl::types::GLuint = 0;
        gl::GenBuffers(1, &mut vbo);
        gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
        gl::BufferData(
            gl::ARRAY_BUFFER,
            std::mem::size_of_val(&quad) as isize,
            quad.as_ptr() as *const _,
            gl::STATIC_DRAW,
        );

        let sampler_loc = gl::GetUniformLocation(program, b"u_tex\0".as_ptr() as *const _);
        let rect_loc = gl::GetUniformLocation(program, b"u_rect\0".as_ptr() as *const _);

        (program, vbo, sampler_loc, rect_loc)
    }
}

/// Build the password-indicator GL program.
///
/// One filled circle per draw call. The "circle" is a unit quad whose
/// fragment shader discards anything outside radius 1 from the quad
/// centre — standard procedural-shape trick, no geometry library
/// needed. The caller issues N draws (N = dot count) with `u_centre`
/// updated between each; `u_radius` and `u_color` stay constant
/// across the row.
///
/// `u_centre` and `u_radius` are in clip space (so per-frame the
/// caller converts surface-px → clip-space). Y is flipped at
/// conversion time, not in the shader, because there's no UV here.
unsafe fn build_indicator_program() -> (
    gl::types::GLuint,
    gl::types::GLuint,
    gl::types::GLint,
    gl::types::GLint,
    gl::types::GLint,
) {
    let vs_src = b"#version 100\n\
        attribute vec2 a_pos;\n\
        uniform vec2 u_centre;\n\
        uniform vec2 u_radius;\n\
        varying vec2 v_local;\n\
        void main() {\n\
            v_local = a_pos;\n\
            vec2 clip = u_centre + a_pos * u_radius;\n\
            gl_Position = vec4(clip, 0.0, 1.0);\n\
        }\n\0";

    // highp on the fragment shader: GLES 2 defaults to mediump,
    // which some Mesa drivers honour as fp16 and bands the circle
    // edge visibly at 12-px diameter. NVIDIA defaults to fp32
    // either way. highp is portable and cheap at this scale.
    //
    // smoothstep gives a one-fragment-wide antialias ramp on the
    // edge instead of a hard discard. Without it the dot looks
    // pixelated on both vendors at small sizes.
    let fs_src = b"#version 100\n\
        precision highp float;\n\
        varying vec2 v_local;\n\
        uniform vec4 u_color;\n\
        void main() {\n\
            float d = length(v_local);\n\
            // 1.0 inside, 0.0 outside, smooth across the last\n\
            // ~1.5/radius_px fraction of the radius. fwidth would\n\
            // be more correct but isn't in GLES 2 core.\n\
            float a = 1.0 - smoothstep(0.92, 1.0, d);\n\
            if (a <= 0.0) discard;\n\
            gl_FragColor = vec4(u_color.rgb, u_color.a * a);\n\
        }\n\0";

    unsafe {
        let vs = compile_shader(gl::VERTEX_SHADER, vs_src);
        let fs = compile_shader(gl::FRAGMENT_SHADER, fs_src);
        let program = link_program(vs, fs);

        // Same unit quad as the compositor. Allocated separately so
        // the two programs stay independent — no shared-VBO coupling
        // to worry about. 48 bytes is free.
        let quad: [f32; 12] = [
            -1.0, -1.0, 1.0, -1.0, -1.0, 1.0, -1.0, 1.0, 1.0, -1.0, 1.0, 1.0,
        ];

        let mut vbo: gl::types::GLuint = 0;
        gl::GenBuffers(1, &mut vbo);
        gl::BindBuffer(gl::ARRAY_BUFFER, vbo);
        gl::BufferData(
            gl::ARRAY_BUFFER,
            std::mem::size_of_val(&quad) as isize,
            quad.as_ptr() as *const _,
            gl::STATIC_DRAW,
        );

        let centre_loc = gl::GetUniformLocation(program, b"u_centre\0".as_ptr() as *const _);
        let radius_loc = gl::GetUniformLocation(program, b"u_radius\0".as_ptr() as *const _);
        let color_loc = gl::GetUniformLocation(program, b"u_color\0".as_ptr() as *const _);

        (program, vbo, centre_loc, radius_loc, color_loc)
    }
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

    let (compositor_program, compositor_vbo, compositor_sampler_loc, compositor_rect_loc) =
        unsafe { build_compositor_program() };
    eprintln!("built compositor program id={}", compositor_program);

    let (
        indicator_program,
        indicator_vbo,
        indicator_centre_loc,
        indicator_radius_loc,
        indicator_color_loc,
    ) = unsafe { build_indicator_program() };
    eprintln!("built indicator program id={}", indicator_program);

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
        egl,
        egl_display,
        egl_config,
        egl_context,
        plugins: Vec::new(),
        compositor_program,
        compositor_vbo,
        compositor_sampler_loc,
        compositor_rect_loc,
        indicator_program,
        indicator_vbo,
        indicator_centre_loc,
        indicator_radius_loc,
        indicator_color_loc,
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
fn entry_matches_output(entry: &config::PluginEntry, output_name: &str) -> bool {
    match &entry.monitors {
        None => true,
        Some(list) => list.iter().any(|name| name == output_name),
    }
}

fn try_spawn_one(
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
    let config_json = entry.config.as_ref().and_then(|v| {
        match serde_json::to_string(v) {
            Ok(s) => Some(s),
            Err(e) => {
                eprintln!(
                    "veiland-core: plugin {:?}: failed to serialise [plugin.config] to JSON: {} \
                     — spawning without VEILAND_PLUGIN_CONFIG",
                    entry.name, e
                );
                None
            }
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
fn current_time_for_configure() -> (i64, i32) {
    let now = chrono::Local::now();
    (now.timestamp(), now.offset().local_minus_utc())
}

/// Wind down the plugin: send Shutdown, give it ~250ms to exit on its own,
/// then SIGTERM, then SIGKILL. Reaps the zombie. Best-effort — if any step
/// fails we log and continue, because at this point the host is exiting
/// anyway and refusing to exit would be worse than a leaked plugin.
fn teardown_one_plugin(slot: PluginSlot) {
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
                self.egl.create_window_surface(
                    self.egl_display,
                    self.egl_config,
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
        self.egl
            .make_current(
                self.egl_display,
                Some(egl_surface),
                Some(egl_surface),
                Some(self.egl_context),
            )
            .expect("eglMakeCurrent");

        unsafe {
            gl::Viewport(0, 0, width as i32, height as i32);
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
            // Straight-alpha blending: plugins emit non-pre-multiplied
            // pixels (gl_FragColor = vec4(rgb, a) with rgb not scaled
            // by a). State stays enabled across the per-plugin loop —
            // every draw in this codebase wants blending on. If a
            // future non-plugin draw needs it off, that site sets state
            // explicitly. See docs/protocol.md §12.
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
        }

        for slot_opt in &self.plugins[output_idx] {
            if let Some(slot) = slot_opt {
                let rect =
                    region::region_to_clip_rect(slot.region.as_ref(), width as i32, height as i32);
                slot.state.composite(
                    self.compositor_program,
                    self.compositor_vbo,
                    self.compositor_sampler_loc,
                    self.compositor_rect_loc,
                    rect,
                );
            }
        }

        // Indicator paints on top of any plugins — soft trust-region
        // enforcement. Plugins can declare any region; the indicator
        // is always painted last so it can't be obscured. See M9
        // step 4 / docs/m9-plan.md Q1.
        self.draw_password_indicator(width as i32, height as i32);

        self.egl
            .swap_buffers(self.egl_display, egl_surface)
            .expect("eglSwapBuffers");
    }
}

impl AppData {
    /// Create a `LockSurface` for one output and place it in
    /// `self.lock_surfaces`. Prefers reusing the first `None` slot
    /// (left behind by an earlier `output_destroyed`) so the
    /// `lock_surfaces` ↔ `plugins` index correspondence stays
    /// compact across hotplug cycles. If no `None` slot exists,
    /// pushes a fresh one. Returns the index the surface was
    /// placed at, or `None` if the session_lock isn't held
    /// (defensive check, log + skip).
    fn create_lock_surface_for_output(
        &mut self,
        output: &wl_output::WlOutput,
        name: String,
    ) -> Option<usize> {
        let session_lock = match self.session_lock.as_ref() {
            Some(l) => l,
            None => {
                eprintln!(
                    "veiland-core: refusing to create lock surface for {:?}: \
                    no session lock held",
                    name
                );
                return None;
            }
        };
        let surface = self.compositor_state.create_surface(&self.qh);
        eprintln!(
            "veiland-core: output {} connected, creating lock surface",
            name
        );
        let lock_surface = LockSurface {
            name,
            wl_output: output.clone(),
            lock_surface: session_lock.create_lock_surface(surface, output, &self.qh),
            egl_window: None,
            egl_surface: None,
        };
        // Reuse the first None slot if one exists (hotplug-out leaves
        // sentinels; reusing keeps lock_surfaces.len() bounded under
        // long sessions with frequent topology changes).
        if let Some(idx) = self.lock_surfaces.iter().position(|s| s.is_none()) {
            self.lock_surfaces[idx] = Some(lock_surface);
            Some(idx)
        } else {
            self.lock_surfaces.push(Some(lock_surface));
            Some(self.lock_surfaces.len() - 1)
        }
    }

    /// Spawn the per-output slice of plugins for `output_name` and
    /// place it at `output_idx` in `self.plugins`, growing the outer
    /// vec with empty slices as needed so the index matches the
    /// LockSurface index returned by `create_lock_surface_for_output`.
    /// Filters each entry by its `monitors` selector. Reusing slot
    /// `output_idx` (overwriting whatever was there) handles the
    /// hotplug-in case where `create_lock_surface_for_output`
    /// returned a recycled `None` slot.
    fn spawn_plugins_for_output(&mut self, output_idx: usize, output_name: &str) {
        // Look up the output's scale factor via SCTK's OutputInfo and clamp to
        // the protocol's 1..=3 range. Hardware reporting an out-of-range value
        // (e.g. 4 on 8K-at-200%) is rare but real; clamping to 3 keeps text
        // *almost* the right size on unfamiliar hardware, which is the least
        // surprising failure mode. Raising the cap is a separate decision.
        let raw_scale = self.lock_surfaces[output_idx]
            .as_ref()
            .and_then(|s| self.output_state.info(&s.wl_output))
            .map(|info| info.scale_factor)
            .unwrap_or(1);
        let scale: u32 = match u32::try_from(raw_scale) {
            Ok(s) if (1..=3).contains(&s) => s,
            Ok(s) => {
                eprintln!(
                    "veiland-core: output {} reports scale {}, outside 1..=3; clamping to 3",
                    output_name, s
                );
                3
            }
            Err(_) => {
                eprintln!(
                    "veiland-core: output {} reports negative scale {}; clamping to 1",
                    output_name, raw_scale
                );
                1
            }
        };

        let mut per_output: Vec<Option<PluginSlot>> = Vec::with_capacity(self.config.plugins.len());
        for entry in &self.config.plugins {
            if !entry_matches_output(entry, output_name) {
                per_output.push(None);
                continue;
            }
            match try_spawn_one(
                entry,
                output_name,
                scale,
                self.host_capabilities,
                &self.egl,
                self.egl_display,
            ) {
                Ok(slot) => {
                    eprintln!(
                        "veiland-core: spawned plugin {:?} on output {} (binary {:?}, z_index {}) pid={}",
                        slot.name, slot.output_name, slot.binary, slot.z_index, slot.pid
                    );
                    per_output.push(Some(slot));
                }
                Err(e) => {
                    eprintln!(
                        "veiland-core: plugin {:?} failed to start on output {}: {} — its layer will be empty",
                        entry.name, output_name, e
                    );
                    per_output.push(None);
                }
            }
        }
        // Sort by z_index, stable: ties keep config-file order. Failed (None)
        // slots sort to the end via i32::MAX; they never render anything.
        per_output.sort_by_key(|slot| slot.as_ref().map(|s| s.z_index).unwrap_or(i32::MAX));
        // Place at output_idx, growing with empty slices if needed.
        while self.plugins.len() <= output_idx {
            self.plugins.push(Vec::new());
        }
        self.plugins[output_idx] = per_output;
    }

    /// Register a plugin's socket as a calloop event source. Captures
    /// `(o, p)` by value; the closure stays valid as long as
    /// `self.plugins[o][p]` remains in place (slot may be `None` later;
    /// `drive_plugin` handles that by returning `PostAction::Remove`).
    fn register_plugin_source(&self, o: usize, p: usize) {
        let Some(slot) = self
            .plugins
            .get(o)
            .and_then(|po| po.get(p))
            .and_then(|s| s.as_ref())
        else {
            return;
        };
        let plugin_fd = slot
            .state
            .connection
            .as_fd()
            .try_clone_to_owned()
            .expect("dup plugin socket for calloop");
        self.loop_handle
            .insert_source(
                Generic::new(plugin_fd, Interest::READ, Mode::Level),
                move |_event, _meta, state: &mut AppData| state.drive_plugin(o, p),
            )
            .expect("register plugin fd with calloop");
    }

    /// Drain the pending-hotplug queues. Called after every
    /// `event_loop.dispatch()` returns — by that point SCTK has fully
    /// processed all events from the batch (registry bind/unbind,
    /// xdg_output.name, geometry, etc.) and our internal state is
    /// safe to mutate.
    ///
    /// Two queues:
    ///
    /// - `pending_outputs_arrived`: outputs whose `wl_output` global
    ///   newly appeared. Create a lock surface (the compositor will
    ///   send us a Configure for it on a later dispatch), then spawn
    ///   the matching plugin instances and register their sockets.
    ///
    /// - `pending_outputs_rebound`: outputs whose `wl_output` proxy
    ///   was rebound (Hyprland's fast-replug pattern). The plugins
    ///   are still alive and connected; only the lock surface needs
    ///   replacing. Destroy the old surface (dropping it lets SCTK's
    ///   Drop send `ext_session_lock_surface_v1.destroy()`), then
    ///   create a fresh one against the new proxy.
    ///
    /// Both paths are idempotent against running again with the same
    /// queues — we filter "already have a lock surface for this
    /// name" out of the arrived queue defensively in case SCTK fires
    /// `new_output` for an output we already created at startup.
    fn process_pending_hotplug(&mut self) {
        let arrived = std::mem::take(&mut self.pending_outputs_arrived);
        let rebound = std::mem::take(&mut self.pending_outputs_rebound);

        // --- Arrivals ---
        // For each newly-arrived output, create a lock surface and
        // spawn plugins. The compositor will send a Configure for
        // the new surface on a subsequent dispatch.
        for (output, name) in arrived {
            // Defensive: if we already have a lock surface for this
            // name, this is a spurious notification. Skip rather
            // than double-create.
            let already_have = self
                .lock_surfaces
                .iter()
                .any(|s| s.as_ref().map(|ls| ls.name == name).unwrap_or(false));
            if already_have {
                eprintln!(
                    "[M8-TRACE] arrival skipped: {:?} already has a lock surface",
                    name
                );
                continue;
            }
            eprintln!("[M8-TRACE] processing arrival: {:?}", name);
            let Some(idx) = self.create_lock_surface_for_output(&output, name.clone()) else {
                continue;
            };
            self.spawn_plugins_for_output(idx, &name);
            // Register calloop sources for every plugin slot at this
            // index. Even if the slot was previously occupied (slot
            // recycled from a torn-down output), the prior calloop
            // sources self-removed when their plugin sockets hit EOF;
            // the new processes need fresh sources.
            for p in 0..self.plugins[idx].len() {
                self.register_plugin_source(idx, p);
            }
        }

        // --- Rebinds ---
        // For each rebound output, replace the lock surface with a
        // fresh one against the new wl_output proxy. The old surface
        // is dropped (sending destroy) before the new one is created.
        for (output, name) in rebound {
            eprintln!("[M8-TRACE] processing rebind: {:?}", name);
            // Find the matching slot. If it's gone (e.g. our previous
            // drain step already replaced it), nothing to do.
            let Some(idx) = self
                .lock_surfaces
                .iter()
                .position(|s| s.as_ref().map(|ls| ls.name == name).unwrap_or(false))
            else {
                eprintln!(
                    "[M8-TRACE] rebind skipped: {:?} has no current lock surface",
                    name
                );
                continue;
            };
            // Tear down EGL bits first (same order as output_destroyed
            // Phase 3 — keep EGL from sending commits to a dying surface).
            if let Some(surface_ref) = self.lock_surfaces[idx].as_mut() {
                if let Some(egl_surface) = surface_ref.egl_surface.take() {
                    if let Err(e) = self.egl.destroy_surface(self.egl_display, egl_surface) {
                        eprintln!(
                            "veiland-core: eglDestroySurface for {:?} (rebind) failed: \
                            {:?} (continuing)",
                            surface_ref.name, e
                        );
                    }
                }
                surface_ref.egl_window = None;
            }
            // Drop the old LockSurface so SCTK's Drop sends destroy.
            self.lock_surfaces[idx] = None;
            // Create the fresh one against the new wl_output proxy.
            // It will land in the just-emptied slot (`create_lock_
            // surface_for_output` prefers None slots).
            self.create_lock_surface_for_output(&output, name);
        }
    }

    /// Fire the 30 s periodic Configure tick. Called from the main
    /// loop after every `event_loop.dispatch()` returns; bails out
    /// early if less than 30 s have elapsed since the last tick.
    ///
    /// Why this exists: the host's only mandatory Configure is at
    /// spawn. A clock plugin (`veiland-clock`, M11 step 2) needs the
    /// time field to advance for its display to track the wall clock.
    /// Re-sending Configure with refreshed `time_unix_seconds` keeps
    /// plugins pure functions of host events instead of each one
    /// reaching for `clock_gettime` independently.
    ///
    /// 30 s is the cadence picked in `docs/m11-plan.md` Q1: within
    /// half a minute of true wall-clock time, cheap enough to ignore.
    /// Other Configure fields (region, scale, output_name) are
    /// re-sent unchanged from `slot.last_configure`.
    ///
    /// If a plugin's socket has died, `send_configure` will return
    /// an error; we log and continue. The next inbound-event read on
    /// that plugin's calloop source will surface the broken-pipe
    /// error through the existing drive_plugin error path, which
    /// removes the slot.
    fn process_periodic_tick(&mut self) {
        const TICK_INTERVAL: std::time::Duration = std::time::Duration::from_secs(30);
        if self.last_time_tick.elapsed() < TICK_INTERVAL {
            return;
        }
        self.last_time_tick = std::time::Instant::now();

        let (time_unix_seconds, time_tz_offset_seconds) = current_time_for_configure();
        for per_output in self.plugins.iter_mut() {
            for slot_opt in per_output.iter_mut() {
                let Some(slot) = slot_opt.as_mut() else {
                    continue;
                };
                let Some(prev) = slot.last_configure.clone() else {
                    continue;
                };
                let next = Configure {
                    time_unix_seconds,
                    time_tz_offset_seconds,
                    ..prev
                };
                if let Err(e) = slot.state.connection.send_configure(next.clone()) {
                    eprintln!(
                        "veiland-core: periodic tick: send_configure to plugin {:?} failed: {} \
                         (will be cleaned up on next read)",
                        slot.name, e
                    );
                    continue;
                }
                slot.last_configure = Some(next);
            }
        }
    }
}

impl AppData {
    /// Repaint every lock surface that already has an EGL window.
    /// Called when a new plugin Buffer arrives — without this, the
    /// first paint (in `configure`) happens before the plugin's
    /// first Buffer and the screen stays at the clear-color.
    /// Real frame-callback wiring is M5.
    fn repaint_lock_surfaces(&mut self) {
        // Bail if we're past the Running state. After lock.unlock()
        // the compositor destroys our lock surface server-side; a
        // swap_buffers() into it then blocks forever waiting for
        // the compositor to release the previous frame. That kept
        // the main loop stuck and teardown never ran (M6 step 6
        // bug; surfaced with three plugins, where there's always a
        // Buffer event queued behind unlock — two plugins worked by
        // timing luck).
        //
        // TODO: this is a defensive bail-out at the GPU layer. The
        // structural fix is to drop the plugin calloop sources at
        // the start of teardown so Buffer events stop arriving at
        // all. Leave both in place once that lands — the
        // invariant "no compositing post-unlock" is worth enforcing
        // at the GPU site regardless of who upstream might violate
        // it.
        if !matches!(self.run, RunState::Running) {
            return;
        }
        for (output_idx, entry) in self.lock_surfaces.iter().enumerate() {
            let Some(entry) = entry else {
                continue;
            };
            let Some(egl_surface) = entry.egl_surface.as_ref() else {
                continue;
            };
            let Some(egl_window) = entry.egl_window.as_ref() else {
                continue;
            };
            let (w, h) = egl_window.get_size();

            self.egl
                .make_current(
                    self.egl_display,
                    Some(*egl_surface),
                    Some(*egl_surface),
                    Some(self.egl_context),
                )
                .expect("eglMakeCurrent (repaint)");

            unsafe {
                gl::Viewport(0, 0, w, h);
                gl::ClearColor(0.0, 0.0, 0.0, 1.0);
                gl::Clear(gl::COLOR_BUFFER_BIT);
                // Straight-alpha blending. See the matching note in
                // SessionLockHandler::configure for the why; the
                // short version is "plugins emit non-pre-multiplied
                // pixels and we don't toggle blend off after the
                // loop because nothing else needs it off."
                gl::Enable(gl::BLEND);
                gl::BlendFunc(gl::SRC_ALPHA, gl::ONE_MINUS_SRC_ALPHA);
            }
            for slot_opt in &self.plugins[output_idx] {
                if let Some(slot) = slot_opt {
                    let rect = region::region_to_clip_rect(slot.region.as_ref(), w, h);
                    slot.state.composite(
                        self.compositor_program,
                        self.compositor_vbo,
                        self.compositor_sampler_loc,
                        self.compositor_rect_loc,
                        rect,
                    );
                }
            }

            // Indicator paints on top of any plugins — see the
            // matching note in SessionLockHandler::configure.
            self.draw_password_indicator(w, h);

            self.egl
                .swap_buffers(self.egl_display, *egl_surface)
                .expect("eglSwapBuffers (repaint)");
        }
    }

    /// Draw the password indicator on the currently-bound EGL surface.
    ///
    /// `width` and `height` are the surface's pixel dimensions. The
    /// caller is responsible for making the right EGL context current
    /// and clearing the framebuffer; this method only issues the
    /// indicator draws. Designed to be called *last* in the per-
    /// surface paint sequence so the indicator appears on top of any
    /// plugins (the soft trust-region — plugins can declare any
    /// region, the indicator always wins on paint order).
    ///
    /// One draw call per dot (N ≤ 32). Cheap enough that loop-vs-
    /// instancing doesn't matter at this scale.
    ///
    /// Sizing / position / colour are hardcoded in M9 step 3; step 5
    /// replaces them with the `[password]` config table.
    fn draw_password_indicator(&self, width: i32, height: i32) {
        let pw = &self.config.password;

        // Cap at max_dots (config-driven; clamped at load to [1, 256]).
        // The row freezes at this value — the user keeps typing but
        // the dot count stops growing.
        let n = self.auth.char_count().min(pw.max_dots as usize);
        if n == 0 || width <= 0 || height <= 0 {
            return;
        }

        // Colours are not configurable in v1; see docs/m9-plan.md
        // "Deferred to post-M9".
        let color: [f32; 4] = [220.0 / 255.0, 220.0 / 255.0, 220.0 / 255.0, 1.0];

        let diameter = pw.dot_diameter as f32;
        let spacing = pw.dot_spacing as f32;

        // `x` default is surface-relative (centred), so it can't be
        // baked into the config struct — resolved per-surface here.
        // Same for `y_percent` default of 75. Both pass through their
        // clamp ranges at load if explicitly set.
        let centre_x_px = pw.x.map(|v| v as f32).unwrap_or(width as f32 / 2.0);
        let y_percent = pw.y_percent.unwrap_or(75) as f32;

        let w = width as f32;
        let h = height as f32;

        // Leftmost dot centre in surface pixels. total_width is the
        // row's extent edge-to-edge; centring it on centre_x_px puts
        // the leftmost *edge* at centre_x_px - total/2, so the
        // leftmost *centre* is half a diameter further right.
        let total_width = (n as f32 - 1.0) * spacing + diameter;
        let start_x = centre_x_px - total_width / 2.0 + diameter / 2.0;
        let centre_y_px = h * y_percent / 100.0;

        // Clip-space radius: surface-px / (surface-px / 2) = 2 * px /
        // surface, per axis. Width and height differ for non-square
        // surfaces, so the dot stays circular on screen.
        let rx = diameter / w;
        let ry = diameter / h;

        unsafe {
            gl::UseProgram(self.indicator_program);

            // Vertex attribute setup — same shape as plugin/state.rs's
            // composite(). Re-binding per call is cheap and keeps this
            // method self-contained (no assumed GL state from the
            // previous program).
            gl::BindBuffer(gl::ARRAY_BUFFER, self.indicator_vbo);
            let a_pos =
                gl::GetAttribLocation(self.indicator_program, b"a_pos\0".as_ptr() as *const _);
            gl::EnableVertexAttribArray(a_pos as u32);
            gl::VertexAttribPointer(a_pos as u32, 2, gl::FLOAT, gl::FALSE, 0, std::ptr::null());

            // Uniforms that don't change between dots.
            gl::Uniform2f(self.indicator_radius_loc, rx, ry);
            gl::Uniform4f(
                self.indicator_color_loc,
                color[0],
                color[1],
                color[2],
                color[3],
            );

            for i in 0..n {
                let centre_x = start_x + i as f32 * spacing;
                // Surface-px → clip space. Y is flipped: surface y=0
                // is top, clip y=+1 is top. (The compositor shader
                // flips at the UV instead; the indicator has no UV,
                // so we flip here.)
                let cx = (centre_x / w) * 2.0 - 1.0;
                let cy = -((centre_y_px / h) * 2.0 - 1.0);
                gl::Uniform2f(self.indicator_centre_loc, cx, cy);
                gl::DrawArrays(gl::TRIANGLES, 0, 6);
            }
        }
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
        _surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
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
                if let Err(e) = self.egl.destroy_surface(self.egl_display, egl_surface) {
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

impl AppData {
    fn handle_key(&mut self, event: &KeyEvent) {
        // Reject modified keys. Plain Shift is fine (for capitals); Ctrl/Alt/Super are not.
        if self.modifiers.ctrl || self.modifiers.alt || self.modifiers.logo {
            return;
        }

        // Tracks whether this key changed the indicator-visible state
        // (buffer length). Set on push/pop and on PAM-fail (the
        // buffer is cleared inside authenticate()). On unlock we
        // don't bother — the lock surface is about to go away and
        // repaint_lock_surfaces will bail on RunState::UnlockedCleanly
        // anyway.
        let mut buffer_changed = false;

        match event.keysym {
            Keysym::Return | Keysym::KP_Enter => {
                if self.auth.is_empty() {
                    return;
                }
                let user = match User::from_uid(getuid()) {
                    Ok(Some(u)) => u.name,
                    _ => {
                        eprintln!("auth: cannot resolve current user, refusing auth");
                        return;
                    }
                };
                match self.auth.authenticate("veiland", &user) {
                    Ok(()) => {
                        if let Some(lock) = self.session_lock.take() {
                            lock.unlock();
                            self.conn.roundtrip().expect("flush unlock");
                        }
                        self.run = RunState::UnlockedCleanly;
                    }
                    Err(_) => {
                        // Buffer already cleared by authenticate(). User retypes.
                        // Repaint so the dots vanish — that's the
                        // (silent) failure feedback for M9.
                        buffer_changed = true;
                    }
                }
            }
            Keysym::BackSpace => {
                self.auth.pop_char();
                buffer_changed = true;
            }
            #[cfg(feature = "debug-unlock")]
            Keysym::Escape => {
                self.auth.clear();
                if let Some(lock) = self.session_lock.take() {
                    lock.unlock();
                    self.conn.roundtrip().expect("flush unlock");
                }
                self.run = RunState::UnlockedCleanly;
            }
            _ => {
                if let Some(s) = event.utf8.as_deref()
                    && !s.chars().any(|c| c.is_control())
                {
                    self.auth.push_utf8(s);
                    buffer_changed = true;
                }
            }
        }

        if buffer_changed {
            // Synchronous repaint from inside the keyboard handler.
            // Single-threaded calloop: this can't race with a Buffer-
            // driven repaint because both run on the same loop. The
            // RunState guard inside repaint_lock_surfaces handles the
            // post-unlock case defensively.
            self.repaint_lock_surfaces();
        }
    }
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

impl AppData {
    fn slot_mut(&mut self, o: usize, p: usize) -> Option<&mut PluginSlot> {
        self.plugins.get_mut(o)?.get_mut(p)?.as_mut()
    }

    fn plugin_name_for_log(&self, o: usize, p: usize) -> String {
        self.plugins
            .get(o)
            .and_then(|per_output| per_output.get(p))
            .and_then(|s| s.as_ref())
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "<unknown>".to_string())
    }
}

impl AppData {
    fn drive_plugin(&mut self, o: usize, p: usize) -> std::io::Result<PostAction> {
        // 1. Recv. Borrow scoped to this block.
        let recv_result = {
            let Some(slot) = self.slot_mut(o, p) else {
                // Slot was nulled by an earlier event on this fd
                // before calloop drained the queue. Remove the source.
                return Ok(PostAction::Remove);
            };
            slot.state.connection.recv_message()
        };

        let (msg, fds) = match recv_result {
            Ok(t) => t,
            Err(e) => {
                let name = self.plugin_name_for_log(o, p);
                eprintln!(
                    "veiland-core: plugin {:?} disconnected or violated protocol: {}",
                    name, e
                );
                self.plugins[o][p] = None;
                return Ok(PostAction::Remove);
            }
        };

        let is_buffer = matches!(msg, ClientMessage::Buffer(_));

        // 2. Dispatch. The block produces an owned outcome
        //    (Result<(), (name, err)>) so the slot borrow ends before
        //    we touch `self.plugins[i] = None` on the failure path.
        //    `&self.egl` and `self.egl_display` are captured *before*
        //    the slot borrow because handle_message needs them while
        //    we hold the slot; the borrow checker won't let us reach
        //    into self after slot_mut has taken &mut self.
        let dispatch_result: Result<(), (String, plugin::HostError)> = {
            let egl = &self.egl;
            let display = self.egl_display;
            let Some(slot) = self
                .plugins
                .get_mut(o)
                .and_then(|per_output| per_output.get_mut(p))
                .and_then(|s| s.as_mut())
            else {
                return Ok(PostAction::Remove);
            };
            let name = slot.name.clone();
            slot.state
                .handle_message(msg, fds, egl, display)
                .map_err(|e| (name, e))
        };

        if let Err((name, e)) = dispatch_result {
            eprintln!(
                "veiland-core: plugin {:?} protocol error: {} — treating as dead",
                name, e
            );
            self.plugins[o][p] = None;
            return Ok(PostAction::Remove);
        }

        // 3. Buffer post-processing: composite, egress fence,
        //    BufferReleased, FrameDone. Each piece re-fetches the
        //    slot as needed
        if is_buffer {
            self.repaint_lock_surfaces();

            // After 5a, lock_surfaces can be non-empty while every slot is
            // None (hotplug-departed outputs leave sentinels behind). The
            // egress fence has nothing to do unless at least one live
            // surface was just composited into.
            if self.lock_surfaces.iter().any(|s| s.is_some()) {
                match plugin::create_host_fence(&self.egl, self.egl_display) {
                    Ok(fence) => {
                        let wait_result = plugin::wait_fence(&self.egl, self.egl_display, &fence);
                        plugin::release_fence(&self.egl, self.egl_display, fence);
                        if let Err(e) = wait_result {
                            eprintln!("egress fence wait failed: {}", e);
                        } else if let Some(slot) = self.slot_mut(o, p) {
                            if let Some(id) = slot.state.current_buffer_id {
                                if let Err(e) = slot.state.connection.send_buffer_released(id) {
                                    let name = slot.name.clone();
                                    eprintln!(
                                        "veiland-core: plugin {:?} send_buffer_released failed: {}",
                                        name, e
                                    );
                                    self.plugins[o][p] = None;
                                    return Ok(PostAction::Remove);
                                }
                            }
                        }
                    }
                    Err(e) => eprintln!("egress fence create failed: {}", e),
                }
            }

            if let Some(slot) = self.slot_mut(o, p) {
                if let Err(e) = slot.state.connection.send_frame_done() {
                    let name = slot.name.clone();
                    eprintln!(
                        "veiland-core: plugin {:?} send_frame_done failed: {}",
                        name, e
                    );
                    self.plugins[o][p] = None;
                    return Ok(PostAction::Remove);
                }
            }
        }
        Ok(PostAction::Continue)
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
