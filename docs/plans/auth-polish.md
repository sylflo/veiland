# Plan: input & auth — the whole lock experience (v0.3.0)

Status: DESIGN (2026-07-23, widened from "auth polish"). Not built. The v0.3.0
theme: make the LOCKING EXPERIENCE richer -- both the password box a user touches
EVERY unlock and the auth methods. Kept UNTRACKED in `docs/plans/` per convention.

## Why input leads here (the most-lacking area)

veiland's INPUT is currently its weakest area vs. hyprlock. hyprlock has caps/fail/
check states, editing keybinds (ESC/Ctrl+U clear), a grace period, placeholder/fail
text; veiland has a dot indicator. This gap is easy to miss from the inside (we've
been chasing plugins) but a user hits it EVERY unlock -- richer input is arguably
more impactful than a fourth widget, because everyone touches the password box and
weather is optional. So input gets EQUAL billing with fingerprint here, listed
first.

## Why these are grouped (open the input/auth path ONCE)

Everything below touches the TRUSTED CORE and the input/auth path -- the most
security-sensitive code in the project (CLAUDE.md: *"Anything touching the password
buffer, PAM, input routing, or the unlock decision: walk through it carefully,
prefer obvious-correct code over clever code"*). None of it is plugin work; a plugin
cannot see keystrokes, layout, caps state, or the unlock decision by construction,
and must not start to.

Grouping into one release theme means we open, review, and reason about the
sensitive core input/auth code ONCE, rather than several separate risky visits.
From a user's view it is ONE story -- "the lock feels polished": the password box is
responsive AND fingerprint works.

Contrast with v0.2.0 (weather + blur + hero preset): those are PLUGIN/config,
untrusted, low blast radius, high visual flash. This release is the opposite -- high
care, low visual flash (the password box does not photograph; a QUIET Reddit
release). Keep them separate so the careful round never delays the crowd-pleasing
one, and never gets rushed.

The items, input-first:

1. **Input-field feedback** — fail messages, attempt count, waiting/check state,
   caps accent (item 3 below).
2. **Familiar editing keys + grace period** — ESC/Ctrl+U clear, Ctrl+A select-all,
   hold-backspace repeat; an optional no-auth grace window after lock (item 4).
3. **Caps-lock / keyboard-layout forwarding** — the core forwards its OWN state so
   a plugin (or the password UI) can show it (item 2).
4. **Fingerprint auth** (fprintd) — a parallel unlock path (item 1).

## 1. Fingerprint auth (fprintd) — the headline

hyprlock runs fingerprint IN PARALLEL with the password field (password stays live
while a finger can be scanned). veiland has PAM only today.

### The load-bearing security consideration: a SECOND unlock path

Today the entire security story rests on ONE invariant:
`keyboard -> password buffer -> PAM -> unlock`, and *"no IPC message maps to
unlock."* Fingerprint adds a SECOND route to the unlock decision
(`fprintd match -> unlock`). This is acceptable but must be reasoned through:

- The fprintd path must be as trustworthy as the PAM path. fprintd runs on the
  SYSTEM bus as a system service; the core talks to it directly (NOT a plugin --
  the unlock decision stays in the trusted core). A plugin never sees or influences
  fingerprint state.
- A compromised/spoofed fprintd must not be able to FORCE an unlock beyond what
  fprintd itself vouches for -- i.e. we trust fprintd's match verdict exactly as
  much as the OS does, no more. The core calls, fprintd decides identity, the core
  maps a verified match to unlock. Do not invent our own matching.
- Both paths compose: the password field stays live during a scan (parallel, like
  hyprlock), and EITHER a correct password OR a matched finger unlocks. Neither
  path weakens the other; a failed scan does not consume password attempts and vice
  versa (decide the exact lockout interplay -- see below).

### Scope (a focused round, not an afternoon)

- Talk to fprintd over the SYSTEM bus from the trusted core (async: the scan
  lifecycle is ready -> scanning -> matched / no-match -> retry).
- Config, mirroring hyprlock's shape: `fingerprint:enabled` (default false),
  ready/present messages, retry delay.
- Surface scan state to the user (the "Scan fingerprint to unlock" / "try again"
  messages) -- this is where it MEETS item 3 (input-field feedback): the same
  message-rendering surface shows fingerprint state.
- Lockout interplay: fprintd has its own retry limits; PAM (pam_faillock) has its
  own. Decide and DOCUMENT how the two compose so a user is not surprised by a
  lockout from one path while using the other. Match OS/fprintd defaults; do not
  invent policy.
- **Care bar = the auth-path bar.** A bug here means someone unlocks who should
  not, or the locker will not unlock at all. Obvious-correct over clever;
  security-review the diff (there is a /security-review skill).

### Open questions to settle when built

- D-Bus client from the CORE: the core is Rust, so this is a Rust D-Bus client
  (zbus), NOT the Python `veiland_dbus` companion (that is plugin-side). New core
  dep -- justify per CLAUDE.md's "ask before adding dependencies." zbus is the
  obvious choice; confirm.
- Async model: fprintd's scan is async; how does it fit the core's calloop event
  loop (an extra fd / a source), alongside PAM which is blocking? This is the real
  engineering wrinkle -- PAM auth is synchronous, fingerprint is a long-lived async
  subscription. Likely: fingerprint scan on its own loop source, PAM stays as-is,
  either success triggers the unlock state change.
- Does fingerprint auth go through PAM (pam_fprintd) or fprintd directly? Through
  PAM is cleaner (one auth abstraction, respects pam config) but couples the two
  flows; direct fprintd is more control but a second auth mechanism to secure.
  Decide -- pam_fprintd is likely the safer, less-code path.

## 2. Caps-lock / keyboard-layout forwarding (the core-forward round)

Already flagged in widget-roadmap.md as the ONE legitimate "core forwards a signal"
case (universal + core-vouchable, like time). A plugin cannot see layout or caps
state today; no protocol message carries them.

- Caps-lock: the core ALREADY tracks `modifiers.caps_lock` for the password box --
  forwarding it is cheap.
- Keyboard layout: the core knows the active layout; forward the active
  layout name/index.
- Mechanism: extend `Configure` (backwards-compatible field) OR a small new
  `ServerMessage`. `Configure` extension is simplest and backwards-compatible.
  This is the same decision widget-roadmap.md's status-cluster section left open.
- It NEVER becomes a general input/keyboard proxy -- only these two universal,
  core-vouchable signals, same discipline as time. The password/keystrokes stay
  absent-by-construction.
- Unblocks the status cluster's caps-lock + keyboard-layout badges (Python
  plugins), which is why it belongs in the auth-polish round even though the badge
  DRAWING is plugin-side.

## 3. Input-field / password-box feedback

hyprlock's input field shows: fail text (`$FAIL`, `$ATTEMPTS`), a check/waiting
color state, caps-lock/numlock color accents, placeholder text. veiland's password
indicator is simpler.

- Fail feedback: attempt count, last-fail reason surfaced to the UI (careful --
  never log or surface the password itself; the fail REASON is fine, the input is
  not).
- Waiting/check state while PAM is verifying (a spinner/color, so a slow PAM call
  is not a dead-looking box).
- Caps-lock accent: reuses item 2's caps state.
- This is core UI (the password indicator is core-owned -- it is drawn last, on top
  of all plugins). So the RENDERING is core, unlike the badges which are plugins.
- Also relates to the deferred "positionable password indicator" (widget-roadmap.md
  frosted-auth-card preset) -- but that is a separate, bigger want; do not fold it
  in.

## 4. Familiar editing keys + grace period

Small core input-handling additions, all raised on Vaila or present in hyprlock,
that make the password box feel like a real text field instead of append-only:

- **Clear:** ESC and Ctrl+U wipe the buffer (hyprlock's default; must zero the
  freed bytes -- password buffer hygiene, CLAUDE.md).
- **Select-all + delete:** Ctrl+A then a keystroke replaces (Vaila ask). Keep it
  minimal -- there is no cursor/selection MODEL to build; "select-all" here just
  means "next input clears first," not a full editable field with a caret.
- **Hold-to-repeat backspace:** key-repeat on backspace (Vaila ask) -- likely just
  honoring the keyboard's repeat events the core already receives.
- **Grace period:** an optional no-auth window right after lock (hyprlock's
  `--grace N` / `general:grace`): the screen is locked/covered but ENTER (or any
  key/mouse) dismisses without a password for N seconds. Careful: this is a
  deliberate security tradeoff the USER opts into (default OFF); document that it
  weakens the lock for that window. Small code, high appeal, but it touches the
  unlock decision -- security-review it with the rest.

All of this is core input handling on the SAME buffer/keymap code as the existing
password entry -- which is exactly why it belongs in this one careful round rather
than scattered. None of it forwards anything new to plugins (plugins still get no
keyboard, by construction); it is all core-internal editing of the core-owned
password buffer.

## Release shape

One release theme, **v0.3.0 "auth polish"**, OR split if it gets large:
- If fingerprint alone is a big careful round: **v0.3.0 = fingerprint**, and
  **v0.4.0 = caps/layout + input-field feedback**. Two smaller auth rounds.
- If caps/layout + input-field are quick (caps is already tracked): fold all three
  into **v0.3.0 "auth polish"**.
- Decide once fingerprint's fprintd/calloop integration cost is scoped -- that is
  the item that dominates the size. Start there; if it balloons, ship it solo.

Either way: separate from v0.2.0 (plugin work), and security-reviewed.

## Verify plan (when built)

1. Fingerprint: enrolled finger unlocks; wrong finger does not; no finger + correct
   password still unlocks (parallel paths). fprintd absent/unreachable ->
   password-only, no crash (degrade, per the never-crash rule). A denied/lockout
   from one path does not silently lock the other in a surprising way.
2. Caps/layout: toggle caps -> the forwarded state changes -> a badge plugin
   reflects it; absent key -> byte-identical to today (no plugin reads it -> no
   change).
3. Input-field: a wrong password shows fail feedback + increments the count; a slow
   PAM shows the waiting state; the password is NEVER surfaced or logged.
4. `/security-review` on the whole diff -- this touches the unlock decision.
