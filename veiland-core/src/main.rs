// SPDX-License-Identifier: GPL-3.0-or-later

mod auth;
mod plugin;

use std::{path::PathBuf, process::ExitCode, time::Duration};

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

use veiland_protocol::{ClientMessage, Configure};

use plugin::{HostConnection, PluginState, spawn_plugin};

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
    plugin: PluginState,
    plugin_pid: nix::unistd::Pid,
    compositor_program: gl::types::GLuint,
    compositor_vbo: gl::types::GLuint,
    compositor_sampler_loc: gl::types::GLint,
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

unsafe fn build_compositor_program() -> (gl::types::GLuint, gl::types::GLuint, gl::types::GLint) {
    let vs_src = b"#version 100\n\
        attribute vec2 a_pos;\n\
        varying vec2 v_uv;\n\
        void main() {\n\
            // a_pos is in clip space [-1, 1]. UV is [0, 1] with V flipped\n\
            // because the dmabuf is top-down but GL samples bottom-up.\n\
            v_uv = vec2(a_pos.x * 0.5 + 0.5, 1.0 - (a_pos.y * 0.5 + 0.5));\n\
            gl_Position = vec4(a_pos, 0.0, 1.0);\n\
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

        (program, vbo, sampler_loc)
    }
}

fn main() -> ExitCode {
    println!("veiland-core");

    #[cfg(feature = "debug-unlock")]
    eprintln!("veiland-core: WARNING: debug-unlock feature enabled — Escape unlocks without auth");

    // --- 1. Spawn the plugin -------------------------------------------------
    // Hardcoded path; plugin discovery is M6. The plugin inherits its
    // socket end as fd 3 (see plugin/spawn.rs); the host keeps the other end.
    let plugin_binary = PathBuf::from("./target/debug/veiland-gradient");
    let process =
        spawn_plugin(&plugin_binary, "gradient").expect("failed to spawn gradient plugin");
    let plugin_pid = process.child_pid;
    eprintln!("spawned gradient plugin pid={}", plugin_pid);

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

    let (compositor_program, compositor_vbo, compositor_sampler_loc) =
        unsafe { build_compositor_program() };
    eprintln!("built compositor program id={}", compositor_program);

    // --- 4. Protocol bootstrap: handshake + Hello + Configure + FrameDone ---
    let mut connection = HostConnection::from_fd(process.socket);
    connection.handshake().expect("plugin handshake");
    eprintln!("handshake ok");

    // recv_message has already enforced "Hello carries no fd" at the wire
    // layer (any fd on a non-Buffer message is ProtocolViolation there).
    // We just need to reject a misbehaving plugin that sent the wrong
    // variant as its first message — and even then, exit cleanly rather
    // than panic, because plugin input must never crash the locker.
    let (plugin_name, plugin_version) = match connection.recv_message() {
        Ok((ClientMessage::Hello(h), _)) => (h.plugin_name, h.plugin_version),
        Ok((other, _)) => {
            eprintln!(
                "plugin sent {:?} before Hello; refusing to start lock",
                other
            );
            return ExitCode::FAILURE;
        }
        Err(e) => {
            eprintln!("plugin handshake failed: {}; refusing to start lock", e);
            return ExitCode::FAILURE;
        }
    };
    eprintln!("plugin says hello: {} v{}", plugin_name, plugin_version);

    // Build PluginState and feed the Hello through handle_message so the
    // state machine records name/version through the canonical path.
    let mut plugin = PluginState::new(connection);
    plugin
        .handle_message(
            ClientMessage::Hello(veiland_protocol::Hello {
                plugin_name: plugin_name.clone(),
                plugin_version: plugin_version.clone(),
            }),
            None,
            &egl,
            egl_display,
        )
        .expect("record Hello in PluginState");

    // Send the initial Configure. Region = full screen at 1920x1080
    // (placeholder until lock-surface configure tells us the real size;
    // we'll re-send when that arrives). Time fields are zeroed for M3.
    plugin
        .connection
        .send_configure(Configure {
            region_x: 0,
            region_y: 0,
            region_w: 1920,
            region_h: 1080,
            scale: 1,
            time_unix_seconds: 0,
            time_tz_offset_seconds: 0,
        })
        .expect("send initial Configure");
    plugin
        .connection
        .send_frame_done()
        .expect("send initial FrameDone");

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
        plugin,
        plugin_pid,
        compositor_program,
        compositor_vbo,
        compositor_sampler_loc,
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
    let plugin_fd = state
        .plugin
        .connection
        .as_fd()
        .try_clone_to_owned()
        .expect("dup plugin socket for calloop");
    event_loop
        .handle()
        .insert_source(
            Generic::new(plugin_fd, Interest::READ, Mode::Level),
            |_event, _meta, state: &mut AppData| {
                match state.plugin.connection.recv_message() {
                    Ok((msg, fd)) => {
                        let is_buffer = matches!(msg, ClientMessage::Buffer(_));
                        if let Err(e) =
                            state
                                .plugin
                                .handle_message(msg, fd, &state.egl, state.egl_display)
                        {
                            eprintln!("plugin protocol error: {} — treating as dead", e);
                            // Drop the texture so subsequent frames go fallback.
                            state.plugin.texture = None;
                            return Ok(PostAction::Remove);
                        }
                        // After we accept a Buffer, cue the next frame.
                        // We do *not* commit the lock surfaces here — they
                        // attach buffers via swap_buffers in the configure
                        // handler. Committing a buffer-less surface here
                        // would trigger "Null buffer attached" from the
                        // compositor and kill the session. Real frame-loop
                        // wiring (wl_surface::frame callbacks) is M5.
                        if is_buffer {
                            state.repaint_lock_surfaces();
                            if let Err(e) = state.plugin.connection.send_frame_done() {
                                eprintln!("send_frame_done failed: {}", e);
                                return Ok(PostAction::Remove);
                            }
                        }
                        Ok(PostAction::Continue)
                    }
                    Err(e) => {
                        eprintln!("plugin disconnected or violated protocol: {}", e);
                        state.plugin.texture = None;
                        Ok(PostAction::Remove)
                    }
                }
            },
        )
        .expect("register plugin fd with calloop");

    // --- 7. Main loop --------------------------------------------------------
    while matches!(state.run, RunState::Running) {
        event_loop
            .dispatch(Duration::from_millis(16), &mut state)
            .expect("event loop dispatch");
    }

    // --- 8. Plugin teardown -------------------------------------------------
    // Polite shutdown sequence: ask, wait, SIGTERM, wait, SIGKILL.
    // Don't let the plugin outlive us; don't leave a zombie either.
    teardown_plugin(&mut state.plugin.connection, state.plugin_pid);

    match state.run {
        RunState::Running => unreachable!(),
        RunState::UnlockedCleanly => {
            println!("unlocked, exiting");
            ExitCode::SUCCESS
        }
        RunState::Refused => ExitCode::FAILURE,
    }
}

/// Wind down the plugin: send Shutdown, give it ~250ms to exit on its own,
/// then SIGTERM, then SIGKILL. Reaps the zombie. Best-effort — if any step
/// fails we log and continue, because at this point the host is exiting
/// anyway and refusing to exit would be worse than a leaked plugin.
fn teardown_plugin(connection: &mut HostConnection, pid: nix::unistd::Pid) {
    use nix::sys::signal::{Signal, kill};
    use nix::sys::wait::{WaitPidFlag, WaitStatus, waitpid};

    // 1. Polite ask. Plugin's recv_event sees Shutdown, returns Ok(()).
    if let Err(e) = connection.send_shutdown() {
        eprintln!("teardown: send_shutdown failed: {} (continuing)", e);
    }

    // 2. Grace period. The spec says "implementation-defined"; 250ms is
    //    enough for a well-behaved plugin to exit and short enough that
    //    a session-unlock doesn't feel laggy.
    let grace = Duration::from_millis(250);
    let deadline = std::time::Instant::now() + grace;
    loop {
        match waitpid(pid, Some(WaitPidFlag::WNOHANG)) {
            Ok(WaitStatus::StillAlive) => {
                if std::time::Instant::now() >= deadline {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(_) => {
                eprintln!("teardown: plugin exited cleanly");
                return;
            }
            Err(e) => {
                eprintln!("teardown: waitpid failed: {} (continuing)", e);
                return;
            }
        }
    }

    // 3. SIGTERM, brief wait.
    eprintln!(
        "teardown: plugin did not exit in {}ms, sending SIGTERM",
        grace.as_millis()
    );
    let _ = kill(pid, Signal::SIGTERM);
    std::thread::sleep(Duration::from_millis(100));
    if let Ok(status) = waitpid(pid, Some(WaitPidFlag::WNOHANG))
        && !matches!(status, WaitStatus::StillAlive)
    {
        eprintln!("teardown: plugin reaped after SIGTERM");
        return;
    }

    // 4. SIGKILL, reap, done.
    eprintln!("teardown: plugin still alive, sending SIGKILL");
    let _ = kill(pid, Signal::SIGKILL);
    let _ = waitpid(pid, None);
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

        self.plugin.composite(
            self.compositor_program,
            self.compositor_vbo,
            self.compositor_sampler_loc,
        );

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
            self.plugin.composite(
                self.compositor_program,
                self.compositor_vbo,
                self.compositor_sampler_loc,
            );
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

// SCTK delegate macros — must come after the *Handler impls they delegate to.
smithay_client_toolkit::delegate_compositor!(AppData);
smithay_client_toolkit::delegate_output!(AppData);
smithay_client_toolkit::delegate_seat!(AppData);
smithay_client_toolkit::delegate_keyboard!(AppData);
smithay_client_toolkit::delegate_registry!(AppData);
smithay_client_toolkit::delegate_session_lock!(AppData);
wayland_client::delegate_noop!(AppData: ignore wl_buffer::WlBuffer);
