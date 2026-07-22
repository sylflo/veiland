# Plan: markup gains an optional leading icon (SVG + text on a chip)

Status: DESIGN (2026-07-23). Not built. A FOLLOW-UP that fell out of the avatar
split: once markup could draw a background chip (shipped 89f01fb), the obvious
next want is markup drawing an ICON beside the text on that chip -- which is
exactly what the old avatar greeting pill did (a `user.svg` head-and-shoulders
glyph left of "Good evening, Sylvain"). Kept UNTRACKED in `docs/plans/` per
convention.

## Why (the trigger)

The avatar split moved the greeting out of avatar and into a markup region. The
old greeting pill had THREE parts: a glass capsule (now markup's `bg_color`), the
text (markup's job all along), and a small leading `user.svg` glyph. The split
delivered the first two; the glyph has no home yet, so `python/examples/icons/
user.svg` is currently an ORPHAN asset (nothing references it in code -- only the
roadmap doc mentions it). This plan gives it a home and, in doing so, makes markup
a richer general widget: any icon + any text on a chip, not a greeting-specific
thing.

It also lines markup up as the STATIC sibling of the status pills. wifi.py /
bluetooth.py / ethernet.py / battery_svg.py all draw "one SVG (chosen from a set)
centered in a pill." Their drawing half is generic and duplicated; what differs is
only the DATA SOURCE (which D-Bus property, how it buckets to a filename -- wifi
buckets a 0..100 strength into 5 bars, bluetooth maps a 3-way enum to 3 glyphs;
"we don't check the same thing"). markup+icon is the config-picks-the-icon version
of the same draw; the status pills are the source-picks-the-icon version.

## Decision (to settle when built)

- **CHOSEN shape: a config key, not inline Pango.** Add `icon = "path/to.svg"` to
  markup's `[plugin.config]`. markup loads it via `veiland_svg.load_svg` (the same
  companion the status pills use), measures it as a fixed leading block beside the
  text, and draws icon + text as ONE unit inside the chip. Rejected: inline-image
  markup via Pango custom shape-rendering -- fiddly, heavy, no real Pango support.

- **One anchored block.** The icon + gap + text form a single measured block; the
  content anchor (`veiland_layout.anchor_offset`) places THAT block, and the chip
  (`bg_color`) wraps it -- so icon, text and chip all move together, exactly as the
  old pill did. The icon does not get its own separate anchor.

- **Fully general, not avatar-specific.** `icon` takes any SVG path; it is a markup
  feature. `user.svg` is just the natural default the avatar scene wires in. Do not
  bake a "user" concept into markup.

- **Optional + absent-is-identity.** No `icon` key -> byte-identical to today's
  markup (the same load-bearing invariant the `bg_color` step held). A bad path
  logs one stderr line and draws text-only (untrusted-input rule), never crashes.

## Sketch (settle exact keys + sizing by render probe)

- `icon = "~/x.svg"` (absent -> no icon). Path is `os.path.expanduser`'d.
- `icon_color = [r,g,b,a]` -- optional tint (reuse `vs.parse_color(..., None)`),
  matching the status pills' `icon_color`. Absent -> the SVG as authored.
- `icon_gap` -- gap between icon and text, a fraction of the font px (like the old
  pill's `gap = pill_h * 0.26`), read with the `resolve_float` helper markup
  already has. Icon SIZE derives from the text block height (roughly the cap/line
  height), so it scales with `font_size` and needs no separate key -- confirm by
  render.
- Draw order in `draw_into`: CLEAR -> chip (`bg_color`, now sized to the icon+text
  block) -> icon (`vs.draw_svg_centered`, tinted) -> shadow+text -> debug border.
  The chip already sizes to the measured block + padding; the block just grows by
  the icon width + gap. Multi-line text: the icon centers on the text block's
  vertical extent (the pill centered the glyph on the pill mid-line).

## Reclaiming user.svg

Once markup has `icon`, the avatar scene (`docs/examples/avatar.toml`, rewritten in
the avatar split) sets the greeting region's `icon = "<repo>/python/examples/icons/
user.svg"` so the display-manager greeting keeps its little person glyph -- from
the generic widget, no bespoke code. Until then user.svg stays in the tree as the
orphan-with-a-purpose (do NOT delete it in the avatar split; this plan consumes
it).

## Dependencies / ordering

- Needs the `bg_color` chip step (DONE, 89f01fb) -- the icon sits on that chip.
- Independent of the avatar-split code shrink, but the avatar SCENE wants it (the
  greeting glyph). Reasonable orderings: (a) finish the avatar split with a
  glyph-less greeting, ship this after, then add `icon = user.svg` to the scene; or
  (b) do this first so the very first avatar.toml has the glyph. Either is fine;
  the split does not NEED the glyph to be correct, only to match the old look
  pixel-for-pixel.
- markup adds `veiland_svg` to its import set (it already imports `vs` for
  `parse_color`, so `load_svg`/`draw_svg_centered` are no new dependency).

## Verify plan (when built)

1. `icon` absent -> render byte-identical to current markup (the identity check,
   as with `bg_color`).
2. `icon = user.svg` + `bg_color = glass` -> render matches the OLD avatar greeting
   pill closely (glyph left of text, both on the capsule, one unit).
3. The icon+text block honors the content anchor: render the 9 positions, confirm
   glyph+text+chip move together as one block.
4. Bad `icon` path -> text-only render + one stderr line, no crash.

## Future (separate plan, NOT this one)

The status pills (wifi/bluetooth/ethernet/battery_svg) share the identical
"pill + centered SVG + anchor + border + pacer + load_icons" boilerplate; only the
D-Bus source and `pick_icon` differ. Extracting that shared drawing/loop into a
helper -- each widget supplying just its source + state->filename mapping -- is a
real dedup, but it is a bigger refactor touching four SHIPPED widgets and earns its
own plan. Noted here only so the duplication is on record; do not fold it into the
markup-icon work.
