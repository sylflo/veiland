// SPDX-License-Identifier: GPL-3.0-or-later

//! The fallback shader: a solid very-dark fill shown when the selected
//! shader can't be used (unknown preset, missing file, compile error).
//!
//! Deliberately plain - a near-black fill, not a pretty preset - so a
//! failure reads as "something is wrong" rather than passing for intended
//! content. It is compiled once at startup, before any user shader, so it
//! is always available to switch to without exiting the plugin.
//!
//! Written as `#version 100` (GLSL ES 1.00), which compiles under both the
//! ES2 and ES3 contexts, so the fallback survives even if ES3 setup is the
//! thing that failed.

/// Vertex shader: pass a fullscreen clip-space quad straight through.
pub const VERTEX: &[u8] = b"#version 100\n\
    precision highp float;\n\
    attribute vec2 a_pos;\n\
    void main() {\n\
        gl_Position = vec4(a_pos, 0.0, 1.0);\n\
    }\n\0";

/// Fragment shader: emit a constant near-black, fully opaque. RGB is
/// premultiplied trivially (alpha 1.0), matching the host's blend.
pub const FRAGMENT: &[u8] = b"#version 100\n\
    precision highp float;\n\
    void main() {\n\
        gl_FragColor = vec4(0.02, 0.02, 0.03, 1.0);\n\
    }\n\0";
