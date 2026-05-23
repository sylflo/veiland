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
        calloop::{EventLoop, Interest, Mode, PostAction, generic::Generic},
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
    lock_surfaces: Vec<LockSurface>,
    run: RunState,
    egl: egl::Instance<egl::Static>,
    egl_display: egl::Display,
    egl_config: egl::Config,
    egl_context: egl::Context,
    plugins: Vec<Option<PluginSlot>>,
    compositor_program: gl::types::GLuint,
    compositor_vbo: gl::types::GLuint,
    compositor_sampler_loc: gl::types::GLint,
    compositor_rect_loc: gl::types::GLint,
    auth: auth::Session,
    modifiers: Modifiers,
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
    let (globals, event_queue) = registry_queue_init(&conn).unwrap();
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

    // --- 4. Spawn plugins per config -----------------------------------------
    let mut plugins: Vec<Option<PluginSlot>> = Vec::with_capacity(config.plugins.len());
    for entry in &config.plugins {
        match try_spawn_one(entry, host_capabilities, &egl, egl_display) {
            Ok(slot) => {
                eprintln!(
                    "veiland-core: spawned plugin {:?} (binary {:?}, z_index {}) pid={}",
                    slot.name, slot.binary, slot.z_index, slot.pid
                );
                plugins.push(Some(slot));
            }
            Err(e) => {
                eprintln!(
                    "veiland-core: plugin {:?} failed to start: {} — its layer will be empty",
                    entry.name, e
                );
                plugins.push(None);
            }
        }
    }

    // Sort by z_index, stable: ties keep config-file order. Failed (None)
    // slots sort to the end via i32::MAX; they never render anything anyway.
    plugins.sort_by_key(|slot| slot.as_ref().map(|s| s.z_index).unwrap_or(i32::MAX));

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
        plugins,
        compositor_program,
        compositor_vbo,
        compositor_sampler_loc,
        compositor_rect_loc,
        auth,
        modifiers: Modifiers::default(),
    };

    let session_lock = state
        .session_lock_state
        .lock(&qh)
        .expect("ext-session-lock not supported");

    for output in state.output_state.outputs() {
        let surface = state.compositor_state.create_surface(&qh);
        let lock_surface = LockSurface {
            lock_surface: session_lock.create_lock_surface(surface, &output, &qh),
            egl_window: None,
            egl_surface: None,
        };
        state.lock_surfaces.push(lock_surface);
    }
    state.session_lock = Some(session_lock);

    WaylandSource::new(conn, event_queue)
        .insert(event_loop.handle())
        .unwrap();

    // --- 6. Register the plugin's socket as a calloop event source ----------
    // calloop owns the fd via Generic; on readability, our closure runs
    // recv_message and dispatches to PluginState. On any error we treat
    // the plugin as dead, log, and remove the source (the lock keeps
    // running with the fallback black screen).
    for i in 0..state.plugins.len() {
        let Some(slot) = state.plugins[i].as_ref() else {
            continue;
        };
        let plugin_fd = slot
            .state
            .connection
            .as_fd()
            .try_clone_to_owned()
            .expect("dup plugin socket for calloop");
        event_loop
            .handle()
            .insert_source(
                Generic::new(plugin_fd, Interest::READ, Mode::Level),
                move |_event, _meta, state: &mut AppData| state.drive_plugin(i),
            )
            .expect("register plugin fd with calloop");
    }

    // --- 7. Main loop --------------------------------------------------------
    while matches!(state.run, RunState::Running) {
        event_loop
            .dispatch(Duration::from_millis(16), &mut state)
            .expect("event loop dispatch");
    }

    // --- 8. Plugin teardown -------------------------------------------------
    // Polite shutdown sequence per plugin: ask, wait, SIGTERM, wait, SIGKILL.
    // Send Shutdown to every live plugin first, then wait per-plugin — for
    // N plugins this caps total teardown at one grace period, not N.
    for slot_opt in state.plugins.iter_mut() {
        if let Some(slot) = slot_opt {
            if let Err(e) = slot.state.connection.send_shutdown() {
                eprintln!(
                    "teardown: plugin {:?} send_shutdown failed: {} (continuing)",
                    slot.name, e
                );
            }
        }
    }

    for slot_opt in state.plugins.iter_mut() {
        if let Some(slot) = slot_opt.take() {
            teardown_one_plugin(slot);
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

fn try_spawn_one(
    entry: &config::PluginEntry,
    host_capabilities: HostCapabilities,
    egl: &egl::Instance<egl::Static>,
    display: egl::Display,
) -> Result<PluginSlot, plugin::HostError> {
    let process = spawn_plugin(&entry.binary, &entry.name)?;
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
    state.connection.send_configure(Configure {
        region_x: 0,
        region_y: 0,
        region_w: 1920,
        region_h: 1080,
        scale: 1,
        time_unix_seconds: 0,
        time_tz_offset_seconds: 0,
    })?;
    state.connection.send_frame_done()?;

    Ok(PluginSlot {
        state,
        pid: process.child_pid,
        name: entry.name.clone(),
        binary: entry.binary.clone(),
        z_index: entry.z_index,
        region: entry.region.clone(),
    })
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

        let target = session_lock_surface.wl_surface();
        let entry = self
            .lock_surfaces
            .iter_mut()
            .find(|ls| ls.lock_surface.wl_surface() == target)
            .expect("Configure for unknown lock surface");
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
            println!(" -> created EGL surface ({}x{})", width, height);
        } else {
            entry
                .egl_window
                .as_ref()
                .unwrap()
                .resize(width as i32, height as i32, 0, 0);
            println!(" -> resized EGL surface ({}x{})", width, height);
        }

        let egl_surface = entry.egl_surface.as_ref().unwrap();
        self.egl
            .make_current(
                self.egl_display,
                Some(*egl_surface),
                Some(*egl_surface),
                Some(self.egl_context),
            )
            .expect("eglMakeCurrent");

        unsafe {
            gl::Viewport(0, 0, width as i32, height as i32);
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }

        for slot_opt in &self.plugins {
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

        self.egl
            .swap_buffers(self.egl_display, *egl_surface)
            .expect("eglSwapBuffers");
    }
}

impl AppData {
    /// Repaint every lock surface that already has an EGL window.
    /// Called when a new plugin Buffer arrives — without this, the
    /// first paint (in `configure`) happens before the plugin's
    /// first Buffer and the screen stays at the clear-color.
    /// Real frame-callback wiring is M5.
    fn repaint_lock_surfaces(&mut self) {
        for entry in &self.lock_surfaces {
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
            }
            for slot_opt in &self.plugins {
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
            self.egl
                .swap_buffers(self.egl_display, *egl_surface)
                .expect("eglSwapBuffers (repaint)");
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
        _output: wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: wl_output::WlOutput,
    ) {
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
                    }
                }
            }
            Keysym::BackSpace => {
                self.auth.pop_char();
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
                }
            }
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
    fn slot_mut(&mut self, i: usize) -> Option<&mut PluginSlot> {
        self.plugins.get_mut(i).and_then(|s| s.as_mut())
    }

    fn plugin_name_for_log(&self, i: usize) -> String {
        self.plugins
            .get(i)
            .and_then(|s| s.as_ref())
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "<unknown>".to_string())
    }
}

impl AppData {
    fn drive_plugin(&mut self, i: usize) -> std::io::Result<PostAction> {
        // 1. Recv. Borrow scoped to this block.
        let recv_result = {
            let Some(slot) = self.slot_mut(i) else {
                // Slot was nulled by an earlier event on this fd
                // before calloop drained the queue. Remove the source.
                return Ok(PostAction::Remove);
            };
            slot.state.connection.recv_message()
        };

        let (msg, fds) = match recv_result {
            Ok(t) => t,
            Err(e) => {
                let name = self.plugin_name_for_log(i);
                eprintln!(
                    "veiland-core: plugin {:?} disconnected or violated protocol: {}",
                    name, e
                );
                self.plugins[i] = None;
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
            let Some(slot) = self.plugins.get_mut(i).and_then(|s| s.as_mut()) else {
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
            self.plugins[i] = None;
            return Ok(PostAction::Remove);
        }

        // 3. Buffer post-processing: composite, egress fence,
        //    BufferReleased, FrameDone. Each piece re-fetches the
        //    slot as needed
        if is_buffer {
            self.repaint_lock_surfaces();

            if !self.lock_surfaces.is_empty() {
                match plugin::create_host_fence(&self.egl, self.egl_display) {
                    Ok(fence) => {
                        let wait_result = plugin::wait_fence(&self.egl, self.egl_display, &fence);
                        plugin::release_fence(&self.egl, self.egl_display, fence);
                        if let Err(e) = wait_result {
                            eprintln!("egress fence wait failed: {}", e);
                        } else if let Some(slot) = self.slot_mut(i) {
                            if let Some(id) = slot.state.current_buffer_id {
                                if let Err(e) = slot.state.connection.send_buffer_released(id) {
                                    let name = slot.name.clone();
                                    eprintln!(
                                        "veiland-core: plugin {:?} send_buffer_released failed: {}",
                                        name, e
                                    );
                                    self.plugins[i] = None;
                                    return Ok(PostAction::Remove);
                                }
                            }
                        }
                    }
                    Err(e) => eprintln!("egress fence create failed: {}", e),
                }
            }

            if let Some(slot) = self.slot_mut(i) {
                if let Err(e) = slot.state.connection.send_frame_done() {
                    let name = slot.name.clone();
                    eprintln!(
                        "veiland-core: plugin {:?} send_frame_done failed: {}",
                        name, e
                    );
                    self.plugins[i] = None;
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
