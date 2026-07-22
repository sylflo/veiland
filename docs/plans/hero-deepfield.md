# Plan: the "Deep Field" astronomy hero scene

Status: DESIGN (2026-07-23). Not built. The chosen HERO scene — a composed,
z-layered "deep field astronomy" lockscreen. Decided to reproduce it FAITHFULLY
(the full mockup, not a subset). Kept UNTRACKED in `docs/plans/` per convention.

## The spec IS the mockup

The reference is a working HTML/WebGL mockup at `mockups/veiland-lockers/`
(index.html + shaders.js + gl.js + scene.js + styles.css). Open index.html, "Deep
Field / astronomy" tab. It renders the exact composition we want and even shows the
plugin/z_index stack in its HUD — read those files as the design source of truth.
`SHADER_NEBULA` in shaders.js is a real, portable GLSL ES 1.0 fragment shader.

## The layer stack (bottom → top, from the mockup's HUD)

| z    | layer               | type      | veiland status / effort |
|------|---------------------|-----------|-------------------------|
| -100 | raymarched-nebula   | GPU shader| **needs veiland-shader**; PORTS ~verbatim from SHADER_NEBULA (fbm gas). Easiest shader. |
| -80  | parallax-stars ×3   | particles | existing particle family; needs a "stars" preset (3 depth layers, twinkle, some gold). TUNE. |
| -40  | aurora              | GPU shader| **needs veiland-shader**; NET-NEW authorship — the mockup lists it but has NO shader for it. Medium. |
| -20  | moon-sphere         | GPU shader| **needs veiland-shader**; the mockup FAKES it (CSS radial gradient + crater blobs). A real raymarched lit sphere is the HARDEST piece — ray-sphere intersect, lighting, crater normals. Could balloon. |
| 10   | meteor (emitter)    | particles | particle emitter variant (occasional streaks). TUNE. |
| 40   | ephemeris-hud       | data      | the readout (SEEING / MOON phase / MAG LIMIT / LST / site). Mockup FAKES the values. Doing it "live" (real sky data) is its own sub-project; the cheap cut is a markup/data widget with COMPUTED values (moon phase + LST are simple; seeing/site are config). SCOPE DECISION below. |
| 100  | [core] password + clock | core  | EXISTS (simpler than the mockup's serif clock + glass pill; styling is config/core polish). |

## The two tiers (this scene is why the user wanted both)

- **STATIC hero — ships WITHOUT veiland-shader.** Render the nebula ONCE to an image
  (offline / screenshot the shader), use it as a wallpaper-plugin background (+ blur
  from v0.2.0), and compose stars + moon-image + clock + password on top. A still
  deep-field. This is a real Reddit thumbnail achievable in the v0.2.0 hero slot.
- **ANIMATED hero — needs veiland-shader.** The LIVE nebula (gas churning via
  u_time), the animated aurora, the raymarched moon, live meteors. The "super clean
  animated" version — a bigger, later, arguably STRONGER post ("live, GPU,
  process-isolated — no other locker can do this").

## The gating dependency: veiland-shader

THREE of the layers (nebula, aurora, moon) are GPU shaders, so the animated hero is
GATED on `veiland-shader.md` (the generic GLSL host plugin) existing. Good news: this
mockup gives veiland-shader its FIRST REAL PRESET (the nebula) — building the host
FOR this gorgeous nebula is far more motivating and scopeable than a generic host in
the abstract. SHADER_NEBULA uses exactly veiland-shader's planned convention
(u_res / u_time, fullscreen quad, Shadertoy-ish), so it ports nearly verbatim.

Implication for the roadmap: the ANIMATED deep-field hero pulls veiland-shader
forward from "someday big track" to "the thing right after the static hero." The
static hero stays in/near v0.2.0; the animated one becomes its own arc.

## Suggested staging (an ARC, not one release)

1. **Static deep-field hero** (v0.2.0 hero slot) — nebula-as-image wallpaper + blur +
   stars particles + moon image + clock + password. No shader plugin. One Reddit post.
2. **veiland-shader host + nebula preset** — the generic GLSL plugin, proven on the
   nebula. Now the backdrop is LIVE. (veiland-shader.md's PRs.)
3. **Animated hero v1** — live nebula + stars + meteor emitter + clock. The churning
   backdrop is 80% of the "wow"; ship this before the hard shaders. Second Reddit post.
4. **The hard shaders** — aurora (author) then the raymarched moon (hardest). Polish
   passes on top of a hero that already reads well; don't block the animated post on
   them.
5. **Ephemeris data** — last, and only after the scope decision below. The scene reads
   as "astronomy" from the visuals alone; the readout is garnish.

## Day/night behavior — deliberately TIMELESS (verified against the mockup)

The Deep Field scene does NOT change color with the time of day, and that is
correct. In the mockup, the sun-tracking / hour logic is guarded to the OTHER scene
(quietdusk/shinkai — `if (theme === 'quietdusk')` in scene.js); SHADER_NEBULA takes
only `u_time` (animation churn) + `u_res`, no sun, no hour. A deep-field is a night
sky by definition — it looks the same at 3am and 3pm. Deep field uses the real clock
ONLY for the clock TEXT and the readout's LST number, never the visuals.

So: keep the nebula/aurora/moon shaders TIMELESS. If we ever want the scene to
"respond to the real sky," that belongs in the EPHEMERIS LAYER (real moon phase via
date math, LST, what's actually overhead), NOT in recoloring the shaders. This is
another reason the ephemeris data layer is worth its own scope decision (below) —
it is where any genuine time/sky response would live.

## Open scope decisions (settle before building the relevant piece)

- **Raymarched moon** — real raymarched lit sphere (hardest, most faithful) vs. a
  cheaper textured/gradient moon quad that looks 90% as good in-scene. Decide by a
  render probe; the moon is small on screen, so the cheap version may well win.
- **Ephemeris "live"** — real ephemeris (moon phase + LST are simple date math;
  actual constellations overhead is a real astronomy calc / data source = big) vs. a
  markup-style widget with computed moon-phase + LST + config'd site/seeing. Strongly
  lean the cheap computed version for v1; "real sky data" is a headline-chaser that
  is its own project.
- **Two-tier authorship** — the static hero's nebula IMAGE and the animated hero's
  nebula SHADER should match visually (same palette/params) so upgrading tiers is
  seamless. Author the shader first, render the still FROM it.

## Verify plan (per stage)

- Static: render the composed scene at 1080p + 4K; reads as a polished deep-field
  lockscreen; regions land proportionally.
- Shader host: nebula compiles + renders in the real plugin; the churn is smooth and
  cheap (it is fbm, not a heavy raymarch); survives the dmabuf import path.
- Composed animated: the full z-stack composites correctly (shader under particles
  under core); no flicker; the clock/password read clearly over the busy backdrop
  (may need the scene's vignette/grain, which the mockup has as #vig/#grain).

## References

Mockup: `mockups/veiland-lockers/` (the spec). Depends on: veiland-shader.md (host),
blur.md (static-tier wallpaper blur), widget-roadmap.md (hero-preset context),
[[project_recording_gallery_gifs]] (capture the eventual screenshot/GIF).
