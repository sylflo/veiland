# Plan: veiland-shader (generic GLSL host plugin)

Status: not started. Four PRs, done one-per-context in the order below.
Read this file first in each new session.

## Context

A plugin whose fragment shader is read from a file (or an embedded preset)
instead of being baked into a Rust binary. The plugin concatenates a
Shadertoy-convention preamble + the user's GLSL + a `main()` wrapper that
calls `mainImage()`, compiles at startup, and renders it as a self-paced
fullscreen quad. Users get an effectively unlimited supply of animated
backgrounds without writing Rust; each shader still runs process-isolated
like every other plugin.

```toml
[[plugin]]
name    = "shader"
binary  = "/usr/bin/veiland-shader"
z_index = 0
[plugin.config]
preset       = "nebula"        # embedded shader shipped in the binary
# or: path   = "/abs/mine.frag"  # user-supplied file (path wins if both set)
render_scale = 1.0             # 0.1..=1.0, buffer size as fraction of region
max_fps      = 30              # 0..=240, 0 = compositor rate
loop_seconds = 0               # 0 = raw iTime; >0 wraps time (see below)
```

The skeleton is the raymarcher: it already implements `render_scale`
(`scaled_dim` + Reconfigure rescale) and `max_fps` (frame-budget sleep),
and its run()/FramePacer shape is the shared shape of all three procedural
plugins. veiland-shader is roughly: raymarcher minus State/camera/palette,
plus file loading, preamble assembly, error handling, and the Shadertoy
uniform set. Estimate ~500 lines.

Design review happened 2026-07-18 (full argument in that session); the
conclusions are recorded below as settled so they are not re-derived.

## Decisions settled — do not re-litigate

- **Shadertoy-compatible, single-pass, v1.** The preamble implements
  Shadertoy's interface and nothing else. No veiland-specific uniforms, no
  config→uniform injection. Multipass (Buffer A-D), cubemap channels,
  keyboard/audio/video channels, `mainVR`: out of scope. This excludes
  many top-100 spectacle shaders (fluid sims, accumulating path tracers);
  the README must say so on day one. **Not a one-way door:** multipass
  lives entirely inside this plugin's process (internal FBO passes, still
  one dmabuf submitted per frame) — adding it later is additive TOML in
  this plugin's config schema. The protocol and SDK API never see it.
- **Presets are embedded via `include_str!`**, not files on disk. No
  share-dir, no path resolution, no packaging changes (flake, PKGBUILD,
  .deb, .rpm all untouched), atomic versioning. `path =` covers users'
  own files. Preset *names* are a one-way door (referenced from user
  configs) — treat renames like CLI-flag breaks.
- **gradient / blobs / raymarcher stay separate binaries.** Their value is
  CPU-side f64 animation state (wrapped phases, seeded Lissajous waves,
  camera basis) that raw f32 `iTime` cannot express without reintroducing
  the unbounded-time precision bug those plugins were engineered to avoid.
  Porting them would also need config→uniform injection (scope creep) and
  break user configs for zero benefit. No `.frag` approximations of them
  as presets either — near-duplicates confuse.
- **Output is forced opaque.** The wrapper writes `vec4(c.rgb, 1.0)`.
  Shadertoy ignores alpha and shaders routinely write garbage there; under
  the host's premultiplied blend that bleeds additively over lower
  z-layers. Opaque matches gradient/blobs/raymarcher. An alpha/overlay
  mode is possible future work, not v1.
- **fragCoord is y-flipped in the wrapper:**
  `vec2(gl_FragCoord.x, iResolution.y - gl_FragCoord.y)`. Shadertoy's y
  grows up on screen; the host samples the buffer flipped (same flip the
  raymarcher does for its NDC). Without this every shader is upside down.
  One-way door: locked at v1.
- **iChannel0 gets a built-in procedural 256×256 RGBA white-noise texture;
  iChannel1-3 get 1×1 black.** A large cohort of single-pass shaders uses
  iChannel0 purely as a noise LUT; noise-by-default converts them from
  wrong/black to approximately correct. Deviates from Shadertoy's
  unbound=black, deliberately. One-way door: changing the default binding
  later changes rendering of deployed setups. Per-channel image config
  (`iChannel0 = "path.png"`) is possible later, additive.
- **iTime is raw f32 seconds since plugin start** (Shadertoy-faithful),
  with an optional `loop_seconds` knob that wraps it for long-lock users
  who accept a periodic pop. Document the degradation: past ~2 days
  locked, f32 resolution makes iTime-driven motion visibly stutter. Do
  not silently wrap.
- **Failure contract = the wallpaper's**: never exit on content failure.
  Stay alive, render the built-in fallback (solid very dark fill — NOT a
  pretty preset; failure should be visible-but-safe), log loudly.
- **Compile after the handshake** (connect → Hello → wait_for_configure →
  compile), the raymarcher's existing order. GLSL compilers have no time
  bounds; a pathological shader compiling forever is then just a silent
  plugin (legal after Hello), region on fallback, locker untouched.
- **Defaults: `render_scale = 1.0`, `max_fps = 30`.** Cost of an arbitrary
  shader is unknown; 30 fps caps the duty cycle on unattended locked
  laptops. Full render scale because cheap gradient-type shaders look
  blurry at 0.5; docs nudge heavy-shader users toward lowering it.
- **No new dependencies.** iDate derives from the latest Configure's
  `time_unix_seconds` + `time_tz_offset_seconds` + elapsed-since-receipt
  (no chrono). Noise texture is generated with the SDK's `Rng`.

## Security posture (recorded, not open)

The config's `binary` key already executes arbitrary user-specified
binaries, so a user-chosen `.frag` adds no new authority — it is strictly
weaker than what the config already permits. What changes is the social
surface: pasting text is lower-friction than installing binaries. Docs
must state plainly: a shader file is code you are choosing to run on your
GPU, same trust level as installing a plugin.

Walked in the review: driver-compiler exploitation lands in the untrusted
plugin process (existing hostile-plugin scenario, same residual risks);
GPU VA isolation covers cross-buffer reads (absent driver bugs, same
class as the import path today); GPU hangs are bounded by kernel
hangcheck/reset and were always reachable from a malicious plugin binary;
the host's plugin-fence wait is already `eglClientWaitSync` with a 1 s
timeout treated as plugin death (`veiland-core/src/plugin/sync.rs`).
**No core changes required.** The host also never respawns dead plugins,
which caps repeated-hang loops at one per lock session.

Licensing (for presets and contributions): Shadertoy's default license is
CC BY-NC-SA 3.0 Unported — NOT GPL-compatible (NC is non-free for
Debian/Fedora/AUR purposes; SA 3.0 has no GPLv3 path). Presets must be
original GPL-3.0-or-later work, or explicitly CC0 / MIT / BSD /
CC BY 4.0 / CC BY-SA 4.0 (the 4.0-SA one-way compatibility is official).
Reimplementing a *technique* from scratch is fine; a port is a
derivative. Users running NC shaders privately via `path =` is a
non-issue (NC/SA bite on redistribution). This goes in the docs PR.

---

## PR 1 — SDK: GLES 3 context option in `GbmEgl`  [THE BLOCKER]

`GbmEgl::new()` hardcodes `CONTEXT_CLIENT_VERSION, 2` and
`OPENGL_ES2_BIT` (`veiland-plugin/src/render.rs`). Shadertoy shaders are
`#version 300 es`, which requires an ES3-capable context. Mesa often
returns a 3.x-backwards-compatible context for a version-2 request, but
that is a driver courtesy, not a spec guarantee — request it explicitly.

- Factor the body of `new()` into a private
  `new_with_es_version(major: i32)`; public `new()` stays ES2 and
  byte-for-byte equivalent in behavior (existing plugins untouched).
  Add `new_es3()` requesting version 3 with
  `EGL_OPENGL_ES3_BIT` (`0x40` — hand-rolled const, same idiom as
  `EGL_PLATFORM_GBM_KHR`) in `RENDERABLE_TYPE`, falling back to the
  ES2 bit if config selection finds no match.
- `new_es3()` returns Err when ES3 is unavailable. veiland-shader then
  falls back to `new()` + fallback-shader-only mode with a clear log
  ("host GL stack has no GLES3; user shaders unavailable").
- ES 3.x contexts still compile `#version 100` shaders (spec-guaranteed
  backwards compat), so the plugin's fallback shader can be ES 1.00 and
  work in both modes.

**Verify (user runs, both boxes).** Existing plugins behave identically
under a normal lock (no SDK regression). ES3 proof rides with PR 2 — or
sooner via a throwaway `#version 300 es` compile in a scratch binary if
wanted.

---

## PR 2 — the plugin: embedded presets end-to-end

New crate `plugins/shader` → binary `veiland-shader`. Raymarcher-derived
skeleton (keep its `scaled_dim`, Reconfigure rescale, frame-budget
sleep). New logic:

- **Config**: `preset` / `path` / `render_scale` / `max_fps` /
  `loop_seconds` with the raymarcher's `sane()` clamping. Both
  `preset` and `path` set → use `path`, log. Unknown preset → fallback +
  log listing valid names.
- **Preamble** (assembled as its own source string):
  - `#version 300 es`, `precision highp float; precision highp int;`,
    `out vec4` color output.
  - Full uniform set — undeclared uniforms are compile errors in any
    shader that mentions them, unused ones cost nothing:
    `iResolution` (**vec3**: w, h, 1.0), `iTime`, `iTimeDelta`,
    `iFrame` (int), `iFrameRate`, `iDate` (vec4, Shadertoy's JS quirks:
    month 0-based, day 1-based, .w = seconds since local midnight incl.
    fraction — verify against a clock shader on the real site),
    `iMouse` (vec4 zeros; no pointer protocol exists),
    `iSampleRate` (44100.0), `iChannelTime[4]` (zeros),
    `iChannelResolution[4]`, `iChannel0..3` (sampler2D).
  - Compat shim: `#define texture2D texture` (pre-WebGL2 shaders). Keep
    shims minimal and listed in docs.
  - Wrapper `main()`: y-flipped fragCoord, forced alpha 1.0 (both per
    settled decisions above).
- **Uniform state per frame**: iTime from `Instant` (wrapped iff
  `loop_seconds > 0`), iTimeDelta measured between renders, iFrame
  counter, iFrameRate smoothed, iDate from the latest Configure's time
  fields + elapsed. Latch Configure time on every Reconfigure.
- **Noise channel**: generate 256×256 RGBA white noise with `Rng` at
  startup, upload once, bind to iChannel0; 1×1 black on 1-3;
  `iChannelResolution` to match.
- **Fallback shader**: trivial `#version 100` dark fill, compiled at
  startup before the user shader so it is always available. Any
  user-shader failure (this PR: unknown preset, compile/link error)
  switches to it without exiting.
- **Compile-log handling**: query `GL_INFO_LOG_LENGTH` and fetch the full
  log rather than the SDK helper's 1 KiB truncation — the log is the
  product here. (Either extend `vgl` or compile directly in-plugin;
  prefer whichever is less churn, decide in-flight.)
- 2-3 embedded presets, original work written for this PR, GPL. Names
  chosen carefully (one-way door).

**Verify (user runs, both boxes: Intel/Mesa + NVIDIA).**
- Lock with a preset → renders and animates on both boxes.
- Orientation: a deliberately asymmetric test shader (distinct
  top/bottom) renders right-side up.
- `kill -9` the plugin mid-lock → region falls back, locker fine.
- Unknown preset name → dark fallback + log listing presets.
- `max_fps = 5` → visibly chunky updates, GPU usage drops;
  `render_scale = 0.25` → soft but correct upscale.
- Multi-monitor: one instance per output, independent surfaces.

---

## PR 3 — user files: `path =`, line numbers, `--check`

- **File loading**: read `path`, UTF-8-validate. Missing/unreadable/not
  UTF-8 → fallback + log (never exit).
- **Line-number correction.** Pass preamble and user source as two
  separate `glShaderSource` strings and emit `#line 1` before user code.
  Neither is portable alone (NVIDIA numbers across the concatenation;
  `#line` has a known off-by-one ambiguity across GLSL
  versions/drivers), so **calibrate empirically at startup**: compile a
  tiny known-bad probe through the same preamble path once, parse where
  the driver says the error is vs. where it is, derive the offset, apply
  it when rewriting the real log's line references. ~30 lines, immune to
  driver convention.
- **`--check` mode**: `veiland-shader --check file.frag` compiles
  headlessly (GbmEgl, no host connection), prints the corrected log to
  stdout, exits 0/1. Without it the edit-test loop is "lock your screen,
  read journalctl."

**Verify (user runs, both boxes).**
- A real single-pass Shadertoy shader via `path =` renders correctly
  (any license — local use).
- Introduce a syntax error at a known line → reported number matches the
  user's file on BOTH drivers (this is the calibration proof).
- `--check` on the broken file prints the same, exits nonzero; on a good
  file exits zero.
- Missing file → dark fallback + log; locker unaffected.

---

## PR 4 — docs + preset licensing rules

- `docs/config.md`: the `[plugin.config]` reference for veiland-shader.
- Plugin docs (`docs/plugins.md` is GENERATED by
  `scripts/gen-plugins-md.py` and CI-checked — add the plugin to that
  pipeline's source, never edit the output).
- README/plugin doc: the supported subset stated plainly ("single-pass
  Image shaders; no Buffer A-D, no cubemap/keyboard/audio channels"),
  the "a shader file is code" trust framing, the iTime long-session
  note, the iChannel0-is-noise note, thermal guidance
  (render_scale/max_fps).
- CONTRIBUTING (or plugin README): preset licensing rules from the
  Security/licensing section above — before the first external preset
  PR arrives, not after.

**Verify.** `gen-plugins-md` CI check passes; docs build/render clean.

---

## Explicitly out of scope for v1

Multipass, cubemap/keyboard/audio/video channels, `mainVR`, per-channel
image config, alpha/overlay mode, config→custom-uniform injection,
hot-reload of the shader file, auto-degradation on slow frames (a
frame-time warning log is optional polish), preset metadata (magic
comments for recommended settings).

## Why this order

- PR 1 first because nothing compiles `#version 300 es` without it, and
  it is the only SDK change — land it small and alone so SDK history
  stays clean.
- PR 2 before PR 3 so the end-to-end pipeline (preamble, uniforms,
  orientation, fallback) is proven on embedded content where file-I/O
  failure modes can't confuse debugging.
- PR 3 isolates the user-facing failure surface (files, driver log
  formats, calibration) — the fiddliest cross-driver part, worth its own
  context.
- PR 4 last, once behavior is fixed and there is something true to
  document.

Each PR verified on both dev boxes (Intel/Mesa + NVIDIA). The user runs
builds/tests; verification steps are written for the user to execute.
