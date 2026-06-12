// SPDX-License-Identifier: GPL-3.0-or-later

//! Thin GL helpers shared by all plugins. Compile a shader or link a
//! program and get back a `Result` — no panics.

/// Compile a single GLSL shader stage.
///
/// `src` must be a null-terminated byte string. Returns the GL shader
/// name on success. On failure logs the GL info log and returns an
/// `Err` — the caller should propagate this up and let the plugin exit
/// cleanly rather than panicking.
///
/// # Safety
/// A current EGL/GL context must exist for the calling thread.
pub unsafe fn compile_shader(
    kind: gl::types::GLenum,
    src: &[u8],
) -> Result<gl::types::GLuint, String> {
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
            let msg = std::str::from_utf8(&log[..len as usize]).unwrap_or("<invalid utf8>");
            return Err(format!("shader compile failed: {msg}"));
        }
        Ok(shader)
    }
}

/// Link a vertex and fragment shader into a GL program.
///
/// Returns the GL program name on success. On failure logs the GL info
/// log and returns an `Err`.
///
/// # Safety
/// A current EGL/GL context must exist for the calling thread.
pub unsafe fn link_program(
    vs: gl::types::GLuint,
    fs: gl::types::GLuint,
) -> Result<gl::types::GLuint, String> {
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
            let msg = std::str::from_utf8(&log[..len as usize]).unwrap_or("<invalid utf8>");
            return Err(format!("program link failed: {msg}"));
        }
        Ok(program)
    }
}
