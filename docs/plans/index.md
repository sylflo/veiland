# docs/plans/ — index & order of work

Scratch design docs for veiland work that is planned but not yet fully shipped.
UNTRACKED by convention (never `git add` this directory). A plan doc is DELETED
once everything it describes is in the code -- so if it's listed here, it still
has open work.

Current release: **v0.1.0** (tag on 89b18f2, 2026-07-14). Next: **v0.2.0**.

---

## When to cut a release (the standing rule)

The problem this rule solves: "I'll never make a release" -- perfectionism batches
everything into a mythical big version that never ships. The fix is a trigger, not
a finish line.

> **Cut a release when a coherent, user-visible theme completes** -- one or two
> features a changelog line can NAME -- and the tree is green (`nix flake check` +
> `nix build`). Don't batch unrelated work; don't wait for the "big" thing. A
> release is "here's a nameable improvement," not "here's everything."

Corollaries:
- A feature too small to name in a changelog line (a single config knob, a doc
  fixup) does NOT get its own release -- it hitches a ride on the next themed one or
  waits.
- Pre-1.0: a themed batch of new features is a MINOR bump (0.1 -> 0.2); a
  fix-only batch is a PATCH (0.2.0 -> 0.2.1).
- Version bumps happen AT release time, not during feature work (see
  [[feedback_no_version_bumps]]).

---

## → NEXT RELEASE: v0.2.0 — "the polished lockscreen" (weather + blur + hero preset)

The theme AND the Reddit story are ONE thing: close the last widget gap (weather),
ship the most-loved polish (blur), and COMPOSE them into a gorgeous default scene —
then lead a single Reddit post with that scene. This is the deliberate answer to the
Vaila loss (we lost on the AT-REST PICTURE, not on capability — widget-roadmap.md).
Don't split weather/blur from the hero preset: they're one visual story, so they're
one release and ONE strong post (a feature-list post about "we added weather" is
weak; the composed screenshot is the pitch).

**In scope (do these, in order):**

1. **weather widget** — the last "universal widget" gap; asked for on BOTH Reddit
   posts. Python tier, Open-Meteo (keyless), temp + condition icon + a location
   OVERRIDE (don't force doxxing). Slot the deferred `veiland_image.py` companion
   here if a third image-decoder caller makes it pay off. See widget-roadmap.md #3.
2. **blur — TIER 1 ONLY** — the wallpaper blurs its OWN pixels (a veiland-shader
   preset or a wallpaper `blur` knob). ~90% of demand, no core change, respects
   isolation. Do NOT attempt Tier 2 (core-composited backdrop) for this release.
   See blur.md.
3. **hero preset — the STATIC "Deep Field" scene** — the FINISHER and the Reddit
   thumbnail. The chosen hero is a specific composed astronomy scene (see
   hero-deepfield.md): a deep-field nebula backdrop + starfield + moon + clock +
   password. The STATIC tier fits v0.2.0 because it needs NO shader plugin — render
   the nebula ONCE to an image, use it as a wallpaper background + blur, compose
   stars/moon/clock on top. The LIVE/animated version is a separate arc (below).
   Recording recipe: [[project_recording_gallery_gifs]].

**🚩 CUT v0.2.0 HERE** once 1+2+3 land and the tree is green. The static hero (3) is
the finisher; if it drags, ship weather+blur and make it a fast-follow — the 🚩 must
never be held hostage by polish. ONE Reddit post, leading with the static deep-field
scene.

**Optional (include ONLY if the above lands with time to spare; must NOT delay):**

4. **bg_padding_x / bg_padding_y** — per-axis chip padding. A minor knob, too small
   to be its own release; rides v0.2.0 if convenient, else slips later. See
   markup-padding-per-axis.md.
5. Housekeeping: the widget-roadmap.md disc-only-avatar note (pure doc fixup).

**Explicitly NOT in v0.2.0** (scope guard): markup-icon (greeting glyph), Tier 2
core blur, text_scale, now-playing condensed region, dashboard embed. All later.

---

## → v0.3.0 — "input & auth" (the whole lock EXPERIENCE)

The theme: make the LOCKING EXPERIENCE richer — both the password box people touch
every unlock AND the auth methods. This is a real, oft-noticed gap: veiland's input
is currently the weakest area vs. hyprlock (which has caps/fail/check states,
editing keybinds, grace period), precisely because it's all CORE work that was easy
to defer while chasing plugins. From a user's view "the password box feels nice" and
"fingerprint works" are ONE story: the lock feels polished.

All of it touches the TRUSTED CORE and the input/auth path (the highest-care code),
so it's grouped to open that code ONCE, and kept SEPARATE from v0.2.0's plugin/visual
work so the careful security round never delays — or gets rushed by — the
crowd-pleasing work. A QUIET release for Reddit (the password box doesn't
photograph); mention it in the next visual post's changelog, don't lead with it.

**Input experience (the half we're most lacking — give it equal weight):**

- **input-field feedback** — fail text / attempt count / waiting-while-PAM-verifies
  state / caps accent on the core-owned password indicator (drawn last, on top).
- **familiar editing keys** — ESC / Ctrl+U clear, Ctrl+A select-all, hold-backspace
  to repeat (all raised on Vaila; all core input handling).
- **grace period** — an optional no-auth window right after lock (hyprlock's
  `--grace`); small, high-appeal.

**Auth methods:**

- **fingerprint auth (fprintd)** — a PARALLEL unlock path (password field stays live
  during a scan, like hyprlock). Core-side Rust D-Bus to fprintd, NOT a plugin — the
  unlock decision stays in the trusted core. The item that dominates release size
  (fprintd + calloop async integration).
- **caps-lock / keyboard-layout forwarding** — the one legitimate "core forwards a
  signal" case (universal + core-vouchable, like time); unblocks the status
  cluster's caps/layout badges. Caps is already tracked, so cheap.

**Release shape:** if fingerprint alone is a big careful round, split —
**v0.3.0 = fingerprint**, **v0.4.0 = caps/layout + input-field**. Scope fingerprint
first; if it balloons, ship it solo. Either way security-reviewed
(`/security-review`). See auth-polish.md.

---

## → THE ANIMATED HERO ARC (veiland-shader → live Deep Field) — a multi-release track

The user chose a specific hero (hero-deepfield.md) and to reproduce it FAITHFULLY.
Its STATIC tier ships in v0.2.0 (above); its ANIMATED tier is a multi-release arc
that PULLS veiland-shader forward from "someday big track" to a named priority,
because 3 of the scene's layers (nebula, aurora, moon) are GPU shaders. Runs
alongside/after the auth round; each stage is its own release theme + Reddit post:

1. **veiland-shader host + nebula preset** (veiland-shader.md) — the generic GLSL
   plugin, proven on SHADER_NEBULA (ports ~verbatim from the mockup). The static
   hero's backdrop becomes LIVE. This is veiland-shader's reason to exist now.
2. **Animated hero v1** — live nebula + starfield + meteor emitter + clock. The
   churning backdrop is ~80% of the "wow"; ship BEFORE the hard shaders. A strong
   "live, GPU, process-isolated — no other locker can do this" post.
3. **Hard shaders** — aurora (net-new authorship; the mockup lists it but has no
   shader) then the raymarched moon (hardest; ray-sphere + lighting + craters).
   Polish on a hero that already reads; don't block the animated post on them.
4. **Ephemeris data** — last; computed moon-phase/LST (cheap) vs. real sky data
   (its own project — lean cheap). The scene reads as astronomy from visuals alone.

See hero-deepfield.md for the full layer table + effort tags. The mockup at
`mockups/veiland-lockers/` (Deep Field tab) is the spec.

## BACKLOG — after the auth round (re-rank on each ship; don't pre-number)

Beyond ~2 releases out it's a backlog, not a roadmap. When v0.4.0 ships, pick the
next theme from here based on what users ask for and what you feel like building.
Small "polish" items CLUSTER — bundle 2–3 into one release; big items are each their
own theme.

**Small polish (bundle 2–3 into a "widget polish" release):**

- **markup-icon.md** — optional leading `icon = "x.svg"` on markup (SVG + text on
  one chip). Reclaims the orphaned `icons/user.svg` so the avatar greeting gets its
  person glyph back. Small; natural pairing with any "widget polish" release.
- **now-playing-star-condensed-region.md** — make now_playing "star" a card-sized
  region instead of a full-surface one painting ~95% transparent. Shipped-widget
  cleanup.
- **text-scale.md** — a per-widget `text_scale` dial so fonts scale consistently
  (design settled: [0.25, 4.0] bounds, parser in veiland_text.py). Answers the
  recurring "make the text bigger" ask.
- **blur.md TIER 2** — the CORE-composited backdrop-blur region (frosted-glass
  behind a widget). Bigger, core work; the one that out-does hyprlock. Its own
  release theme when you want it.

- **clickable-plugins.md — the biggest capability, do it LAST of the core rounds.**
  The FIRST input path from core to plugin: forward a confined, region-local CLICK
  to a plugin that opts in — which unlocks now-playing transport controls
  (play/pause/next/prev) and interactivity for EVERY plugin. It's a new protocol
  message + a new trust-boundary decision (pointer only, never keyboard; never
  reaches the unlock decision; password UI input off-limits), so it comes AFTER the
  read-only widgets (v0.2.0) and auth round (v0.3.0) are solid, and gets its own
  `/security-review`. A strong "veiland widgets are now INTERACTIVE" headline no
  isolated-plugin locker can match.

## Bigger tracks (multi-PR, span several releases)

- **veiland-shader.md** — generic GLSL host plugin (Shadertoy-preamble frag shader
  from a file/preset, self-paced fullscreen quad). The GPU tier; also the home for
  blur Tier 1. Four PRs, not started.

## Living / history docs (NOT a to-do list; don't delete on ship)

- **widget-roadmap.md** — the informational-widget roadmap + Reddit rationale + all
  the per-widget design notes. The source of truth for widget decisions. NOTE: a
  few lines are stale post-avatar-split (says avatar "keeps its greeting" / "pending
  verification") — fix in the v0.2.0 housekeeping pass.
- **python-sdk.md** — the Python plugin track's SDK/examples history + forward spec.
  PRs A/B landed; PR C is the active edge. Running record, not a pure to-do.

---

## Parallel non-widget track (fold into whichever release they're ready for)

Real Reddit demand, not tied to a specific release theme. Already DONE: per-monitor
wallpaper (the wallpaper plugin + region `monitor`/anchor selection already do
this — do NOT list it as a gap). Still open: TOML includes (pywal colors),
curated wallpapers + presets. Tracked in widget-roadmap.md's parallel-track section.

NOTE: the password-box UX items (caps indicator, fail feedback, familiar editing
keys, grace period) are NOT loose parallel-track items — they are the INPUT half of
the v0.3.0 "input & auth" round below. Richer input is a real, oft-noticed gap
(every unlock touches the password box); it lives with fingerprint because it is the
same core/input/auth code, opened once.
