// SPDX-License-Identifier: GPL-3.0-or-later

use std::{
    env, fs,
    io::{IoSliceMut, Read, Write},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd},
        unix::net::{UnixListener, UnixStream},
    },
    path::PathBuf,
    time::Duration,
};

use nix::sys::socket::{ControlMessageOwned, MsgFlags, recvmsg};

use smithay_client_toolkit::{
    compositor::{CompositorHandler, CompositorState},
    output::{OutputHandler, OutputState},
    reexports::{calloop::EventLoop, calloop_wayland_source::WaylandSource},
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
    received_dmabuf: Option<OwnedFd>,
    buffer_width: u32,
    buffer_height: u32,
    buffer_format: u32,
    buffer_stride: u32,
    buffer_modifier: u64,
    plugin_image: egl::Image,
    plugin_texture: gl::types::GLuint,
    compositor_program: gl::types::GLuint,
    compositor_vbo: gl::types::GLuint,
    compositor_sampler_loc: gl::types::GLint,
}

unsafe fn compile_shader(kind: gl::types::GLenum, src: &[u8]) -> gl::types::GLuint {
    let shader = gl::CreateShader(kind);
    let src_ptr = src.as_ptr() as *const _;
    gl::ShaderSource(shader, 1, &src_ptr, std::ptr::null());
    gl::CompileShader(shader);
    let mut ok: gl::types::GLint = 0;
    gl::GetShaderiv(shader, gl::COMPILE_STATUS, &mut ok);
    if ok == 0 {
        let mut log = [0u8; 1024];
        let mut len: gl::types::GLsizei = 0;
        gl::GetShaderInfoLog(shader, log.len() as i32, &mut len, log.as_mut_ptr() as *mut _);
        panic!(
            "shader compile failed: {}",
            std::str::from_utf8(&log[..len as usize]).unwrap_or("<invalid utf8>")
        );
    }
    shader
}

unsafe fn link_program(vs: gl::types::GLuint, fs: gl::types::GLuint) -> gl::types::GLuint {
    let program = gl::CreateProgram();
    gl::AttachShader(program, vs);
    gl::AttachShader(program, fs);
    gl::LinkProgram(program);
    let mut ok: gl::types::GLint = 0;
    gl::GetProgramiv(program, gl::LINK_STATUS, &mut ok);
    if ok == 0 {
        let mut log = [0u8; 1024];
        let mut len: gl::types::GLsizei = 0;
        gl::GetProgramInfoLog(program, log.len() as i32, &mut len, log.as_mut_ptr() as *mut _);
        panic!(
            "program link failed: {}",
            std::str::from_utf8(&log[..len as usize]).unwrap_or("<invalid utf8>")
        );
    }
    program
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

    let vs = compile_shader(gl::VERTEX_SHADER, vs_src);
    let fs = compile_shader(gl::FRAGMENT_SHADER, fs_src);
    let program = link_program(vs, fs);

    let quad: [f32; 12] = [
        -1.0, -1.0,
         1.0, -1.0,
        -1.0,  1.0,
        -1.0,  1.0,
         1.0, -1.0,
         1.0,  1.0,
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

fn main() {
    println!("veiland-core");

    let xdg_runtime_dir = env::var("XDG_RUNTIME_DIR").expect("XDG_RUNTIME_DIR is not set");
    let socket_path = PathBuf::from(&xdg_runtime_dir).join("veiland.sock");

    let _ = fs::remove_file(&socket_path);
    eprintln!("waiting for plugin on {}", socket_path.display());
    let listener = UnixListener::bind(&socket_path).expect("Could not bind to socket");

    let mut socket = match listener.accept() {
        Ok((s, addr)) => {
            eprintln!("Plugin connected: {addr:?}");
            s
        }
        Err(e) => {
            eprintln!("accept function failed: {e:?}");
            return;
        }
    };

    let mut payload = [0u8; 24];
    let mut iov = [IoSliceMut::new(&mut payload)];
    let mut cmsg_buf = nix::cmsg_space!([std::os::fd::RawFd; 1]);

    let msg = recvmsg::<()>(
        socket.as_raw_fd(),
        &mut iov,
        Some(&mut cmsg_buf),
        MsgFlags::empty(),
    )
    .expect("recvmsg");

    let mut received_fd: Option<OwnedFd> = None;
    for cmsg in msg.cmsgs().expect("cmsgs iter") {
        if let ControlMessageOwned::ScmRights(fds) = cmsg {
            for raw in fds {
                received_fd = Some(unsafe { OwnedFd::from_raw_fd(raw) });
            }
        }
    }
    let width    = u32::from_le_bytes(payload[ 0.. 4].try_into().unwrap());
    let height   = u32::from_le_bytes(payload[ 4.. 8].try_into().unwrap());
    let format   = u32::from_le_bytes(payload[ 8..12].try_into().unwrap());
    let stride   = u32::from_le_bytes(payload[12..16].try_into().unwrap());
    let modifier = u64::from_le_bytes(payload[16..24].try_into().unwrap());
    eprintln!(
        "recv header: {}x{} format=0x{:08x} stride={} modifier=0x{:016x}",
        width, height, format, stride, modifier,
    );

    let fd = received_fd.expect("plugin did not send an fd");
    eprintln!("got dmabuf fd (now fd {} in core); holding for 5g", fd.as_raw_fd());

    let conn = Connection::connect_to_env()
        .expect("failed to connect to Wayland display (is WAYLAND_DISPLAY set?)");
    let (globals, event_queue) = registry_queue_init(&conn).unwrap();
    let qh = event_queue.handle();
    let mut event_loop: EventLoop<AppData> = EventLoop::try_new().expect("calloop event loop");

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
    // Make the context current surfacelessly so we can import the dmabuf
    // into a GL texture before any lock-surface configure runs. 
    egl.make_current(
        egl_display, None, None, Some(egl_context)
    ).expect("eglMakeCurrent (surfaceless)");
    // EGL attribs — same constants as the plugin. We rebuild image_attribs
    // because we need the fd from `fd` (the OwnedFd) here.
    const EGL_LINUX_DMA_BUF_EXT: egl::Int = 0x3270;
    const EGL_LINUX_DRM_FOURCC_EXT: egl::Int = 0x3271;
    const EGL_DMA_BUF_PLANE0_FD_EXT: egl::Int = 0x3272;
    const EGL_DMA_BUF_PLANE0_OFFSET_EXT: egl::Int = 0x3273;
    const EGL_DMA_BUF_PLANE0_PITCH_EXT: egl::Int = 0x3274;
    const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: egl::Int = 0x3443;
    const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: egl::Int = 0x3444;
    let image_attribs: [egl::Attrib; 17] = [
        egl::WIDTH as egl::Attrib,                            width as egl::Attrib,
        egl::HEIGHT as egl::Attrib,                           height as egl::Attrib,
        EGL_LINUX_DRM_FOURCC_EXT as egl::Attrib,              format as egl::Attrib,
        EGL_DMA_BUF_PLANE0_FD_EXT as egl::Attrib,             fd.as_raw_fd() as egl::Attrib,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT as egl::Attrib,         0,
        EGL_DMA_BUF_PLANE0_PITCH_EXT as egl::Attrib,          stride as egl::Attrib,
        EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT as egl::Attrib,    (modifier & 0xFFFF_FFFF) as egl::Attrib,
        EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT as egl::Attrib,    (modifier >> 32) as egl::Attrib,
        egl::ATTRIB_NONE,
    ];
    let plugin_image = egl
        .create_image(
            egl_display,
            unsafe { egl::Context::from_ptr(std::ptr::null_mut()) }, // EGL_NO_CONTEXT
            EGL_LINUX_DMA_BUF_EXT as std::ffi::c_uint,
            unsafe { egl::ClientBuffer::from_ptr(std::ptr::null_mut()) }, // EGL_NO_CLIENT_BUFFER
            &image_attribs,
        )
        .expect("eglCreateImage (import dmabuf from plugin)");
    eprintln!("imported plugin dmabuf as EGLImage");

    let mut plugin_texture: gl::types::GLuint = 0;
    unsafe {
        gl::GenTextures(1, &mut plugin_texture);
        gl::BindTexture(gl::TEXTURE_2D, plugin_texture);
        let target_fn: extern "system" fn(gl::types::GLenum, *const std::ffi::c_void) =
            std::mem::transmute(
                egl.get_proc_address("glEGLImageTargetTexture2DOES")
                    .expect("glEGLImageTargetTexture2DOES not available"),
            );
        target_fn(gl::TEXTURE_2D, plugin_image.as_ptr() as *const _);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32); 
    }
    eprintln!("bound EGLImage as GL texture id={}", plugin_texture);

    let (compositor_program, compositor_vbo, compositor_sampler_loc) =
        unsafe { build_compositor_program() };
    eprintln!("built compositor program id={}", compositor_program);

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
        received_dmabuf: Some(fd),
        buffer_width: width,
        buffer_height: height,
        buffer_format: format,
        buffer_stride: stride,
        buffer_modifier: modifier,
        plugin_image,
        plugin_texture,
        compositor_program,
        compositor_vbo,
        compositor_sampler_loc,
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

    while matches!(state.run, RunState::Running) {
        event_loop
            .dispatch(Duration::from_millis(16), &mut state)
            .expect("event loop dispatch");
    }
    match state.run {
        RunState::Running => unreachable!(),
        RunState::UnlockedCleanly => println!("unlocked, exiting"),
        RunState::Refused => std::process::exit(1),
    }
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

            gl::UseProgram(self.compositor_program);

            gl::ActiveTexture(gl::TEXTURE0);
            gl::BindTexture(gl::TEXTURE_2D, self.plugin_texture);
            gl::Uniform1i(self.compositor_sampler_loc, 0);

            gl::BindBuffer(gl::ARRAY_BUFFER, self.compositor_vbo);
            let a_pos =
                gl::GetAttribLocation(self.compositor_program, b"a_pos\0".as_ptr() as *const _);
            gl::EnableVertexAttribArray(a_pos as u32);
            gl::VertexAttribPointer(a_pos as u32, 2, gl::FLOAT, gl::FALSE, 0, std::ptr::null());

            gl::DrawArrays(gl::TRIANGLES, 0, 6);
        }

        self.egl
            .swap_buffers(self.egl_display, *egl_surface)
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
        if event.keysym == Keysym::Escape {
            if let Some(lock) = self.session_lock.take() {
                lock.unlock();
                self.conn.roundtrip().expect("flush unlock");
            }
            self.run = RunState::UnlockedCleanly;
        }
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
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
        _: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
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
