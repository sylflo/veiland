// SPDX-License-Identifier: GPL-3.0-or-later
// deepfield - veiland-shader preset. Original work.
//
// A richer deep-space nebula for the "Deep Field" astronomy scene: a
// domain-warped fractal cloud tinted through a cold-blue / magenta / gold
// palette, crossed by a dark dust lane, sitting on a vertical base gradient
// and closed off with a soft vignette. Self-contained value noise (no
// channel bindings), a handful of octaves per pixel - cheap, no raymarch.
//
// This is an original reimplementation of the domain-warp fbm technique,
// not a port of any specific Shadertoy shader.

// 2D hash -> [0,1). Self-contained so the preset needs no iChannel0.
float hash(vec2 p) {
    p = fract(p * vec2(127.1, 311.7));
    p += dot(p, p + 34.345);
    return fract(p.x * p.y);
}

// Value noise: bilinear blend of hashed lattice corners, smoothstep-weighted.
float noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);
    float a = hash(i);
    float b = hash(i + vec2(1.0, 0.0));
    float c = hash(i + vec2(0.0, 1.0));
    float d = hash(i + vec2(1.0, 1.0));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

// Fractal sum with a rotating lattice so the octaves do not line up into
// grid artifacts. Six octaves: enough structure to read as gas, still cheap.
float fbm(vec2 p) {
    float sum = 0.0;
    float amp = 0.5;
    mat2 m = mat2(1.6, 1.2, -1.2, 1.6);
    for (int i = 0; i < 6; i++) {
        sum += amp * noise(p);
        p = m * p;
        amp *= 0.5;
    }
    return sum;
}

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    // uv in [0,1]; p aspect-corrected so the cloud is not stretched wide.
    vec2 uv = fragCoord / iResolution.xy;
    vec2 p = uv;
    p.x *= iResolution.x / iResolution.y;

    // Very slow drift: the nebula should breathe, not flow.
    float t = iTime * 0.015;

    // Domain warp: sample fbm through an fbm-displaced coordinate. Two
    // offset phases give a rolling, curdled gas rather than smooth blobs.
    vec2 q = vec2(fbm(p * 2.0 + vec2(0.0, t)),
                  fbm(p * 2.0 + vec2(5.2, -t)));
    float n = fbm(p * 3.0 + q * 1.8 + t * 0.25);

    // Vertical base gradient: near-black indigo at the bottom warming a
    // touch toward the top, so the frame is never flat black.
    vec3 base = mix(vec3(0.015, 0.020, 0.060),
                    vec3(0.045, 0.030, 0.120), uv.y);

    // Palette: cold blue core -> magenta body -> gold rim, walked by the
    // cloud density n. The gold only appears in the densest wisps.
    vec3 cold = vec3(0.12, 0.28, 0.55);
    vec3 mag  = vec3(0.48, 0.13, 0.52);
    vec3 gold = vec3(0.92, 0.62, 0.34);
    vec3 neb = mix(cold, mag, smoothstep(0.20, 0.60, n));
    neb = mix(neb, gold, smoothstep(0.62, 0.95, n));

    // Density mask: gas only where the cloud is thick, so the void stays dark.
    float mask = smoothstep(0.30, 0.92, n);
    vec3 col = base + neb * mask * 0.55;

    // Dust lane: a darker warped band carved through the gas where a second
    // low-frequency fbm crosses its midline.
    float lane = smoothstep(0.35, 0.50, abs(fbm(p * 1.4 - t) - 0.5));
    col *= 0.65 + 0.35 * lane;

    // Soft radial vignette so the edges fall away into space.
    col *= 1.0 - 0.55 * length(uv - 0.5);

    // Mild gamma so the darks stay inky rather than muddy.
    col = pow(col, vec3(0.9));

    fragColor = vec4(col, 1.0);
}
