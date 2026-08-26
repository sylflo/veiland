// SPDX-License-Identifier: GPL-3.0-or-later
// nebula - veiland-shader preset. Original work.
//
// Layered value-noise (fractal Brownian motion) drifting slowly, tinted
// through a deep-space palette. Cheap: a handful of noise octaves per
// pixel, no raymarching, no channels.

// 2D hash -> [0,1). Not the noise texture in iChannel0 - self-contained so
// the preset needs no channel bindings.
float hash(vec2 p) {
    p = fract(p * vec2(123.34, 345.45));
    p += dot(p, p + 34.345);
    return fract(p.x * p.y);
}

// Value noise: bilinear blend of hashed lattice corners.
float noise(vec2 p) {
    vec2 i = floor(p);
    vec2 f = fract(p);
    f = f * f * (3.0 - 2.0 * f);          // smoothstep weighting
    float a = hash(i);
    float b = hash(i + vec2(1.0, 0.0));
    float c = hash(i + vec2(0.0, 1.0));
    float d = hash(i + vec2(1.0, 1.0));
    return mix(mix(a, b, f.x), mix(c, d, f.x), f.y);
}

// Fractal sum: octaves of noise at doubling frequency, halving amplitude.
float fbm(vec2 p) {
    float sum = 0.0;
    float amp = 0.5;
    for (int i = 0; i < 5; i++) {
        sum += amp * noise(p);
        p *= 2.0;
        amp *= 0.5;
    }
    return sum;
}

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    // Normalise to [0,1] on the shorter axis so the look is aspect-stable.
    vec2 uv = fragCoord / iResolution.xy;
    vec2 p = (fragCoord - 0.5 * iResolution.xy) / iResolution.y;

    // Slow drift; two fbm samples at offset phases give a rolling cloud.
    float t = iTime * 0.03;
    vec2 q = vec2(fbm(p * 3.0 + t), fbm(p * 3.0 - t + 5.2));
    float f = fbm(p * 3.0 + q * 1.5);

    // Deep-space palette: near-black through indigo to a faint warm core.
    vec3 dark  = vec3(0.02, 0.02, 0.05);
    vec3 mid   = vec3(0.15, 0.10, 0.35);
    vec3 hot   = vec3(0.55, 0.35, 0.45);
    vec3 col = mix(dark, mid, smoothstep(0.2, 0.6, f));
    col = mix(col, hot, smoothstep(0.6, 0.9, f) * 0.6);

    // Gentle vignette so edges fall to black.
    float vig = smoothstep(1.2, 0.3, length(uv - 0.5));
    col *= vig;

    fragColor = vec4(col, 1.0);
}
