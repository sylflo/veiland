# Plan: background blur (two tiers) + the trust-boundary reasoning

Status: DESIGN (2026-07-23). Not built. A hallway idea from comparing against
hyprlock (which has built-in background blur; veiland has none). Kept UNTRACKED in
`docs/plans/` per convention.

## The two things people mean by "blur"

Almost every blur request is one of these, and they have DIFFERENT right answers:

1. **"Blur my wallpaper."** A frosted, out-of-focus background so widgets pop.
   ~90% of real demand. This is exactly what hyprlock's `blur_passes` does, and it
   is scoped to the BACKGROUND only.
2. **"Frosted glass behind a widget."** The iOS/macOS control-center look: a panel
   blurs whatever is under IT specifically. The fancy one. This is what "blur on
   top of anything via z-index" was reaching for.

## The hard constraint that shapes everything

A blur shader needs to SAMPLE the pixels it is blurring as an input texture. In
veiland, **a plugin cannot see what is behind it** -- and this is load-bearing, not
an oversight:

- Each plugin renders into its OWN dmabuf in its OWN process. It never receives
  another plugin's buffer.
- The core composites plugins in z-order but does NOT hand any plugin a readback of
  "the frame so far."
- Threat model (CLAUDE.md): *"Read another plugin's buffer -- each plugin owns its
  dmabufs; the core composites but doesn't redistribute."*

So `z_index` orders COMPOSITING; it does NOT grant an upper layer read access to a
lower one. A blur plugin at z=5 physically has no texture of the z=0 wallpaper to
blur. This is precisely the isolation that makes veiland safer than hyprlock (which
is one process, so its background widget trivially has the pixels in hand). The
safety property is exactly what blocks the naive "blur layer on top."

## Three paths, and the verdict on each

### Tier 1 -- plugin blurs its OWN pixels (DO THIS FIRST)

The wallpaper/background plugin blurs its own output before handing the buffer over.
No new mechanism: the plugin already owns its pixels. This covers want #1 (blurred
wallpaper) completely -- and note hyprlock's blur is background-scoped anyway, so
this matches the real feature, not a subset.

- Home: a **blur preset in `veiland-shader`** (point it at an image, it renders a
  blurred version) OR a `blur` knob on a wallpaper plugin. Kawase/Gaussian frag
  shader, the standard technique.
- Cost: small, no core change, no boundary change. Fits the shader track naturally.
- **If we only ever ship one blur, this is it.**

### Tier 2 -- core-composited backdrop-blur REGION (the right answer for want #2)

Reframe the frosted-glass effect: it is NOT a plugin feature, it is a COMPOSITING
feature -- "blur the accumulated frame at this z-layer." Compositing is already the
core's exclusive job (z-order, alpha blend, the password-UI-drawn-last rule). So:

- A plugin DECLARES a backdrop-blur region in its config (think hyprlock's `shape`
  with an `xray` hole, done the veiland way).
- The **core** -- not the plugin -- blurs THAT rectangle of the composited scene,
  then draws the plugin on top.
- The plugin never SEES the blurred pixels; the core just blurs a hole for it.

This is the one that would actually out-do hyprlock (whose blur is background-only).
More work (core GL compositing path, careful), for LATER.

### Tier 3 -- hand plugins a backdrop texture (DO NOT)

A protocol mechanism that gives a plugin a read-only texture of what is behind it.
Rejected: it hands one plugin a readback of the composited scene, including OTHER
plugins' output -- a direct violation of "a plugin can't read another plugin's
buffer," one of the four by-construction guarantees. Trading a load-bearing
security property for a cosmetic effect is a bad deal. Killed.

## Does Tier 2 break a promise? (the reasoning to keep on record)

Two promises to test, because "the core does it" is not automatically safe.

### The security promise: NOT broken

The guarantee is *"the core composites but doesn't REDISTRIBUTE."* The key word is
redistribute -- the promise is NOT "the core never touches plugin pixels" (it
touches them constantly; that IS compositing). The promise is that the core never
hands one plugin ANOTHER plugin's pixels.

- Tier 2: the core blurs a rect of its OWN composited framebuffer and draws the
  requesting plugin on top. The plugin never receives, samples, or sees the blurred
  pixels. No plugin gets pixels it did not render. **Guarantee holds.**
- Tier 3: the core hands the plugin the backdrop texture -> redistribution ->
  broken. That single line -- "does any plugin receive pixels it didn't render" --
  is the whole difference between Tier 2 (safe) and Tier 3 (out).

### The architectural promise ("UI is plugins, core stays minimal"): NOT broken, but a judgment call

The ethos (CLAUDE.md) is "if it's UI, it's a plugin." A blur feels UI-ish, so is
core-blur core-creep? Resolution: **blur-of-the-composited-scene is compositing, not
UI**, and compositing is already the core's job. The core owns HOW buffers combine
(z-order, alpha blend, password-last); "blur the accumulated frame in this rect
before continuing" is the same KIND of operation -- a blend, not a widget. The
plugin still owns WHAT goes in the region (it draws its panel); the core owns how
the layers combine.

The tell that it belongs in the core: **only the core is even CAPABLE of this**,
because only the core holds "the frame so far." A feature that REQUIRES the trusted
frame buffer cannot be a plugin -- so putting it in the core is not creep, it is the
only place it can live (same logic as the password compositing-last rule).

### The discipline that keeps it clean (build to this)

The core does the **blur (a blend op) and NOTHING else** -- no rounded corners, no
borders, no content, no shadows, no "while we're here." The plugin draws all of that
into its own buffer, on top. The core's role is strictly "blur this rect of the
backdrop." Keep it that narrow and both promises stay intact; widen it and it
becomes boundary creep. A safe blend must not grow into the core drawing widgets.

## Ordering / relation to other plans

- Tier 1 depends on the `veiland-shader` track (docs/plans/veiland-shader.md) or a
  wallpaper-plugin blur knob. Do it there.
- Tier 2 is a CORE feature, independent of the Python/plugin tracks, and larger. No
  dependency on Tier 1; they coexist (blurred wallpaper AND a frosted panel).
- Not urgent -- veiland's differentiators are the plugin isolation + animated
  backgrounds, which hyprlock cannot do at all; blur is a parity nicety, not a moat.
