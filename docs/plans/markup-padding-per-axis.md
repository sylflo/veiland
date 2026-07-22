# Plan: markup gains per-axis chip padding (bg_padding_x / bg_padding_y)

Status: DESIGN (2026-07-23). Not built. Another follow-up the avatar split
surfaced: once the greeting sat on a real chip, the immediate want was "more
horizontal room than vertical" -- which the single symmetric `bg_padding` cannot
express. Kept UNTRACKED in `docs/plans/` per convention.

## Why (the trigger)

markup's chip (shipped 89f01fb) is padded by ONE scalar on both axes:

    pad = bg_padding * px
    chip_w = block_w + 2*pad
    chip_h = block_h + 2*pad

So you cannot ask for a wider-than-tall pad. Two things you might reach for
instead do NOT work, and it is worth stating why so nobody chases them:

- **"Just widen the region box."** No: the chip is sized to the measured TEXT
  block, not the region. A wider region only leaves more empty space AROUND an
  unchanged chip (the chip stays centered by the content anchor). The chip hugs
  the text on purpose -- that is what makes it a pill, not a bar.
- **"Raise bg_padding."** That grows BOTH axes together, so you get a taller chip
  too. Fine if you want uniform breathing room; useless for horizontal-only.

Measured on the avatar greeting (2026-07-23): with `bg_padding = 0.5` the
chip-to-ink gaps came out ~14/15/17/14 px (L/R/T/B) -- geometrically even, and it
reads even for text with ascenders+descenders. The symmetric default is correct;
this plan is about the OVERRIDE, for when a design wants an asymmetric pill.

## Decision (settled 2026-07-23)

Add `bg_padding_x` and `bg_padding_y` that OVERRIDE the `bg_padding` shorthand per
axis -- mirroring the region system's `margin` / `margin_x` / `margin_y` exactly
(see the anchor-margin single-axis work, where per-axis margins already override a
shorthand). This keeps ONE mental model across the whole config: a shorthand that
sets both, plus optional per-axis overrides.

- `bg_padding`   -> both axes (default 0.5, unchanged).
- `bg_padding_x` -> horizontal only; absent -> falls back to `bg_padding`.
- `bg_padding_y` -> vertical only;   absent -> falls back to `bg_padding`.

Each is a fraction of the font px (same unit as `bg_padding` today), read with the
existing `resolve_float` helper (non-negative, bad value -> default + one stderr
line, the untrusted-input rule). ABSENT bg_padding_x AND bg_padding_y -> identical
to today's symmetric render (the load-bearing invariant every markup knob has
held: absent == byte-identical).

## Sketch

In `main()`, resolve two pads instead of one:

    base = resolve_float(cfg, "bg_padding", DEFAULT_BG_PADDING)
    pad_x = resolve_float(cfg, "bg_padding_x", base)   # default is the shorthand
    pad_y = resolve_float(cfg, "bg_padding_y", base)

Carry `bg_pad_x` / `bg_pad_y` on `Style` (replacing the single `bg_padding`
field), and in `draw_into`:

    px_pad_x = style.bg_pad_x * px
    px_pad_y = style.bg_pad_y * px
    chip_w = block_w + 2*px_pad_x
    chip_h = block_h + 2*px_pad_y
    rounded_rect(cr, x - px_pad_x, y - px_pad_y, chip_w, chip_h,
                 style.bg_radius * chip_h)

Note `bg_radius` stays a fraction of chip HEIGHT, so an asymmetric pad still gives
a sensible capsule (radius tracks the vertical extent, which is what the eye reads
as the pill roundness). Confirm by render that a wide pad_x + small pad_y still
looks like a lozenge, not a slab -- adjust the radius basis only if it looks wrong.

## Docs / examples

- `docs/examples/markup.toml`: extend the optional-defaults block -- document
  `bg_padding` as the shorthand and `bg_padding_x` / `bg_padding_y` as the
  per-axis overrides, same wording shape as the region `margin_x`/`margin_y`
  comments.
- `docs/examples/avatar.toml`: OPTIONAL -- if the greeting wants a wider chip,
  drop in `bg_padding_x`. Not required; the symmetric default already looks right.

## Verify plan (when built)

1. `bg_padding_x`/`bg_padding_y` both absent -> byte-identical to current markup
   (the identity invariant).
2. `bg_padding_x` only set -> horizontal gap grows, vertical unchanged (measure
   the four chip-to-ink gaps as done 2026-07-23; L/R grow, T/B hold).
3. `bg_padding` set + one axis overridden -> the overridden axis uses its own
   value, the other inherits the shorthand.
4. A wide pad_x + small pad_y still reads as a rounded pill (radius basis check).

## Relation to the other unplanned bits

- Independent of `markup-icon.md` (the leading-glyph plan) -- they touch different
  parts of the chip math and can land in either order. If both land, an icon +
  asymmetric pad should still form one block on one chip; nothing about per-axis
  pad changes the icon layout.
- This is deliberately AFTER the avatar split (0b9f0bc): the greeting shipped with
  symmetric padding, and this is a pure additive override on top.
