// SPDX-License-Identifier: GPL-3.0-or-later
// plasma - veiland-shader preset. Original work.
//
// The demoscene classic: several sine waves at different orientations and
// phases summed into a scalar field, then mapped through a cosine palette.
// Cheapest of the presets - a few sines and no loops.

// Iquilez-style cosine palette: smooth, loops seamlessly. a=bias,
// b=amplitude, c=frequency, d=phase, all per-channel.
vec3 palette(float t) {
    vec3 a = vec3(0.5, 0.5, 0.5);
    vec3 b = vec3(0.5, 0.5, 0.5);
    vec3 c = vec3(1.0, 1.0, 1.0);
    vec3 d = vec3(0.00, 0.33, 0.67);
    return a + b * cos(6.28318 * (c * t + d));
}

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    // Aspect-corrected coords roughly in [-1,1] on the short axis.
    vec2 uv = (fragCoord - 0.5 * iResolution.xy) / iResolution.y;
    float t = iTime * 0.2;

    // Sum of sines at varied directions/frequencies -> interference field.
    float v = 0.0;
    v += sin(uv.x * 4.0 + t);
    v += sin((uv.y * 4.0 + t) * 1.3);
    v += sin((uv.x + uv.y) * 3.0 + t * 1.7);
    v += sin(length(uv) * 6.0 - t * 2.0);   // radial ripple from center
    v *= 0.25;                               // back to ~[-1,1]

    // Map the field through the palette; the 0.5 offset centers it.
    vec3 col = palette(v * 0.5 + 0.5 + t * 0.1);

    // Slight contrast lift so the bands read cleanly.
    col = pow(col, vec3(1.2));

    fragColor = vec4(col, 1.0);
}
