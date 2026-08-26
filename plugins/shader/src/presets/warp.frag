// SPDX-License-Identifier: GPL-3.0-or-later
// warp - veiland-shader preset. Original work.
//
// A star tunnel: screen-space is remapped to polar coordinates and the
// radius is driven by iTime so points stream past the viewer. Three depth
// layers at different speeds give parallax. Self-contained; no channels.

// 2D hash -> [0,1).
float hash(vec2 p) {
    p = fract(p * vec2(123.34, 345.45));
    p += dot(p, p + 34.345);
    return fract(p.x * p.y);
}

// One layer of streaming stars. `speed` moves them toward the viewer;
// `density` sets how many cells across the angular axis.
vec3 layer(vec2 uv, float t, float speed, float density, vec3 tint) {
    // Polar: angle around center, and a depth that scrolls with time.
    float a = atan(uv.y, uv.x);
    float r = length(uv);
    // 1/r puts the vanishing point at center; add time to fly inward.
    float depth = 0.3 / (r + 0.02) + t * speed;

    // Cell grid in (angle, depth). Each cell holds at most one star.
    vec2 cell = vec2(a * density, depth);
    vec2 id = floor(cell);
    vec2 f = fract(cell) - 0.5;

    float h = hash(id);
    // Star only in cells whose hash clears a threshold -> sparse field.
    float star = smoothstep(0.5, 0.0, length(f)) * step(0.6, h);
    // Twinkle: fade each star in and out on its own phase.
    float tw = 0.5 + 0.5 * sin(t * 3.0 + h * 6.2831);
    // Fade with radius so stars are born faint at the center.
    return tint * star * tw * smoothstep(0.0, 0.4, r);
}

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = (fragCoord - 0.5 * iResolution.xy) / iResolution.y;

    float t = iTime;
    vec3 col = vec3(0.0);
    col += layer(uv, t, 0.25, 12.0, vec3(0.6, 0.7, 1.0));  // far, blue
    col += layer(uv, t, 0.45, 8.0,  vec3(0.9, 0.9, 1.0));  // mid, white
    col += layer(uv, t, 0.75, 5.0,  vec3(1.0, 0.85, 0.7)); // near, warm

    // Faint radial glow toward the vanishing point.
    float r = length(uv);
    col += vec3(0.05, 0.06, 0.12) * smoothstep(0.6, 0.0, r);

    fragColor = vec4(col, 1.0);
}
