// SPDX-License-Identifier: GPL-3.0-or-later
// starfield - veiland-shader preset. Original work. TRANSPARENT overlay: set
// opacity < 1.0 in the layer config so the stars composite over the layers
// below. Everything between the stars is alpha 0.
//
// A mostly-still deep-field starfield for the "Deep Field" astronomy scene:
// three depth tiers of crisp point-stars (faint many / mid / bright few), a
// minority of the bright ones warm gold and the rest cool blue-white, each
// twinkling out of sync (dimming toward a floor, never fully off), a tight
// soft glow per star, and a barely-perceptible horizontal parallax drift. This
// reproduces the mockup's parallax-stars look, which the drifting-particle
// plugin cannot: those rise-and-fall like snow; a sky should sit still and
// blink.

// 2D -> 2D hash: independent pseudo-random pair per cell. Used for the star's
// sub-cell position, and (offset) for its brightness/size/color/phase.
vec2 hash22(vec2 p) {
    p = vec2(dot(p, vec2(127.1, 311.7)),
             dot(p, vec2(269.5, 183.3)));
    return fract(sin(p) * 43758.5453);
}
float hash21(vec2 p) {
    return fract(sin(dot(p, vec2(41.7, 289.1))) * 24634.633);
}

// One tier of stars: a cell grid at `density` cells across the height. `thresh`
// is the fraction of cells that actually hold a star (fewer -> sparser). `amp`
// scales the tier's overall brightness (faint tiers dimmer). `sizePx` is the
// core point radius in this tier. `goldChance` is the odds a star is gold.
// Returns (rgb, a) accumulation for this tier; the wrapper premultiplies.
vec4 tier(vec2 uv, float asp, float density, float thresh,
          float amp, float sizePx, float goldChance, float t, float drift) {
    // Grid in a square-ish metric: scale x by aspect so cells are not stretched.
    vec2 g = vec2(uv.x * asp, uv.y) * density;
    // Parallax: shift the whole tier a touch horizontally. Nearly static; the
    // brighter tiers get slightly more, giving faint depth.
    g.x += drift;

    vec2 cell = floor(g);
    vec2 f = fract(g);

    vec4 acc = vec4(0.0);
    // Look at the 3x3 neighbourhood so a star's glow can spill across cell
    // borders without popping.
    for (int j = -1; j <= 1; j++) {
        for (int i = -1; i <= 1; i++) {
            vec2 off = vec2(float(i), float(j));
            vec2 id = cell + off;

            // Does this cell hold a star at all?
            float present = hash21(id);
            if (present > thresh) {
                continue;
            }

            // Star attributes from a second hash stream.
            vec2 rnd = hash22(id + 7.0);
            vec2 pos = off + rnd;             // sub-cell position
            float d = length(f - pos);        // distance in cell units

            // Per-star brightness base and twinkle. Dim toward a floor (0.55),
            // never fully off -- matches the mockup's 0.55 + 0.45*sin.
            float ph = rnd.x * 6.2831 + hash21(id + 3.0) * 6.2831;
            float tws = 0.6 + hash21(id + 5.0) * 2.2;   // per-star pulse rate
            float base = 0.5 + rnd.y * 0.5;
            float tw = 0.55 + 0.45 * sin(t * tws + ph);
            float bright = amp * base * tw;

            // Crisp point with a tight soft glow: a narrow core plus an
            // exponential halo, both scaled by the tier's point size. The core
            // is small (star-like), the halo tight (a rim, not a bloom).
            float core = smoothstep(sizePx * 0.012, 0.0, d);
            float halo = exp(-d * (52.0 / sizePx)) * 0.5;
            float lum = (core + halo) * bright;

            // Colour: mostly cool blue-white; a minority of the bright tier
            // gold. hash on a fresh offset so gold selection is independent.
            float goldPick = hash21(id + 11.0);
            vec3 cool = vec3(0.88, 0.92, 1.0);
            vec3 gold = vec3(0.96, 0.84, 0.62);
            vec3 col = (goldPick < goldChance) ? gold : cool;

            acc.rgb += col * lum;
            acc.a += lum;
        }
    }
    return acc;
}

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = fragCoord / iResolution.xy;      // [0,1], y up
    float asp = iResolution.x / iResolution.y;

    // Barely-perceptible parallax: a very slow horizontal sway. This is the
    // whole "living but still" trick -- the field breathes, it does not stream.
    float drift = sin(iTime * 0.03) * 0.15;

    vec4 acc = vec4(0.0);
    // Three depth tiers, back (faint/dense/small) to front (bright/sparse/big).
    // Densities and thresholds echo the mockup's 140 / 80 / 36 counts and the
    // gold-only-in-the-bright-tier rule.
    acc += tier(uv, asp, 26.0, 0.55, 0.55, 1.0, 0.0,  iTime, drift * 0.4);
    acc += tier(uv, asp, 16.0, 0.42, 0.85, 1.5, 0.0,  iTime, drift * 0.7);
    acc += tier(uv, asp,  9.0, 0.30, 1.15, 2.2, 0.30, iTime, drift);

    // Clamp so overlapping glows do not blow past white; the wrapper
    // premultiplies. Alpha is the accumulated luminance (clamped) so the stars
    // composite over the nebula and everything else stays clear.
    vec3 rgb = min(acc.rgb, vec3(1.0));
    float a = clamp(acc.a, 0.0, 1.0);
    fragColor = vec4(rgb, a);
}
