# Plan: give now_playing "star" a condensed (card-sized) region

Status: DESIGN (2026-07-22). Not built. A FOLLOW-UP, postponed out of the
content-anchor branch (it is a shipped-widget sizing redesign, not an anchor
migration). Kept UNTRACKED in `docs/plans/` per convention.

## Why (the trigger)

Adding the debug border to the star layout made its region visible for the first
time: the star's region is the WHOLE lock surface, and the card floats centered in
it painting ~95% of the buffer transparent. That prompted the question "why isn't
star just a card-sized rectangle?" -- and on inspection, the two reasons the
full-surface choice was justified by DON'T HOLD:

1. "The buffer must cover the screen so the wallpaper shows behind/around the
   card." WRONG. Layering is `z_index` + the card's transparent background:
   wallpaper at a lower z_index shows through wherever now_playing is transparent,
   and shows OUTSIDE a tight region because that area simply belongs to the
   wallpaper plugin, not to now_playing. Region size is irrelevant to this.
2. "A tight region can't be centered on screen without the plugin knowing the
   screen size." WRONG. The anchored region form already centers the BOX on the
   output host-side (`region = { halign = "center", valign = "center", ... }`).
   The plugin needs no screen size to be centered.

The only REAL difference is card SIZING: today the card scales off the full
surface (`card_w = min(w*0.30, h*0.42, 420)` with w,h = screen). In a tight region
it would scale off the REGION. And that is MORE intuitive, not less: "the region
IS the card's footprint" is the same model every other widget uses, instead of
star's special "grab the whole screen and carve out 30%."

So full-surface was the PRE-ANCHOR workaround (before the region form could
center a card-sized box, the only way to center a card was to own the screen and
do `(w-card_w)/2`). It is no longer necessary.

## Decision (agreed 2026-07-22)

Convert star to a condensed, card-sized region. Do it as a SEPARATE follow-up
after the content-anchor branch, because it changes a shipped widget's sizing
basis and needs its own render + verification. On the content-anchor branch, star
keeps ONLY the debug border (committed f6b2e09) and is deliberately NOT given a
content anchor (a condensed region's card == region, so content_* is a no-op there
and the region's own halign/valign does the placement -- anchoring draw_star would
be dead code).

## What changes

- **`draw_star` sizes the card off its (w, h) = region**, not the full screen. The
  card can still be smaller than the region (leaving glass/wallpaper margin around
  it) OR fill it -- decide how much "floating in space" slack to keep. Likely keep
  a modest inset so it still reads as a floating card, not a full-bleed panel.
- **`now_playing_star.toml` gains a real region** instead of full-surface:
  `region = { halign = "center", valign = "center", width = ~0.35, height = ~0.55 }`
  (tune by render). The host centers it; the card sizes to it.
- Placement is then the REGION's halign/valign (host-enforced), which is the
  correct home for "where the box goes" -- consistent with every other widget.

## Interactions to re-check when building

- **Password indicator.** now_playing_star.toml parks the password box at
  `y_percent = 92` "clear of the centered card." A resized/repositioned card must
  still clear it -- re-verify, and adjust the region or y_percent together.
- **Card sizing across resolutions.** The card now scales off region fractions;
  confirm it stays a sensible portrait block on 1080p AND 4K (the region fractions
  are resolution-independent, so it should -- verify by render at both).
- **The "floating card" feel.** With a tight region the card no longer sits in a
  sea of wallpaper unless the region is larger than the card. Decide the region-
  to-card ratio by eye; too tight and it stops looking like the star centerpiece.
- **Behavior change for existing star users.** Anyone on now_playing_star.toml
  today gets a differently-sized card. now_playing is unreleased, so acceptable;
  note it in the roadmap.

## Verify plan (when built)

1. Render star at the new region on 1080p and 4K: card is a sensible portrait
   block, centered, wallpaper visible around it, reads as the same "star" look.
2. Confirm the password indicator still clears the card.
3. Confirm buffer is now region-sized (not screen-sized) -- the efficiency win.
4. Compact layout unaffected (its region was already the card's box).
