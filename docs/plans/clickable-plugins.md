# Plan: clickable plugins — forward pointer clicks to a plugin's own region

Status: DESIGN (2026-07-23). Not built. The largest single capability on the
roadmap: it introduces the FIRST input path from the core to a plugin, unlocking
interactivity (starting with now-playing transport controls) for the whole plugin
ecosystem. Kept UNTRACKED in `docs/plans/` per convention.

## Why this is a class apart (read before treating it as "a widget feature")

It is NOT a widget feature -- it is a new PROTOCOL CAPABILITY plus a new
TRUST-BOUNDARY decision. Today there is deliberately no click/pointer message:

- Threat model (CLAUDE.md): plugins receive *"only the events the core chooses to
  forward (configuration, time ticks, **optionally clicks within their own
  region**)"* -- "optionally clicks" is written as a FUTURE capability, not a
  current one.
- widget-roadmap.md: *"Read-only in v1. No play/pause buttons... blocked on a
  pointer/click protocol message that does not exist (its own future design
  round)."*

So this is the round that design. It is high-value (interactivity for EVERY plugin,
not just now-playing) and high-care (the core forwards pointer events into
untrusted processes, and a plugin acting on a click reaches back out to the system,
e.g. MPRIS pause). Do it AFTER the read-only widgets and the auth round are solid.

## What must NOT change (the invariants that survive)

Forwarding CLICKS must not weaken any of the by-construction guarantees:

- **No keyboard, ever.** This adds POINTER events only. No protocol message carries
  keystrokes in any direction -- that stays absent by construction. A click message
  is not a keyboard message and must never become a channel for one.
- **No unlock via a plugin.** A click reaches a plugin; a plugin still has NO way to
  map anything to the unlock decision. The unlock path stays
  `keyboard -> password buffer -> PAM -> unlock`, untouched. A plugin receiving a
  click cannot escalate it to an unlock; the API surface is absent.
- **Region confinement.** A plugin only ever hears about clicks INSIDE ITS OWN
  region -- the core translates a surface-space pointer event to region-local
  coordinates and forwards it ONLY to the plugin whose region (and z-order) owns
  that point. A plugin never learns about clicks elsewhere, never sees the pointer
  when it is over another plugin or the password UI. Same confinement the buffer
  boundary already has.
- **Password UI is off-limits.** The core composites the password indicator LAST,
  on top. Clicks that land on the password UI's area go to the CORE, never
  forwarded to any plugin (a plugin must not be able to intercept interaction with
  the auth surface -- the paint-order guarantee extends to input-order).

## Protocol shape (settle when built)

- A new `ServerMessage` (host -> plugin), e.g. `PointerClick { x, y, button }` with
  region-LOCAL coordinates. Backwards-compatible addition (tagged enums extend
  cleanly -- CLAUDE.md). Only forwarded to plugins that OPT IN (see below); silent
  plugins never receive it, so existing read-only widgets are unaffected.
- Opt-in: a plugin declares it wants clicks. Options: a flag on `Hello`, or a
  config/region property the CORE reads (`clickable = true` on the region). The core
  reading it (region is core-owned) is cleaner than trusting a Hello flag, and keeps
  "which regions are interactive" a core decision. Decide when built.
- Scope v1 to CLICK (press+release in-region), not full pointer motion/hover/drag.
  Motion streaming is more surface (and more per-frame cost); a single click covers
  transport buttons. Add motion later only if a real widget needs it.
- The plugin ACTS on the click itself (now-playing calls MPRIS Play/Pause over its
  own D-Bus connection). The core does NOT interpret the click's meaning -- it only
  delivers "a click happened here." What the plugin DOES is the plugin's business
  (and runs as the user, so pausing a media player is no boundary issue -- same as
  the plugin already reading MPRIS state today).

## First consumer: now-playing transport controls

Once clicks arrive, now-playing gains play/pause/next/prev:

- Draw the transport buttons (it already draws a pause badge -- extend to a control
  row). Hit-test the click against the button rects; call the matching MPRIS method
  (`PlayPause` / `Next` / `Previous`) on the active player over its existing D-Bus
  connection.
- Purely additive to the READ-ONLY now-playing: no click -> it still just displays.
  A player that does not support a control -> the call no-ops / logs, never crashes.
- This is the widget that motivates the whole round (the recurring "add controls to
  the playing widget" ask), but the capability is general -- any plugin can now be
  interactive (a clickable weather widget that toggles units, a power menu, etc.).

## Security review is mandatory

This touches input routing (a CLAUDE.md extra-scrutiny path) and adds the first
core->plugin input channel. `/security-review` the diff. Walk through: can a click
message ever carry more than a click? can a plugin infer anything about clicks
outside its region? can the password UI's input be intercepted? can any of this
reach the unlock decision? The answers must all be "no, by construction," not "no,
by filter."

## Ordering

- AFTER the read-only widget set (v0.2.0) and the auth round (v0.3.0): interactivity
  is worthless if the widgets and auth are not solid first, and this is the highest-
  care non-auth change.
- Independent of the visual backlog (hero preset, polish) -- it is a protocol/core
  round, its own release theme. A natural "veiland widgets are now INTERACTIVE"
  headline + Reddit post, since no competitor's isolated-plugin locker can do this
  (hyprlock's onclick runs in-process; veiland forwards a confined click to an
  isolated process that still cannot touch the password).
- Its own design round before code -- the protocol message shape + the opt-in
  mechanism + the password-UI input-order guarantee are the decisions to nail first.
