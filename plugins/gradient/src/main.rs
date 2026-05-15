// SPDX-License-Identifier: GPL-3.0-or-later

use gbm::AsRaw;
use khronos_egl as egl;
use nix::sys::{
    socket::{ControlMessage, MsgFlags, sendmsg},
};
use std::{
    io::{IoSlice},
    os::{fd::AsRawFd, unix::net::UnixStream},
};

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

fn main() {
    let args: Vec<String> = std::env::args().collect();
    eprintln!(
        "veiland-gradient (pid {}) args={:?}",
        std::process::id(),
        args
    );
    let socket_path = args.get(1).expect("usage: veiland-gradient <socket-path>");

    let drm_fd = nix::fcntl::open(
        "/dev/dri/renderD128",
        nix::fcntl::OFlag::O_RDWR | nix::fcntl::OFlag::O_CLOEXEC,
        nix::sys::stat::Mode::empty(),
    )
    .expect("open /dev/dri/renderD128");

    // Hand fd ownership to gbm::Device. From here on, gbm owns the close.
    let gbm = gbm::Device::new(drm_fd).expect("gbm::Device::new");

    eprintln!("opened GBM device on /dev/dri/renderD128");
    eprintln!("  backend name: {}", gbm.backend_name());

    let egl = egl::Instance::new(egl::Static);
    // EGL display from the GBM device pointer.
    // SAFETY: gbm owns a live libgbm device handle; the pointer is valid
    let egl_display =
        unsafe { egl.get_display(gbm.as_raw() as *mut std::ffi::c_void) }.expect("eglGetDisplay");

    egl.initialize(egl_display)
        .expect("egl failed to initialize");
    egl.bind_api(egl::OPENGL_ES_API)
        .expect("Failed to bind OPENGL_ES_API");

    // Pick an XRGB8888 config — no alpha needed.
    let config_attribs = [
        egl::SURFACE_TYPE,
        egl::PBUFFER_BIT,
        egl::RENDERABLE_TYPE,
        egl::OPENGL_ES2_BIT,
        egl::RED_SIZE,
        8,
        egl::GREEN_SIZE,
        8,
        egl::BLUE_SIZE,
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
    egl.make_current(egl_display, None, None, Some(egl_context))
        .expect("eglMakeCurrent");

    gl::load_with(|name| {
        egl.get_proc_address(name)
            .map(|p| p as *const _)
            .unwrap_or(std::ptr::null())
    });

    unsafe {
        let version = std::ffi::CStr::from_ptr(gl::GetString(gl::VERSION) as *const _);
        let renderer = std::ffi::CStr::from_ptr(gl::GetString(gl::RENDERER) as *const _);
        eprintln!("GL_VERSION: {}", version.to_string_lossy());
        eprintln!("GL_RENDERER: {}", renderer.to_string_lossy());
    }

    const BUFFER_WIDTH: u32 = 512;
    const BUFFER_HEIGHT: u32 = 512;
    let bo = gbm
        .create_buffer_object::<()>(
            BUFFER_WIDTH,
            BUFFER_HEIGHT,
            gbm::Format::Xrgb8888,
            gbm::BufferObjectFlags::RENDERING,
        )
        .expect("gbm create_buffer_object");
    let bo_modifier = bo.modifier();
    let bo_stride = bo.stride();
    eprintln!(
        "allocated GBM bo: {}x{} XR24, modifier=0x{:016x}, stride={}",
        bo.width(),
        bo.height(),
        u64::from(bo_modifier),
        bo_stride,
    );

    // EGL attribs for eglCreateImage with dma_buf import.
    // Constants come from EGL_EXT_image_dma_buf_import.
    const EGL_LINUX_DMA_BUF_EXT: egl::Int = 0x3270;
    const EGL_LINUX_DRM_FOURCC_EXT: egl::Int = 0x3271;
    const EGL_DMA_BUF_PLANE0_FD_EXT: egl::Int = 0x3272;
    const EGL_DMA_BUF_PLANE0_OFFSET_EXT: egl::Int = 0x3273;
    const EGL_DMA_BUF_PLANE0_PITCH_EXT: egl::Int = 0x3274;
    const EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT: egl::Int = 0x3443;
    const EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT: egl::Int = 0x3444;

    let bo_fd = bo.fd().expect("export bo dmabuf fd");
    let bo_stride = bo.stride() as i32;
    let modifier_u64 = u64::from(bo_modifier);

    let image_attribs: [egl::Attrib; 17] = [
        egl::WIDTH as egl::Attrib,
        BUFFER_WIDTH as egl::Attrib,
        egl::HEIGHT as egl::Attrib,
        BUFFER_HEIGHT as egl::Attrib,
        EGL_LINUX_DRM_FOURCC_EXT as egl::Attrib,
        gbm::Format::Xrgb8888 as u32 as egl::Attrib,
        EGL_DMA_BUF_PLANE0_FD_EXT as egl::Attrib,
        bo_fd.as_raw_fd() as egl::Attrib,
        EGL_DMA_BUF_PLANE0_OFFSET_EXT as egl::Attrib,
        0,
        EGL_DMA_BUF_PLANE0_PITCH_EXT as egl::Attrib,
        bo_stride as egl::Attrib,
        EGL_DMA_BUF_PLANE0_MODIFIER_LO_EXT as egl::Attrib,
        (modifier_u64 & 0xFFFF_FFFF) as egl::Attrib,
        EGL_DMA_BUF_PLANE0_MODIFIER_HI_EXT as egl::Attrib,
        (modifier_u64 >> 32) as egl::Attrib,
        egl::ATTRIB_NONE,
    ];

    let egl_image = egl
        .create_image(
            egl_display,
            unsafe { egl::Context::from_ptr(std::ptr::null_mut()) }, // EGL_NO_CONTEXT
            EGL_LINUX_DMA_BUF_EXT as std::ffi::c_uint,
            unsafe { egl::ClientBuffer::from_ptr(std::ptr::null_mut()) }, // EGL_NO_CLIENT_BUFFER
            &image_attribs,
        )
        .expect("eglCreateImage");
    eprintln!("created EGLImage from bo");

    let mut texture: gl::types::GLuint = 0;
    let mut framebuffer: gl::types::GLuint = 0;

    unsafe {
        gl::GenTextures(1, &mut texture);
        gl::BindTexture(gl::TEXTURE_2D, texture);
        // Resolve glEGLImageTargetTexture2DOES at runtime — it's an extension.
        let target_fn: extern "system" fn(gl::types::GLenum, *const std::ffi::c_void) =
            std::mem::transmute(
                egl.get_proc_address("glEGLImageTargetTexture2DOES")
                    .expect("glEGLImageTargetTexture2DOES not available"),
            );
        target_fn(gl::TEXTURE_2D, egl_image.as_ptr() as *const _);

        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
        gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
        gl::GenFramebuffers(1, &mut framebuffer);
        gl::BindFramebuffer(gl::FRAMEBUFFER, framebuffer);
        gl::FramebufferTexture2D(
            gl::FRAMEBUFFER,
            gl::COLOR_ATTACHMENT0,
            gl::TEXTURE_2D,
            texture,
            0,
        );
        let status = gl::CheckFramebufferStatus(gl::FRAMEBUFFER);
        assert_eq!(status, gl::FRAMEBUFFER_COMPLETE, "FBO incomplete: 0x{:x}", status);

        gl::Viewport(0, 0, BUFFER_WIDTH as i32, BUFFER_HEIGHT as i32);

        gl::ClearColor(0.0, 0.0, 0.0, 1.0);
        gl::Clear(gl::COLOR_BUFFER_BIT);

        // Vertex shader: pass each corner's normalized UV (0..1) to the fragment.
        let vs_src = b"#version 100\n\
            attribute vec2 a_pos;\n\
            varying vec2 v_uv;\n\
            void main() {\n\
                v_uv = a_pos * 0.5 + 0.5;\n\
                gl_Position = vec4(a_pos, 0.0, 1.0);\n\
            }\n\0";

        // Fragment shader: paint a diagonal RGB gradient from the UV.
        let fs_src = b"#version 100\n\
            precision mediump float;\n\
            varying vec2 v_uv;\n\
            void main() {\n\
                gl_FragColor = vec4(v_uv.x, v_uv.y, 1.0 - v_uv.x, 1.0);\n\
            }\n\0";

        let vs = compile_shader(gl::VERTEX_SHADER, vs_src);
        let fs = compile_shader(gl::FRAGMENT_SHADER, fs_src);
        let program = link_program(vs, fs);
        gl::UseProgram(program);

        // Fullscreen quad in clip space, as two triangles.
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

        let a_pos = gl::GetAttribLocation(program, b"a_pos\0".as_ptr() as *const _);
        gl::EnableVertexAttribArray(a_pos as u32);
        gl::VertexAttribPointer(a_pos as u32, 2, gl::FLOAT, gl::FALSE, 0, std::ptr::null());

        gl::DrawArrays(gl::TRIANGLES, 0, 6);
        gl::Finish();
    };
    eprintln!("rendered gradient into FBO");


    let mut stream = UnixStream::connect(&socket_path).expect("Could not connect to socket");
    eprintln!("Connected to {}", socket_path);

    let mut header = [0u8; 24];
    header[ 0.. 4].copy_from_slice(&BUFFER_WIDTH.to_le_bytes());
    header[ 4.. 8].copy_from_slice(&BUFFER_HEIGHT.to_le_bytes());
    header[ 8..12].copy_from_slice(&(gbm::Format::Xrgb8888 as u32).to_le_bytes());
    header[12..16].copy_from_slice(&(bo_stride as u32).to_le_bytes());
    header[16..24].copy_from_slice(&modifier_u64.to_le_bytes());

    let iov = [IoSlice::new(&header)];
    let fds = [bo_fd.as_raw_fd()];
    let cmsgs = [ControlMessage::ScmRights(&fds)];

    sendmsg::<()>(stream.as_raw_fd(), &iov, &cmsgs, MsgFlags::empty(), None)
        .expect("sendmsg dmabuf");

    eprintln!(
        "sent dmabuf: {}x{}, stride={}, modifier=0x{:016x}",
        BUFFER_WIDTH, BUFFER_HEIGHT, bo_stride, modifier_u64
    );

    eprintln!("Done, closing");
}
