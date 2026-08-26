// SPDX-License-Identifier: GPL-3.0-or-later

//! Assembles the GLSL source veiland-shader actually compiles: a
//! Shadertoy-convention preamble, then the user's shader body, then a
//! `main()` wrapper that calls the user's `mainImage()`.
//!
//! The preamble is a stable contract: every uniform Shadertoy exposes for
//! a single-pass Image shader is declared here, so a shader that mentions
//! (say) `iMouse` compiles even though we always feed it zeros. Undeclared
//! uniforms are compile errors; unused declared ones cost nothing.
//!
//! Two wrapper decisions are load-bearing and locked:
//!   * fragCoord is y-flipped so Shadertoy's y-up matches how the host
//!     samples our buffer (without this every shader is upside down);
//!   * the output alpha is forced to 1.0 (Shadertoy ignores alpha and
//!     shaders write garbage there; under the host's premultiplied blend
//!     that would bleed over lower z-layers).

/// The GLSL version + precision + output declaration. `#version 300 es`
/// requires the ES3 context from `GbmEgl::new_es3`. highp everywhere:
/// procedural shaders accumulate error across many ops and mediump bands
/// badly. The output is named `veiland_fragColor` to avoid colliding with
/// any `fragColor` a user shader declares at global scope.
const HEADER: &str = "\
#version 300 es\n\
precision highp float;\n\
precision highp int;\n\
out vec4 veiland_fragColor;\n";

/// The full Shadertoy uniform set for a single-pass Image shader. Types
/// match Shadertoy exactly (iResolution is vec3, iFrame is int,
/// iChannelResolution is vec3[4], etc.) so shaders copied from the site
/// compile unchanged.
const UNIFORMS: &str = "\
uniform vec3  iResolution;\n\
uniform float iTime;\n\
uniform float iTimeDelta;\n\
uniform int   iFrame;\n\
uniform float iFrameRate;\n\
uniform vec4  iMouse;\n\
uniform vec4  iDate;\n\
uniform float iSampleRate;\n\
uniform float iChannelTime[4];\n\
uniform vec3  iChannelResolution[4];\n\
uniform sampler2D iChannel0;\n\
uniform sampler2D iChannel1;\n\
uniform sampler2D iChannel2;\n\
uniform sampler2D iChannel3;\n";

/// Compatibility shim for shaders written against WebGL1 / GLSL ES 1.00.
/// `texture2D` was renamed `texture` in GLSL ES 3.00; many older shaders
/// still call `texture2D`. Kept deliberately minimal and documented; we do
/// not paper over larger dialect gaps.
const SHIMS: &str = "\
#define texture2D texture\n";

/// Opaque wrapper `main()`: drive the user's `mainImage`, discard its
/// alpha, force 1.0. Shadertoy shaders write undefined alpha, so a
/// background ignores it. `gl_FragCoord.y` is flipped against
/// `iResolution.y` so the shader sees Shadertoy's bottom-left origin.
const WRAPPER_OPAQUE: &str = "\
void main() {\n\
    vec2 fragCoord = vec2(gl_FragCoord.x, iResolution.y - gl_FragCoord.y);\n\
    vec4 color;\n\
    mainImage(color, fragCoord);\n\
    veiland_fragColor = vec4(color.rgb, 1.0);\n\
}\n";

/// Transparent wrapper `main()`: honour the shader's alpha so the layer
/// composites over lower z-layers. Output is premultiplied (rgb * a) to
/// match the host's ONE / ONE_MINUS_SRC_ALPHA blend, and alpha is clamped
/// to [0,1] because a shader may write out-of-range values that would
/// otherwise break premultiplied blending. `u_veiland_opacity` scales the
/// whole layer (the config's `opacity`). Same y-flip as the opaque path.
const WRAPPER_ALPHA: &str = "\
uniform float u_veiland_opacity;\n\
void main() {\n\
    vec2 fragCoord = vec2(gl_FragCoord.x, iResolution.y - gl_FragCoord.y);\n\
    vec4 color;\n\
    mainImage(color, fragCoord);\n\
    float a = clamp(color.a, 0.0, 1.0) * u_veiland_opacity;\n\
    veiland_fragColor = vec4(color.rgb * a, a);\n\
}\n";

/// Build the complete fragment-shader source for `user_body`.
///
/// Layout: header, uniforms, shim, `#line 1`, the user's GLSL, then the
/// wrapper. `opaque` selects the wrapper: the opaque one forces alpha 1.0
/// (background); the transparent one premultiplies and honours the
/// shader's alpha (overlay). The result is an owned `String`; the caller
/// null-terminates it for `glShaderSource`.
///
/// The `#line 1` directive resets the compiler's line counter at the start
/// of the user body, so errors are reported relative to the user's file
/// rather than our preamble. Drivers that ignore it number across the
/// whole concatenation instead; `preamble_lines` gives the offset for
/// those.
pub fn assemble(user_body: &str, opaque: bool) -> String {
    let wrapper = if opaque {
        WRAPPER_OPAQUE
    } else {
        WRAPPER_ALPHA
    };
    let mut src = String::with_capacity(
        HEADER.len() + UNIFORMS.len() + SHIMS.len() + user_body.len() + wrapper.len() + 16,
    );
    src.push_str(HEADER);
    src.push_str(UNIFORMS);
    src.push_str(SHIMS);
    src.push_str("#line 1\n");
    src.push_str(user_body);
    // Ensure the user body ends with a newline so the wrapper starts on
    // its own line even if the file had no trailing newline.
    if !user_body.ends_with('\n') {
        src.push('\n');
    }
    src.push_str(wrapper);
    src
}

/// Number of lines the preamble adds above the user's body, for drivers
/// that ignore the `#line 1` and number across the concatenation.
pub fn preamble_lines() -> usize {
    // +1 for the "#line 1" directive itself.
    HEADER.lines().count() + UNIFORMS.lines().count() + SHIMS.lines().count() + 1
}
