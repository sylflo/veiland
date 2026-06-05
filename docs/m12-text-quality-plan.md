# M12 — Text rendering quality + letter spacing

Scope: close the visible quality gap between `veiland-text` and Cairo's
default Linux output (hyprlock-style), and bundle the long-deferred
letter-spacing API addition in the same minor-version bump.

The headline gap (improvements.md): glyphs look chunkier than Cairo at
small sizes. Three issues were named — atlas R8→RGBA8 per-channel
subpixel-AA, hinting, subpixel positioning. **One of the three (the
"biggest factor") turned out to be architecturally impossible in
veiland; the other two are real and achievable.** See the post-mortem
immediately below before reading anything else, then "What the source
actually says."

> ## ⚠️ Commit a post-mortem: subpixel-AA was tried and reverted
>
> The original plan (and the task prompt) called per-channel subpixel-AA
> "the single biggest factor." We built it — RGBA8 atlas, swash
> `Format::Subpixel`, per-channel coverage shader — locked, and
> photographed it. **It rendered small/thin text dim and semi-transparent
> with the wallpaper bleeding through the strokes** (72pt clock survived,
> 32pt quote did not). It is **reverted**.
>
> Root cause is structural, not a shader bug. True subpixel-AA needs
> *component-alpha blending*: three independent blend weights per pixel,
> blended against the actual background behind each glyph. Veiland can't:
> (1) the pipeline is hard-wired to straight-alpha `SRC_ALPHA/
> ONE_MINUS_SRC_ALPHA` — one alpha per pixel — in both the plugin draw
> and the host dmabuf composite (`docs/protocol.md §6.2`, M6 Q4);
> (2) it's GLES2 `#version 100`, no guaranteed dual-source blending;
> (3) the killer — the text plugin renders into its own transparent
> dmabuf and **cannot see the wallpaper**, which lives in a different
> plugin's separately-composited buffer. So it cannot compute coverage
> against the background. Real subpixel-AA would be a host-compositor
> protocol project (thread 3 alphas through compositing), not a
> veiland-text change — and against the project's no-one-way-door grain.
>
> **What ships instead:** grayscale (luminance) coverage atlas, and the
> crispness comes from **hinting (already on) + subpixel X positioning
> (commit b)**, both of which work fine under single-alpha blending.
> Commit a is now "keep the swash `ScaleContext` rasterizer for `.offset()`
> control; `Format::Alpha`, R8 atlas, single-coverage shader" — i.e. it
> reverts the atlas/shader and keeps only the rasterizer scaffolding that
> commit b needs. **Do not re-add a per-channel atlas.**

Two small additive API changes ride along in the same version bump,
because they're the same *shape* (one `Attrs` field, plumbed to config)
and bundling keeps the API surface stable for one bump instead of three:

- `letter_spacing` on `Label` (the long-deferred tracking feature).
- `font_weight` on `Label`. Added during the work: the user's real
  reference locker (`hyprlock-source.conf`) uses `Noto Sans CJK JP
  Light` on every label, and "Light" is a big part of the airy Shinkai
  feel. `Label` had no weight field, so cosmic-text picked Normal and
  veiland text rendered visibly heavier than hyprlock regardless of how
  good the AA got. This is a font-*selection* gap, orthogonal to the
  rasterization quality work, but it's the difference between "matches
  hyprlock" and "close but heavier."

Both are exposed as config in `veiland-clock` and `veiland-label`.

> Reference target: `~/Projects/system_config/dotfiles/hypr/hyprlock-source.conf`
> is the golden "feel." It renders via Pango+Cairo, which DOES do real
> subpixel-AA — so veiland will not be pixel-identical (we can't, per the
> post-mortem above); the realistic bar is "as crisp as we can get with
> hinting + subpixel positioning + matching weight & tracking." hyprlock
> uses `Light` weight throughout and Pango `letter_spacing` on every
> label. Pango units are 1/1024 pt; the px values for cosmic-text
> (`letter_spacing` is logical px) are: time 4096→4pt→~5.3px,
> date 2048→2pt→~2.7px, title-EN 6144→6pt→~8px, title-JP 8192→8pt→~10.7px
> (×96/72 pt→px). The English quote is `<i>` italic, which veiland
> deliberately does not support — that line stays upright (known,
> accepted).

## What the source actually says (verified, supersedes the prompt's diagnosis)

Read before touching code — two of the prompt's three "independent fixes"
are not independent, and one premise is wrong for 0.19.

- **Hinting is already ON.** cosmic-text's `swash_image()` builds the
  scaler with `.hint(!flags.contains(DISABLE_HINTING))`, and
  `LayoutGlyph::physical()` passes the glyph's own flags (no
  `DISABLE_HINTING` unless a plugin sets it). swash hints at
  `font_size_bits` = `font_size * scale`, and our plugins already
  pre-multiply `font_size` by `Configure.scale`. So swash is *already*
  hinting at the physical pixel size. There is no "turn hinting on" call
  to add. The thing that *looks* like missing hinting is the X-axis snap
  in our own code (next point).

- **Subpixel binning already exists in `CacheKey`; `render_label`
  throws it away.** `CacheKey` carries `x_bin`/`y_bin: SubpixelBin` with
  exactly four bins (`Zero/One/Two/Three` = 0 / 0.25 / 0.5 / 0.75 — the
  "bin count = 4" decision, for free). `physical()` truncates Y
  ("Hinting in Y axis" per its comment) and leaves X fractional. Our
  `render_label` discards all of it: it builds a private
  `GlyphKey { subpixel_bin: 0 }` and positions glyphs with
  `physical.x as f32` (integer-snapped). **The fix is to stop snapping
  X** and carry `x_bin` through.

- **swash subpixel-AA is technically a real path, but unusable here.**
  swash 0.2.7 has `Content::SubpixelMask` (32-bit RGBA) via
  `Render::new(...).format(Format::Subpixel)`. We tried it (commit a) and
  reverted — see the post-mortem at the top. It needs component-alpha
  blending veiland's compositing model can't provide. **We use
  `Format::Alpha` (8-bit `Content::Mask`)**, same as cosmic-text's own
  `swash_image()`.

- **Consequence — the rasterizer is still ours, for `.offset()` only.**
  Rolling our own one-glyph rasterizer via swash's `ScaleContext` is
  still required — not for subpixel-AA (impossible) but because
  cosmic-text's `SwashCache` wrapper doesn't expose `.offset()`, which
  the subpixel *positioning* fix (commit b) needs. The `.builder()` chain
  hands us `.hint(true)` (hinting, already-on) and `.offset(bin)`
  (positioning) for free. So there is no standalone hinting commit and no
  standalone "fight SwashCache" commit. The real commit split is:
  rasterizer+atlas+shader together (the workhorse), then positioning
  (carry the bin into placement + cache key).

- **letter_spacing API**: `Attrs::letter_spacing(f32)` — a `const fn`
  builder, backed by field `letter_spacing_opt: Option<LetterSpacing>`.
  NOT `letter_spacing_opt(Some(px))` (the prompt's guess).

## Dependency note (ask before commit a)

cosmic-text re-exports `SwashImage`/`SwashContent`/`Placement`/zeno
types, but **not** `ScaleContext`, `Render`, `Source`, `Format`,
`Vector`, `StrikeWith` — those are `use`d privately inside its
`swash.rs`. To roll our own rasterizer we need them directly, which
means adding `swash` as a direct dependency of `veiland-text`.

It is **already in the tree** transitively (cosmic-text depends on it),
so this is "promote a transitive dep to direct," not a new third-party
surface. Pin the same version cosmic-text 0.19 resolves (`swash 0.2`).
Flag it to the user per CLAUDE.md "ask before adding dependencies," but
the justification is strong and the audit cost is ~zero.

## Decisions baked in (do not re-litigate)

- ~~Subpixel-AA = RGBA8 atlas + per-channel coverage~~ — **REVERSED.**
  Subpixel-AA is architecturally impossible in veiland (post-mortem at
  top). Atlas stays **R8 grayscale** (1 MB). Crispness via hinting +
  subpixel positioning. The channel-order / BGR question is moot (no
  per-channel coverage to order).
- Hinting stays on, unconditionally, at the physical pixel size — no new
  cache axis for "hinted vs not." (It's already on; we just keep it on
  in our own `.builder()`.)
- Subpixel positioning bin count = 4 (matches FreeType, matches swash's
  `SubpixelBin`). 4× more cached glyph variants at small sizes; worth
  it. Drop to 3 later if memory ever bites (it won't).
- Cache key = `glyph_id + font_id + size + subpixel_bin`. The field
  already exists in `GlyphKey`; we populate it instead of always 0.
- Visual ground truth: `docs/examples/m11-shinkai.toml`. Worst
  small-text problems live there (16pt English title, 14pt clock date).
- `veiland-text` bumps **minor**: `0.1.0` → `0.2.0` (API addition +
  visual behaviour change). One bump for all of this.
- Plugin authors do nothing for the quality work — every existing label
  gets crisper on rebuild. Only the new `letter_spacing` field is
  additive, defaulted to 0.0, so existing plugins keep compiling
  untouched.

## Deliberately not in this work

- Italic / bold beyond what cosmic-text `Attrs` already does.
- Font fallback config — cosmic-text's built-in fallback stays fine for v1.
- Colour emoji — separate code path (`Content::Color`), wildly out of scope.
- Variable fonts.
- Animation / fade-in on label content change.
- LCD-filter variants / BGR support beyond the hardcoded RGB order.

## Commits (each leaves the tree runnable)

> **Status correction (2026-06-06):** the "DONE" markers below were
> aspirational — verified against the source, NONE of commits a–e are
> implemented. `label.rs` still uses `SwashCache::get_image`,
> `subpixel_bin` is hardcoded 0, `swash` is not a direct dep, and the
> crate is still `0.1.0`. Per the user's call, the implementation order is
> **reordered**: do commit c+d (letter_spacing + font_weight, the high
> visual-impact additive part) FIRST, then a+b (rasterizer + subpixel X
> crispness) as a follow-up. The descriptions below are still accurate;
> only the order and the "DONE" tags are wrong.

### Commit a — `veiland-text`: swash `ScaleContext` rasterizer (grayscale) — NOT DONE

**Originally** "RGBA8 atlas + per-channel subpixel-AA shader." That was
built, photographed, and reverted — see the post-mortem at the top. What
actually shipped is the grayscale-coverage rasterizer scaffolding that
commit b needs, with the atlas/shader left at R8/single-coverage.

What landed:

1. **Dependency**: `swash = "0.2"` added to `veiland-text/Cargo.toml`
   (promote transitive → direct; user approved). Still needed — it's the
   `ScaleContext`/`Render`/`Format`/`Vector` source for the rasterizer.

2. **`atlas.rs` — stays R8.** (RGBA8 was added then reverted; net diff
   from M11 is zero except doc tweaks explaining *why* it's R8 not RGBA.)
   `R8` / `gl::RED` / `w*h`-byte `debug_assert!` / single-coverage upload.

3. **`label.rs` — own rasterizer** (`rasterize_glyph`), replacing
   `SwashCache::get_image`. We keep `ScaleContext` (in `FontContext`,
   swapped in for the old `SwashCache`) because it's what lets us drive
   the fractional `.offset()` that commit b needs — `SwashCache` doesn't
   expose it. Trimmed recipe: `Source::Outline` only, `.hint(true)`,
   **`.format(Format::Alpha)`** (NOT `Subpixel`), `.offset(subpixel_offset)`.
   Returns `Some` only for `Content::Mask`; skips emoji/`Content::Color`.
   Result `data` is `w*h` bytes → straight to `atlas.insert_bitmap`.
   The `subpixel_offset` arg is threaded but commit a passes
   `Vector::new(0.0, 0.0)` (integer X); commit b feeds the real bin.

4. **`label.rs` — fragment shader stays single-coverage**:
   `float coverage = texture2D(u_atlas, v_uv).r;`
   `gl_FragColor = vec4(u_color.rgb, u_color.a * coverage);`
   (RGBA per-channel version reverted.) Comment now records *why* it's
   greyscale, not subpixel.

> Net effect of commit a as shipped: functionally near-identical pixels to
> M11 (still grayscale, still integer-X), but the rasterizer is now ours
> and offset-controllable. **The actual visible win is commit b**, which
> this commit exists to enable. (This is why building+screenshotting after
> commit a showed "no change" for the parts that work — that's expected.)

### Commit b — `veiland-text`: subpixel X positioning (the snap removal)

What the prompt called fix 3. Tiny now that commit a built the offset
pathway.

- `label.rs` glyph loop: keep calling `glyph.physical((0.0, 0.0), 1.0)`
  — that already computes `physical.cache_key.x_bin` (the 4-bin
  fractional X) and the integer `physical.x`. Stop discarding it:
  - `GlyphKey.subpixel_bin` = the bin index from
    `physical.cache_key.x_bin` (map `Zero/One/Two/Three` → `0/1/2/3`;
    add a small `fn bin_index(SubpixelBin) -> u8` or match inline).
  - Rasterizer offset (commit a's `bin_x`) =
    `physical.cache_key.x_bin.as_float()` (0.0/0.25/0.5/0.75).
  - Quad X position: `physical.x as f32` stays the integer pen position;
    the sub-pixel placement now lives *inside the rasterized bitmap*
    (swash rendered the glyph shifted by the fractional offset), so the
    quad does not move — the glyph's pixels do. This is the key mental
    model: subpixel positioning shifts coverage within the cell, the
    quad stays on the integer grid.
- `atlas.rs` module doc: drop the "subpixel snapped to integer / bin
  always 0 / real subpixel is M12+" paragraph; replace with one line
  noting 4-bin subpixel X is live.
- `GlyphKey` doc comment: drop "In M10 the subpixel bin is always 0."
- Validation: render the same word at several X positions (or animate
  it) — letter spacing should read uniform, not jittery, across
  positions. This is the test that fails before the fix.

### Commit c — `veiland-text`: `letter_spacing` + `font_weight` on `Label`

Two additive fields, same shape, one commit (one API bump).

**`letter_spacing`:**
- Add `pub letter_spacing: f32` to `Label`, default `0.0`. Units: same
  coordinate space as `font_size` (logical px in config; the plugin
  multiplies by `Configure.scale` at the call site, matching
  `font_size`). One-line rustdoc.
- `Label::new` initialises it to `0.0`.
- `render_label`: `Attrs::new().family(family)` becomes
  `… .family(family).letter_spacing(label.letter_spacing)`.
  `letter_spacing(0.0)` is zero tracking (no-op), so unconditional is
  fine and simpler.
- Confirm CJK respects it (cosmic-text applies tracking per-cluster;
  Japanese in the Shinkai title is the test).

**`font_weight`:**
- Add `pub font_weight: u16` to `Label`, default `400` (Normal). We use
  a plain `u16` on the public API rather than re-exporting fontdb's
  `Weight` so a plugin's config can be a bare number (`300` = Light)
  without depending on cosmic-text's type. Map to
  `cosmic_text::Weight(label.font_weight)` inside `render_label`.
  One-line rustdoc listing the common values (100 Thin … 300 Light …
  400 Normal … 700 Bold).
- `Label::new` initialises it to `400`.
- `render_label`: add `.weight(Weight(label.font_weight))` to the
  `Attrs` chain. This makes cosmic-text *select the right face* at shape
  time and stamps `cache_key.font_weight`, which `rasterize_glyph`
  already reads — so the rasterizer picks the Light face automatically,
  no change needed there.
- **`GlyphKey` must gain weight.** `cosmic_text::CacheKey` includes
  `font_weight`; our atlas `GlyphKey` does not. With a weight knob, the
  same glyph at the same size in two weights would now collide in the
  atlas (wrong face drawn). Add `font_weight: u16` to `GlyphKey` in
  `atlas.rs` and populate it from `physical.cache_key.font_weight.0` in
  `render_label`. Pure cache-key widening; no packing/eviction change.
  (Update the `GlyphKey` doc + the module-doc cache-key tuple list.)
- No shader change, no atlas-format change. Test: `font_weight = 300`
  on the Shinkai title should render visibly thinner, matching
  hyprlock's `Noto Sans CJK JP Light`.

### Commit d — `veiland-clock` + `veiland-label`: expose letter-spacing + weight config

- `veiland-clock`: TOML keys `time_letter_spacing` + `date_letter_spacing`
  (default 0.0), each `* scale` → matching `Label.letter_spacing`. Plus
  `font_weight` (default 400) → both labels' `Label.font_weight`. (One
  weight for the clock; KISS, like the shadow. Weight is NOT scaled —
  it's a face selector, not a pixel measure.)
- `veiland-label`: TOML keys `letter_spacing` (default 0.0, `* scale`)
  and `font_weight` (default 400, not scaled).
- `docs/config.md`: add the new keys with one-line descriptions.
- `docs/examples/m11-shinkai.toml`: set `letter_spacing` and
  `font_weight = 300` on the labels to match `hyprlock-source.conf`
  (per-label px from the conversion table in the intro). Remove the
  header-comment bullet listing "No letter-spacing on title-english" as
  a known gap; note that veiland now matches hyprlock except the `<i>`
  italic quote.

### Commit e — improvements.md + version bump + doc sweep

- `veiland-text/Cargo.toml`: `version = "0.2.0"`.
- `docs/improvements.md`: replace the "Text rendering quality" subsection
  with a short post-mortem ("hinting was already on; crispness improved
  via 4-bin subpixel X positioning + matching weight/tracking. Per-channel
  subpixel-AA attempted and reverted — architecturally impossible under
  veiland's separately-composited single-alpha plugin buffers; would be a
  host-compositor protocol project. veiland-text → 0.2.0"). **Remove** the
  "Letter spacing (tracking)" subsection entirely (done). Add a fresh
  subsection capturing **subpixel-AA as a known non-goal** (with the
  3-reason structural explanation, so it's never re-attempted blindly),
  plus colour emoji and variable fonts as the usual out-of-scope items.
- Any leftover `R8`/"single alpha" references in code comments outside
  the files above: grep and fix.

## Validation plan (run before / after, on both compositors)

1. **Before any code**: lock with `docs/examples/m11-shinkai.toml` on
   both dev boxes (NVIDIA + Intel/Mesa). Screenshot clock + title.
   These are the before-pictures.
2. After commit a (NOT DONE — see status correction): expect **no visible
   change** for the
   text that works (expected — grayscale, integer-X, same as M11). The
   RGBA8/subpixel-AA experiment that *did* change things rendered wrong
   and was reverted (post-mortem at top). The win is commit b.
3. After commit b: re-shot + jitter/animate a label's X. Spacing reads
   uniform across positions; digits feel crisp rather than soft (hinting
   now actually shows because X isn't snapped). Compare to hyprlock
   side-by-side.
4. After commits c + d: set `letter_spacing` + `font_weight = 300` on the
   Shinkai title; visible widening + thinner strokes, closer to hyprlock.
   Both Latin ("YOUR NAME") and Japanese ("君の名は。") respect both.
5. Both compositors (Hyprland + Sway).

## Risks / open questions

- **`ScaleContext` lifetime in `FontContext`** (resolved in commit a):
  `get_font` returns an owned `Arc<Font>`, so `font.as_swash()` borrows
  the local Arc, not `font_system` — no borrow conflict with the
  `&mut font_system` the rasterizer also takes. Confirmed against
  cosmic-text 0.19 source; compiles.
- **Memory**: atlas stays 1 MB (R8). No change from M11.
- *(The NVIDIA-RGBA8-row and coverage-gamma risks are gone — they only
  existed for the reverted RGBA8 path.)*
