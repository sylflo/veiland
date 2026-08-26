// SPDX-License-Identifier: GPL-3.0-or-later
// moon - veiland-shader preset. Original work. TRANSPARENT overlay: set
// opacity < 1.0 in the layer config so the disc composites over the layers
// below it. Everything outside the disc and its glow is alpha 0.
//
// A lit sphere parked in the upper-left, cropped by the left edge - a pale
// desaturated tan moon lit from the upper-left with a clear terminator (it
// reads as a 3D sphere, not a flat coin), a few small faint craters, and a
// tight subtle glow. Matches the "Deep Field" mockup's #moon.

// 2D hash / value noise for the crater placement and faint surface mottle.
float hash(vec2 p) {
    p = fract(p * vec2(127.1, 311.7));
    p += dot(p, p + 34.345);
    return fract(p.x * p.y);
}
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

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    // Work in a space normalised by height so the disc stays circular on any
    // aspect. asp lets us place the centre by the region's actual width.
    float asp = iResolution.x / iResolution.y;
    vec2 uv = fragCoord / iResolution.xy;      // [0,1], y up
    vec2 P = vec2(uv.x * asp, uv.y);           // aspect-corrected space

    // Disc placement: upper-left, cropped by the left edge. The centre sits
    // just off the left side and near the top, so only the right portion of
    // the sphere is on screen. Modest radius.
    vec2 center = vec2(0.02 * asp, 0.86);
    float R = 0.062;                           // radius in height-units

    vec2 d = P - center;
    float r = length(d) / R;                   // 0 at centre, 1 at limb

    // Antialiased disc coverage. fwidth gives a ~1px feather at the limb so
    // the edge is crisp, not blurry. fwidth is core in this ES3 context.
    float aa = fwidth(r) * 1.5 + 1e-4;
    float disc = 1.0 - smoothstep(1.0 - aa, 1.0, r);

    // Reconstruct the sphere normal from the disc coordinate: z = sqrt(1-r^2)
    // over the unit disc. This is what makes it read as a ball, not a coin.
    vec2 s = d / R;                            // [-1,1] across the disc
    float z = sqrt(max(0.0, 1.0 - dot(s, s)));
    vec3 normal = normalize(vec3(s, z));

    // Light from the upper-left, slightly toward the viewer. The Lambert term
    // gives the terminator; a small ambient floor keeps the dark side from
    // going pure black.
    vec3 L = normalize(vec3(-0.55, 0.6, 0.58));
    float ndl = dot(normal, L);
    // Concentrate the lit face toward the light so the disc is not a flat wash:
    // the bright zone is a tight cap near the light-facing point and the rest
    // (center + lower-right) falls off into shadow, the way a real side-lit
    // sphere reads. Raising the smoothstep floor toward the light and gamma-ing
    // the term pulls the highlight into a cap rather than covering half the disc.
    float lit = smoothstep(-0.05, 0.95, ndl);
    lit = pow(lit, 1.9);                        // tighter cap; center goes dimmer
    float shade = 0.05 + 0.95 * lit;           // 0.05 ambient floor

    // Pale desaturated tan surface, warm highlight to cool-brown shadow. The
    // highlight is eased down from pure-bright so the light-facing (upper-left)
    // side does not dominate; the right/face-fill glow below carries the disc.
    vec3 highlight = vec3(0.74, 0.70, 0.62);   // near the light, tempered
    vec3 shadow    = vec3(0.30, 0.26, 0.21);   // brown terminator side
    vec3 surface = mix(shadow, highlight, shade);

    // A faint warm hot spot toward the light (the CSS's radial-gradient at
    // 36%,34%). Kept subtle so it marks the light direction without making the
    // upper-left the overwhelming bright patch.
    float hs = clamp(ndl, 0.0, 1.0);
    float hotspot = pow(hs, 3.5);
    surface += vec3(0.04, 0.03, 0.02) * hotspot;

    // Faint surface mottle so the lit side is not a flat gradient.
    float mottle = noise(s * 6.0) * 0.06;
    surface *= 1.0 - mottle;

    // A few small, faint craters. Placed in disc-local coords so they sit on
    // the sphere; darkened via multiply, and faded on the dark side.
    // Each crater is (cx, cy, radius) in disc-space [-1,1].
    vec3 craters[4];
    craters[0] = vec3( 0.18,  0.12, 0.16);
    craters[1] = vec3(-0.10, -0.30, 0.22);
    craters[2] = vec3( 0.34, -0.34, 0.13);
    craters[3] = vec3( 0.05,  0.44, 0.10);
    float crater = 0.0;
    for (int i = 0; i < 4; i++) {
        float cd = length(s - craters[i].xy) / craters[i].z;
        crater += (1.0 - smoothstep(0.6, 1.0, cd)) * 0.18;
    }
    crater = clamp(crater, 0.0, 0.30);
    surface *= 1.0 - crater * (0.4 + 0.6 * lit);   // fade craters on the dark side

    // Inner glow: lift the lit face so the moon glows from within rather than
    // from a rim. Broadened to reach across the disc toward the right: a lower
    // exponent spreads the bloom past the center, and a term keyed to the
    // sphere front (z, the disc interior) carries a softer glow into the
    // right/shaded side so it does not fall bare. No halo outside the limb.
    float innerGlow = pow(hs, 1.2);            // light-facing glow, dialed way down
    float faceGlow  = smoothstep(0.0, 1.0, z); // disc-interior fill, side-independent
    surface += vec3(0.08, 0.06, 0.04) * innerGlow;   // just a whisper on the left
    surface += vec3(0.30, 0.25, 0.16) * faceGlow;    // gentle fill across the face

    // Compose. The disc alone: the lit + inner-glowing surface at full alpha.
    // Everything outside the limb is clear. The wrapper premultiplies.
    vec3 rgb = surface * disc;
    float alpha = disc;

    fragColor = vec4(rgb, alpha);
}
