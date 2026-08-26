// SPDX-License-Identifier: GPL-3.0-or-later

//! Shader compilation with the driver's *complete* info log.
//!
//! The SDK's `vgl::compile_shader` truncates the log at 1 KiB, which is
//! right for plugins that bake their GLSL in at build time. veiland-shader
//! compiles *user-supplied* GLSL, so the compile log is the product the
//! user debugs against -- in the journal when a locked session falls back,
//! and on stdout under `--check`. This module queries `GL_INFO_LOG_LENGTH`
//! and returns the whole thing, untruncated and unframed (the caller adds
//! any prefix).

/// Compile a GLSL stage, returning the driver's full info log on failure.
///
/// `src` must be null-terminated. On error the returned `String` is the
/// verbatim GL info log (no "compile failed:" prefix), trailing whitespace
/// trimmed. The failed shader object is deleted before returning.
///
/// # Safety
/// A current EGL/GL context must exist for the calling thread.
pub unsafe fn compile_shader_full_log(
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
        if ok != 0 {
            return Ok(shader);
        }

        // Failed: fetch the whole log. GL_INFO_LOG_LENGTH includes the NUL
        // terminator; allocate that, let GL write the actual count, then
        // trim to what was written and drop trailing whitespace.
        let mut cap: gl::types::GLint = 0;
        gl::GetShaderiv(shader, gl::INFO_LOG_LENGTH, &mut cap);
        let log = if cap > 0 {
            let mut buf = vec![0u8; cap as usize];
            let mut written: gl::types::GLsizei = 0;
            gl::GetShaderInfoLog(shader, cap, &mut written, buf.as_mut_ptr() as *mut _);
            buf.truncate(written.max(0) as usize);
            String::from_utf8_lossy(&buf).trim_end().to_string()
        } else {
            String::new()
        };

        gl::DeleteShader(shader);
        Err(log)
    }
}
