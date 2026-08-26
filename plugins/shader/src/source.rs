// SPDX-License-Identifier: GPL-3.0-or-later

//! Loading a user-supplied shader body from disk.
//!
//! The path comes from the plugin config, which is untrusted input, so
//! every failure here is a log-and-fall-back rather than an error the
//! caller has to handle.

/// Largest file we will read as a shader. Real single-pass shaders are a
/// few KiB; this is far above anything plausible and keeps a mistyped path
/// (a video, a disk image) from being pulled into memory in full.
const MAX_SHADER_BYTES: u64 = 1 << 20;

/// Read the shader body at `path`.
///
/// Returns `None` on any problem, having logged a message naming the path
/// and what was wrong -- the caller falls back to the dark fill.
pub fn load_body(plugin_name: &str, path: &str) -> Option<String> {
    if path.is_empty() {
        eprintln!("veiland-{plugin_name}: shader path is empty; using fallback.");
        return None;
    }

    // Size-check the metadata first so an oversized regular file is never
    // read at all; the post-read check below catches the cases that report
    // no size (pipes, character devices).
    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > MAX_SHADER_BYTES
    {
        eprintln!(
            "veiland-{plugin_name}: shader file {path:?} is {} bytes, over the \
             {MAX_SHADER_BYTES}-byte limit; using fallback.",
            meta.len()
        );
        return None;
    }

    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "veiland-{plugin_name}: failed to read shader file {path:?}: {e}; using fallback."
            );
            return None;
        }
    };

    if bytes.len() as u64 > MAX_SHADER_BYTES {
        eprintln!(
            "veiland-{plugin_name}: shader file {path:?} is {} bytes, over the \
             {MAX_SHADER_BYTES}-byte limit; using fallback.",
            bytes.len()
        );
        return None;
    }

    let body = match String::from_utf8(bytes) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "veiland-{plugin_name}: shader file {path:?} is not valid UTF-8 \
                 (byte {}); using fallback.",
                e.utf8_error().valid_up_to()
            );
            return None;
        }
    };

    // An empty body would compile to a preamble and wrapper with no
    // mainImage, whose link error points at our wrapper, not the real
    // problem.
    if body.trim().is_empty() {
        eprintln!("veiland-{plugin_name}: shader file {path:?} is empty; using fallback.");
        return None;
    }

    Some(body)
}
