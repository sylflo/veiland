// SPDX-License-Identifier: GPL-3.0-or-later
// meteor - veiland-shader preset. Original work. TRANSPARENT overlay: set
// opacity < 1.0 in the layer config so the streak composites over the layers
// below. Everything but the meteor and its tail is alpha 0.
//
// A periodic shooting star for the "Deep Field" astronomy scene: a bright head
// with a short warm tail sweeps diagonally across the upper sky, fading in and
// out, then repeats after a pause. Unlike the mockup's meteor (which fires at
// random intervals from random directions via JS), a pure iTime shader is
// deterministic, so this cycles on a fixed schedule - each pass takes a
// different diagonal path, seeded from the pass number, so it does not look
// like the exact same streak every time.

// 1D hash for per-pass variation (entry point, angle, side).
float hash11(float n) {
    return fract(sin(n * 12.9898) * 43758.5453);
}

void mainImage(out vec4 fragColor, in vec2 fragCoord) {
    vec2 uv = fragCoord / iResolution.xy;      // [0,1], y up
    float asp = iResolution.x / iResolution.y;
    // Aspect-corrected space so the streak is straight, not sheared.
    vec2 P = vec2(uv.x * asp, uv.y);

    // Timeline: one meteor every PERIOD seconds; it is only visible for the
    // first STREAK seconds of each period (a brief flash), dark the rest.
    const float PERIOD = 8.0;
    const float STREAK = 1.9;                  // seconds the streak is alive (slower sweep)
    float pass = floor(iTime / PERIOD);        // which meteor (for seeding)
    float local = iTime - pass * PERIOD;       // seconds into this pass

    // Only compute the streak while it is alive; otherwise fully transparent.
    float rgbA = 0.0;
    vec3 col = vec3(0.0);
    if (local < STREAK) {
        // Per-pass randomness: which side it enters, its vertical band, and its
        // downward slope. Widely scattered so consecutive meteors feel random:
        // entry height roams most of the sky and the angle varies a lot. Each
        // attribute uses a distinct hash multiplier so side/height/slope do not
        // move together in a visible pattern.
        float side = step(0.5, hash11(pass * 1.7 + 1.3));   // 0 = from left, 1 = right
        float y0 = 0.30 + hash11(pass * 2.3 + 2.7) * 0.62;  // entry height, most of the sky
        float slope = 0.12 + hash11(pass * 3.1 + 5.1) * 0.55; // downward drop, wide range

        // Head travels across the full aspect width over STREAK seconds. From
        // left: x goes 0 -> asp; from right: asp -> 0. y falls by `slope`.
        // Express the path as start + vel*prog so `dir` below is the head's
        // EXACT travel direction (per-unit-prog velocity), not an approximation.
        float xSign = (side > 0.5) ? -1.0 : 1.0;
        float xTravel = xSign * (asp + 0.30);               // total x distance
        vec2 start = vec2((side > 0.5) ? asp + 0.15 : -0.15, y0);
        vec2 vel = vec2(xTravel, -slope);                   // per-prog velocity
        float prog = local / STREAK;                        // 0..1 along the path
        vec2 head = start + vel * prog;

        // Tail direction = the head's actual travel direction. Using the real
        // velocity (which is much more horizontal than +/-1 on a wide screen)
        // keeps the tail collinear with the path, so the streak is one straight
        // line instead of kinking where head and tail disagree.
        vec2 dir = normalize(vel);

        // Distance from this pixel to the meteor line segment (head back along
        // -dir for TAIL length). `along` is the projection onto the tail axis;
        // `perp` is the perpendicular distance to the (infinite) line.
        const float TAIL = 0.28;               // tail length in height-units (longer)
        vec2 rel = P - head;
        float along = dot(rel, -dir);          // >0 behind the head
        float perp = abs(dot(rel, vec2(-dir.y, dir.x)));  // signed-perp magnitude

        // Straight streak: a thin gaussian across the line width, drawn ONLY
        // where the pixel projects onto the [0, TAIL] segment. Gating on `along`
        // (rather than measuring distance to a clamped endpoint) gives a clean
        // straight cut at both ends instead of a rounded blob at the tail tip.
        float width = 0.0016;
        float onSegment = step(0.0, along) * step(along, TAIL);
        float line = exp(-(perp * perp) / (2.0 * width * width)) * onSegment;
        float taper = 1.0 - clamp(along, 0.0, TAIL) / TAIL;   // 1 at head -> 0 at tip
        taper = pow(taper, 1.5);               // graceful fade along the longer tail

        // A small bright point right at the head (the meteor "rock"). Tight
        // falloff so it stays a compact point of light, not a big bloom.
        float headGlow = exp(-length(P - head) * 200.0) * 0.9;

        // Overall fade in-and-out across the streak's life (sin envelope, like
        // the mockup's sin(life/max * PI)).
        float env = sin(prog * 3.14159265);

        float lum = (line * taper + headGlow) * env;
        col = vec3(1.0, 0.94, 0.82) * lum;     // warm white
        rgbA = clamp(lum, 0.0, 1.0);
    }

    fragColor = vec4(col, rgbA);               // wrapper premultiplies
}
