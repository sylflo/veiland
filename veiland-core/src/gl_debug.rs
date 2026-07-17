// SPDX-License-Identifier: GPL-3.0-or-later

//! GL error observability, gated behind `VEILAND_GL_DEBUG=1`.
//!
//! Two mechanisms, both no-ops unless the env var is set (so production
//! pays nothing):
//! - [`install_debug_callback`] registers the driver's `glDebugMessageCallback`
//!   so the driver reports every GL error with source/type/severity for free.
//! - [`check_gl`] drains `glGetError` at fragile GL boundaries as a portable
//!   fallback for drivers that lack `GL_KHR_debug` (likely on our GLES2 context).
//!
//! Diagnostics only: nothing here changes control flow. The env var gates
//! whether we *observe* errors; it never gates how the locker *reacts* to
//! them.

use std::sync::atomic::{AtomicBool, Ordering};

/// Process-global toggle. Set once at startup from `main`, then only read.
/// A static beats threading a `bool` through `import_dmabuf` / `composite` /
/// the repaint loop, since `check_gl` is called from several modules whose
/// signatures we don't want to grow. `AtomicBool` (not `static mut bool`) so
/// reads stay in safe code and are well-defined even if the driver debug
/// callback fires on its own thread. Safe to make global because this is a
/// pure diagnostics switch, not a security/auth boundary.
static GL_DEBUG_ENABLED: AtomicBool = AtomicBool::new(false);

/// Enable or disable GL diagnostics. Call once at startup after reading
/// `VEILAND_GL_DEBUG`.
pub fn set_enabled(on: bool) {
    GL_DEBUG_ENABLED.store(on, Ordering::Relaxed);
}

fn enabled() -> bool {
    GL_DEBUG_ENABLED.load(Ordering::Relaxed)
}

/// Drain the GL error queue and log anything set, tagged with `label`.
///
/// No-op unless `VEILAND_GL_DEBUG` was enabled — the `enabled()` check comes
/// first, so with diagnostics off we never even call `glGetError` (no
/// per-frame driver round-trip). When on, we loop until `GL_NO_ERROR`: GL can
/// queue several errors, and a single read would leave stale ones to be
/// misattributed to the next `check_gl` label.
///
/// Requires a current EGL context on the calling thread — every call site has
/// one (they sit right after GL draw/import work).
pub fn check_gl(label: &str) {
    if !enabled() {
        return;
    }
    loop {
        // SAFETY: caller holds a current context; glGetError has no other
        // precondition and is always loaded (gl::load_with ran in main).
        let err = unsafe { gl::GetError() };
        if err == gl::NO_ERROR {
            break;
        }
        eprintln!(
            "veiland-core: [gl-debug] {label}: {} (0x{err:04X})",
            err_name(err)
        );
    }
}

/// Human-readable name for a `glGetError` value (the fallback path).
fn err_name(e: gl::types::GLenum) -> &'static str {
    match e {
        gl::INVALID_ENUM => "GL_INVALID_ENUM",
        gl::INVALID_VALUE => "GL_INVALID_VALUE",
        gl::INVALID_OPERATION => "GL_INVALID_OPERATION",
        gl::OUT_OF_MEMORY => "GL_OUT_OF_MEMORY",
        gl::INVALID_FRAMEBUFFER_OPERATION => "GL_INVALID_FRAMEBUFFER_OPERATION",
        _ => "GL_UNKNOWN_ERROR",
    }
}

/// Register the driver debug callback, if diagnostics are on *and* the entry
/// point resolved. Safe to call unconditionally — it self-gates.
///
/// Must run with a current EGL context (main calls it right after the
/// surfaceless `make_current`). The runtime context is GLES2, which exposes
/// this only via `GL_KHR_debug`, so on many drivers the fn won't resolve; we
/// skip gracefully and lean on `check_gl` instead.
pub fn install_debug_callback() {
    if !enabled() {
        return;
    }
    // The gl crate points *unresolved* functions at `missing_fn_panic`, so
    // calling glDebugMessageCallback without this guard would panic on a
    // driver that lacks GL_KHR_debug. is_loaded() reflects whether load_with
    // found the entry point (core, ARB, or KHR variant).
    if !gl::DebugMessageCallback::is_loaded() {
        eprintln!(
            "veiland-core: [gl-debug] glDebugMessageCallback not available \
             (no GL_KHR_debug on this GLES2 driver); using check_gl fallback only"
        );
        return;
    }
    // SAFETY: context is current and the fn is loaded (checked above). The
    // callback is a 'static fn item that outlives the process; we pass a null
    // userParam because we carry no per-call state.
    unsafe {
        gl::Enable(gl::DEBUG_OUTPUT);
        // Synchronous: the callback fires on the thread that made the
        // offending call, so its log line lands next to the code that caused
        // it. Async mode may report from a driver thread, out of order.
        gl::Enable(gl::DEBUG_OUTPUT_SYNCHRONOUS);
        gl::DebugMessageCallback(Some(debug_callback), std::ptr::null());
    }
    eprintln!("veiland-core: [gl-debug] driver debug callback installed");
}

/// The driver calls this for every GL debug message. Signature must match
/// `gl::types::GLDEBUGPROC` exactly, including the `extern "system"` ABI (the
/// driver, not us, does the calling).
extern "system" fn debug_callback(
    source: gl::types::GLenum,
    gltype: gl::types::GLenum,
    id: gl::types::GLuint,
    severity: gl::types::GLenum,
    length: gl::types::GLsizei,
    message: *const gl::types::GLchar,
    _user: *mut std::ffi::c_void,
) {
    // The driver hands us `message` valid only for this call, as `length`
    // bytes. We copy into an owned String before returning so nothing
    // dangles. Guard null/negative length; don't blindly CStr::from_ptr
    // (drivers disagree on whether `length` counts the trailing NUL).
    let text = if message.is_null() || length < 0 {
        String::from("<no message>")
    } else {
        // SAFETY: driver guarantees `message` points to `length` valid bytes
        // for the duration of this call; we read exactly that many and copy.
        let bytes = unsafe { std::slice::from_raw_parts(message as *const u8, length as usize) };
        String::from_utf8_lossy(bytes)
            .trim_end_matches('\0')
            .to_string()
    };
    eprintln!(
        "veiland-core: [gl-debug] source={} type={} severity={} id={} {}",
        source_name(source),
        type_name(gltype),
        severity_name(severity),
        id,
        text,
    );
}

fn source_name(s: gl::types::GLenum) -> &'static str {
    match s {
        gl::DEBUG_SOURCE_API => "API",
        gl::DEBUG_SOURCE_WINDOW_SYSTEM => "WINDOW_SYSTEM",
        gl::DEBUG_SOURCE_SHADER_COMPILER => "SHADER_COMPILER",
        gl::DEBUG_SOURCE_THIRD_PARTY => "THIRD_PARTY",
        gl::DEBUG_SOURCE_APPLICATION => "APPLICATION",
        gl::DEBUG_SOURCE_OTHER => "OTHER",
        _ => "?",
    }
}

fn type_name(t: gl::types::GLenum) -> &'static str {
    match t {
        gl::DEBUG_TYPE_ERROR => "ERROR",
        gl::DEBUG_TYPE_DEPRECATED_BEHAVIOR => "DEPRECATED",
        gl::DEBUG_TYPE_UNDEFINED_BEHAVIOR => "UNDEFINED",
        gl::DEBUG_TYPE_PORTABILITY => "PORTABILITY",
        gl::DEBUG_TYPE_PERFORMANCE => "PERFORMANCE",
        gl::DEBUG_TYPE_MARKER => "MARKER",
        gl::DEBUG_TYPE_OTHER => "OTHER",
        _ => "?",
    }
}

fn severity_name(s: gl::types::GLenum) -> &'static str {
    match s {
        gl::DEBUG_SEVERITY_HIGH => "HIGH",
        gl::DEBUG_SEVERITY_MEDIUM => "MEDIUM",
        gl::DEBUG_SEVERITY_LOW => "LOW",
        gl::DEBUG_SEVERITY_NOTIFICATION => "NOTIFICATION",
        _ => "?",
    }
}
