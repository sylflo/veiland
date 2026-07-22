# Plan: `text_scale` — a per-widget text-size dial

Status: DESIGN (2026-07-22). Not built. This spec is written BEFORE the code, at
the user's request, so the decisions are settled in writing first. Kept UNTRACKED
in `docs/plans/` per convention. Companion to the diagnosis in
`widget-roadmap.md` ("text sizes are nominal-px, so fonts don't scale the same").

## Why this exists (one paragraph)

Every text line in the Python widgets is sized by `set_absolute_size(px)` where
`px` is a fixed fraction of the box (`card_w * 0.058` for the now_playing title,
`pill_h * 0.44` for the avatar greeting, ...). That `px` is the font's NOMINAL
size (em/ascent box), not the height of the actual glyphs. How much real letter
sits inside that box is a per-font property (cap-height / x-height): Sans nearly
fills its em; a cursive/script face (Dancing Script) spends much of its height on
loops and slant, so at the same `px` it reads visibly smaller. The fractions were
tuned to Sans metrics, so a non-Sans font "does not scale the same."

`text_scale` is the PRAGMATIC fix: a single multiplier the user bumps in TOML to
compensate. It does NOT auto-normalize across fonts (that is the deferred
"measured-metric sizing" fix, out of scope here — see the roadmap). It is the
dial that makes the widgets usable with any font TODAY, and it also answers the
recurring "how do I make the now-playing text bigger" ask without resizing the
whole card.

## Scope — what it IS and IS NOT

- **IS:** a per-widget multiplier applied to the widget's CONTENT text px:
  now_playing's title / artist / elapsed-total times; avatar's greeting line and
  the initials-disc letter. Default `1.0` (pixel-identical to today).
- **IS NOT:** a change to layout GEOMETRY (card size, art square, progress bar,
  pill capsule, avatar diameter, paddings) — those stay derived from the region.
  Only the glyph size changes; the boxes around the text do not move or resize.
- **IS NOT** applied to CHROME text. Specifically the now_playing **pause badge**
  ("PAUSED") is left UNSCALED: its pill width is computed from the MEASURED text
  extent, so scaling the text without the pill would overflow the pill. The badge
  is decorative art-overlay chrome, not content; it scales with the card only.
- **IS NOT** `markup`. markup already honors `font_size` (its own fraction knob),
  so it needs no `text_scale` — a markup user sets `font_size` directly. Keep the
  knob on the two widgets that IGNORE `font_size` (now_playing, avatar).

## Config key

- Key name: **`text_scale`** (a float multiplier). Chosen over `text_size`
  (which the roadmap uses for the DIFFERENT idea of an absolute
  fraction-of-region for avatar) — `text_scale` is unambiguously "multiply
  today's size," which is exactly the mental model.
- Default: `1.0`.
- Bounds: clamp to **[0.25, 4.0]**. A typo (`text_scale = 40`) must not blow the
  layout apart or push text off-buffer; too-small must not vanish it. Out-of-range
  clamps to the nearest bound.
- Bad value (non-number): fall back to `1.0` + one stderr line — the
  untrusted-input rule, exactly as `font_from_config` / `parse_color` do.

## Parser home

A new helper in **`veiland_text.py`** (the text companion — the natural home,
next to `font_from_config`):

```python
def text_scale_from_config(cfg: Mapping[str, Any], tag: str = "veiland-text") -> float:
    """Read text_scale (a size multiplier) from a plugin-config dict. Default 1.0,
    clamped to [TEXT_SCALE_MIN, TEXT_SCALE_MAX]; a bad value logs one line and
    returns 1.0 (untrusted-input rule)."""
```

- Add `TEXT_SCALE_MIN, TEXT_SCALE_MAX = 0.25, 4.0` module constants.
- Export both the function and (optionally) the bounds via `__all__`.
- Both widgets call it once in `main()` and thread the float down, the same way
  `font` is threaded today.

Rationale for the companion vs per-widget: it is the SAME move `font_from_config`
made — one parser, one clamp, one log format, both widgets identical. Two callers
is the established extraction threshold here.

## Threading (how the float reaches the px)

Both widgets already thread a `font: FontSpec` through their draw functions. Add
a parallel `scale: float` param next to it (NOT bundled into FontSpec — scale
multiplies a px, it is not a font property; and the `vt.draw_ellipsized*` helpers
already take `px` and `spec` separately, so a bundle would be unpacked at every
callsite anyway with no saving).

- **now_playing:** `draw_into(buf, layout_name, track, font, scale)` ->
  `draw_star(..., font, scale)` / `draw_compact(..., font, scale)`. At each of the
  6 content-text callsites, multiply the px: `card_w * 0.058 * scale` (title),
  `* 0.046 * scale` (artist), `* 0.036 * scale` (times) in star; the `h * 0.18 /
  0.145 / 0.11` trio in compact. `draw_pause_badge` gets NO scale param (chrome,
  see Scope).
- **avatar:** thread `scale` into `draw_into` -> `draw_avatar_disc` (the initials
  letter px `d * 0.42 * scale`) and `draw_greeting_pill` (the greeting px
  `pill_h * 0.44 * scale`), plus the row-layout greeting px `pill_h * 0.38 *
  scale`. The pill capsule already MEASURES its text and wraps it, so bumping the
  text px makes the capsule grow to fit automatically — good. BUT: clamp/behavior
  note below.

## The overflow question (the one real risk — decide before coding)

Scaling text UP inside a fixed box can overflow. Two behaviors per widget:

- **now_playing title/artist:** these already `set_width(max_w)` +
  end-ellipsize, so a larger font just ellipsizes sooner. No overflow, no crash —
  it self-limits. The times are short (`M:SS`), unlikely to collide even at 4x.
  So now_playing is SAFE to scale freely; worst case is more ellipsis. Acceptable.
- **avatar greeting pill:** the pill grows to wrap the measured text. At a large
  `text_scale` the pill can exceed the region width. Today `draw_greeting_pill`
  passes `max_w = w - 4` to the layout, so the text itself ellipsizes at region
  width, but the pill height is driven by `pill_h` (region-derived), NOT the text
  px — so scaled text can exceed the pill's vertical bounds and clip.
  **Decision needed:** either (a) let it clip (document "large text_scale + stack
  layout may clip; use a taller region"), or (b) clamp the effective text px so
  text never exceeds the pill height (`min(px * scale, pill_h * CAP)`), trading
  "the dial stops working past a point" for "never clips." **Recommendation: (b)
  with a generous cap**, so the dial is safe by construction — a widget that
  clips on a config value violates the never-surprise rule more than a dial that
  saturates. Revisit if (b) feels too clever; (a) + a doc line is the fallback.

## Docs to update when built

- `docs/config.md` — add `text_scale` to the plugin-config notes (float, default
  1.0, range, "compensates for fonts that render small at a given size").
- `docs/examples/now_playing.toml` / `now_playing_star.toml` /
  `avatar.toml` — a commented `# text_scale = 1.25` line next to `font_family`,
  since the two are used together (bump scale when a font renders small).
- `docs/plugin-api.md` — `text_scale_from_config` in the veiland_text companion
  surface, next to `font_from_config`.
- `widget-roadmap.md` — mark option (a) as SHIPPED in the nominal-px section;
  the measured-metric fix (b) stays deferred.

## Verification plan (before committing)

1. `mypy --strict` + `ruff check` + `ruff format --check` + `py_compile` on the
   3 changed files (veiland_text, now_playing, avatar) — the standard bar.
2. Render probe (dev shell, PNG): now_playing star + compact at
   `text_scale = 1.0` (must be pixel-identical to today), `1.4`, and `2.0`, each
   with Sans and Dancing Script — eyeball that (a) 1.0 is unchanged, (b) the text
   grows and the card geometry does NOT, (c) the pause badge does NOT grow, (d)
   no clip past the region on avatar (whichever overflow behavior we pick).
3. Bad-value probe: `text_scale = "big"` and `text_scale = 99` -> the first logs
   one line + uses 1.0, the second clamps to 4.0; neither crashes.
4. Confirm `text_scale` absent -> byte-identical render to pre-change (the
   default-1.0 path must be a true no-op).

## Open decisions (settle in-flight)

- **Overflow behavior** (the section above): recommendation (b) saturating clamp;
  fallback (a) clip + document. DECIDE before writing avatar's pill code.
- Whether to also expose `text_scale` on the Rust `label`/`clock` plugins for
  parity. NO for now — they already take an absolute `font_size` fraction, so the
  multiplier is redundant there; this knob is specifically for the Python widgets
  that derive size from geometry. Note it, don't build it.
- Bounds [0.25, 4.0] are a guess; widen if a real use wants more. Cheap to change.
