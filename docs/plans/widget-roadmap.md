# Plan: informational-widget roadmap (the "at-rest picture" plugins)

Status: in progress (updated 2026-07-22). #1 now-playing SHIPPED; #2 status
cluster SHIPPED (battery/wifi/ethernet/bluetooth + pill_color/icon_color
theming; only the caps-lock/keyboard-layout badges remain, blocked on the
small core-forward round). #4 avatar + greeting BUILT (see below), pending
runtime verification. This is the widget-track companion to
`python-sdk.md` (the SDK) and `veiland-shader.md` (the GPU tier). Kept
UNTRACKED in `docs/plans/` per convention.

CONTENT ANCHOR + DEBUG BORDER SHIPPED (branch `content_anchor`, 2026-07-22):
a `veiland_layout` companion + the `content_halign`/`content_valign` opt-in
convention (place a widget's content within its box; flush-left now reachable)
and a `debug_border` overlay, rolled out to markup + the status pills; avatar
and now_playing got the border only. Two follow-ups fell out and are deferred
with their own plan docs: the **avatar split** (disc-only avatar + markup
greeting, `avatar-split.md`) and the **condensed-region star** for now-playing
(`now-playing-star-condensed-region.md`) — both shipped-widget redesigns kept
out of the anchor branch.

## Why this exists — the Reddit signal (read this first)

Two r/unixporn posts, same category, tell the whole story:

- **veiland** (ours, animated GPU plugins): 291 up. Comments were about
  the *project* — AI honesty, quickshell comparison, "I'll try it." The
  one buying signal: *"I'll switch from hyprlock when it has more
  plugins."* Almost zero comments about the *look* or specific widgets.
- **Vaila** (naurissteins, github.com/naurissteins/Veila): 371 up, and
  it explicitly says *"the goal is NOT to be animated or fancy."* Static
  images, no GPU. Comments asked for: **wallpapers** (top comment), the
  **font**, **per-monitor wallpaper**, **TOML includes** (pywal colors),
  a **weather-location override**, **password-box UX** (select-all,
  hold-backspace). Its advertised widget set: **battery, weather,
  now-playing**, plus avatars, presets, blur, 10 theme presets.

**Diagnosis: we did not lose on animation — Vaila proves animation is not
the draw. We lost on the AT-REST PICTURE.** Our post sold an
*architecture*; theirs sold *a lockscreen someone wants on their screen*.
On r/unixporn the thumbnail IS the pitch: a rich static composition
(clock + status cluster + now-playing + avatar over a blurred wallpaper)
reads instantly as "my daily driver"; an animated tunnel reads as "tech
demo." Our own "plugins I'd like to build next" list (now-playing,
status, weather) is *exactly* Vaila's widget set — the jury is telling us
to build it.

**The good news: the runway is already built.** The Python CPU-widget
tier (`python-sdk.md`), the `veiland_svg` companion, and the
region/anchor placement (branch `region-dimension-fix`, merged
2026-07-19) are exactly the infrastructure these widgets need. We have
the airfield; we need the aircraft.

## Guiding principles (don't re-litigate)

- **These are Python CPU-tier widgets, not Rust.** "Poll a source, draw
  text/icons, occasionally" is the exact CPU-widget-tier task
  (`python-sdk.md`). Rust stays the GPU-heavy tier (raymarcher,
  particles). See [[project_python_plugin_plan]].
- **D-Bus lives in the plugin, never the core, never a new protocol
  message.** The core is trusted/minimal (PAM, password buffer); a media
  player / NM / bluez client does not belong in it. The pacer's
  `extra_fds` already multiplexes a D-Bus socket alongside the host
  socket — no threads, no new SDK surface. The core forwards a signal via
  protocol ONLY if it is universal AND core-vouchable (time already does;
  keyboard layout is the one candidate — see Status widget). It never
  becomes a general D-Bus proxy. See [[project_dbus_placement]].
- **D-Bus duplication is deferred, not designed-around (YAGNI).** If a
  2nd/3rd Python widget repeats the D-Bus boilerplate, promote it to an
  opt-in `veiland_dbus.py` companion — the same move `veiland_svg.py`
  already proved. Do NOT build that abstraction before the repetition
  exists.
- **Read-only in v1.** No play/pause buttons, no interactive controls —
  blocked on a pointer/click protocol message that does not exist (its
  own future design round). Widgets display; they do not control.
- **Separate composable plugins, à la carte in TOML** — the label/clock
  model, placed via the anchored `[plugin.region]`. NOT one
  `veiland-statusbar` mega-plugin.

## What already exists (build on, don't redo)

- **Rust GPU plugins:** wallpaper, clock, label, vignette, raymarcher,
  particle family (particles/sakura/snow/rain/embers/fireflies), ambient
  (gradient/parallax/blobs), stress.
- **Python SDK + examples:** `veiland_plugin.py` (codec/Connection/
  GbmDevice/LinearBuffer/FramePacer), `veiland_svg.py` (librsvg→cairo),
  `battery.py` / `battery_cairo.py` / `battery_svg.py`.
- **Placement:** anchored regions (`halign`/`valign` + fraction-of-surface
  `width`/`height`/`margin`), resolution-independent — the status-chip
  placement mechanism. See [[project_anchor_region_fractional]].

## The roadmap — ranked by thumbnail-impact-per-unit-work

Ranking uses the Reddit priority: what makes the AT-REST screenshot read
as a polished daily lockscreen.

### 1. now-playing (MPRIS) — FIRST, highest visual payoff

The single most screenshot-worthy widget: album art + title/artist +
progress bar is a big, colorful, instantly-recognizable block that turns
a bare lockscreen into "a real daily driver." It is what Vaila-style
shots lead with, and it is the widget our own post promised.

- **Tier:** Python. This is `python-sdk.md`'s PR C, already speced.
- **Draw:** pycairo + PangoCairo, zero-copy via `buf.map()` — album art
  (from `mpris:artUrl`, `file://`, decode via PIL), title/artist with
  real shaping + end-ellipsization (long CJK titles are the test case),
  progress bar.
- **Data:** MPRIS over D-Bus, pure-Python client (jeepney or dbus-next —
  decide in-flight, example-only dep). Its socket → pacer `extra_fds`; a
  ~1s TIMEOUT tick advances the progress bar. No player → a quiet
  "nothing playing" state.
- **Placement:** anchored region (e.g. a card bottom-left or bottom-
  center). This is the first *large* anchored region (vs. the small
  status chip) — good exercise of the placement work.
- **De-risks the D-Bus + PangoCairo stack** for every later widget.

### 2. status cluster — SECOND, cheapest (half-built)

The top-right chip row every polished lockscreen has:
**battery** (DONE — `battery_svg.py`), **wifi**, **bluetooth**, plus
**caps-lock** and **keyboard-layout** badges.

- **Tier:** Python, SVG-swap pattern (`veiland_svg` + `if/else` picks a
  bucketed icon), each a small anchored pill. `battery_svg.py` is the
  copy-me template.
- **wifi:** signal buckets from NetworkManager over D-Bus (reuses now-
  playing's D-Bus plumbing — do NOT invent D-Bus before #1 settles it).
- **bluetooth:** bluez over D-Bus, on/off + connected.
- **caps-lock / keyboard-layout — THE EXCEPTION, needs a core change.**
  A plugin cannot see the layout or caps state today; no protocol message
  carries them. This is the one legitimate "core forwards a signal"
  case (universal + core-vouchable, like time): host forwards active
  layout + caps via `Configure` (or a small new field). Its own small
  design round; tracked here, blocks nothing else. Caps-lock the core
  already tracks (`modifiers.caps_lock`) for the password box — forwarding
  it is cheap.
- Ships a first-party CC0 icon set (buckets) under `python/examples/icons/`
  (battery buckets already there).

#### Deferred: configurable chip background (color + shape)

Status/planning (2026-07-19). The status pills (`battery_svg.py`,
`wifi.py`, and the coming ethernet/bluetooth) currently HARDCODE the
translucent dark circular chip: `PILL_BG = (15,18,28,175/255)` +
`vs.draw_pill(...)` (a `cr.arc`, circle only). A ricer will want to
restyle it — a transparent/glyph-only chip, a colorscheme tint, or a
rounded-rect (which suits a chip *row* better than circles). That is
config, not code. Flagged while building wifi; deliberately NOT built yet.

**Agreed scope IF/WHEN built (keep it this small):**

- `[plugin.config]` gets two optional keys, applied UNIFORMLY across every
  status pill so a cluster is themed in one place:
  - `pill_color = [r, g, b, a]` (0..1 floats). Omitted OR alpha 0 ->
    transparent / today's default. Same color-array shape `gradient` uses.
  - `pill_shape = "circle" | "rounded" | "square"`. Default `"circle"`
    (today's look). `rounded` reuses the existing `rounded_rect` helper.
- Both omitted -> pixel-identical to today. Bad/unknown values -> fall
  back to the default, never crash (untrusted-input rule).
- Helper home: a `draw_chip(cr, cx, cy, w, h, shape, rgba)` in
  `veiland_svg.py` next to `draw_pill` (keep `draw_pill` for back-compat);
  both plugins already import `veiland_svg`, so no new dep, no per-plugin
  duplication, and ethernet/bluetooth inherit it. Plus a
  `chip_style_from_config()` parser so each plugin reads the two keys the
  same way.

**Why it's deferred, not done — the rabbit hole to think about first.**
"Configurable chip" is a doorway to a whole theming DSL: per-corner radii,
radius in px vs fraction, borders/outline, padding, drop shadow, blur,
gradients-as-fills, and then "why not the glyph color too, and the icon
set, and per-monitor overrides." CLAUDE.md says don't add speculative
surface. The named-enum shape (3 values, no freeform radius) + a single
color array is the deliberately-boring cut that covers the real asks
without opening the DSL. Before building even that, decide: does chip
styling belong per-plugin at all, or is it really the front edge of the
**`[palette]` / preset theming layer** (a one-way door already deferred —
see [[project_palette_theming_deferred]])? If a shared theme is coming, a
per-plugin `pill_color` may be the wrong place to put it and we'd want the
chip to read a shared named color instead. That question is the actual
blocker; the code is 30 minutes. Settle the layering, THEN build.

### 3. weather — THIRD, more plumbing for less flash

Requested on BOTH posts, so real demand — but it is an HTTP/API +
location + icon-set piece: more moving parts (network fetch, API
key/provider, geolocation, refresh cadence, offline state) for a smaller
visual block than now-playing.

- **Tier:** Python. HTTP (stdlib `urllib` to keep it dep-light, or an
  example-only `requests`), an icon set (SVG-swap by condition code), temp
  + condition text via PangoCairo.
- **Provider:** pick one with a keyless/free tier (e.g. Open-Meteo needs
  no key — good default; wttr.in as a fallback). Decide in-flight.
- **Location:** config-provided lat/long or city string. Note the Vaila
  feedback — support a location OVERRIDE so users can screenshot without
  doxxing.
- **Network from a locked screen** is a wrinkle worth thinking about
  (cache last result; never block the pacer on a slow fetch — do it off
  the render path / on the TIMEOUT tick with a timeout).

### 4. avatar + greeting — BUILT 2026-07-20 (pending runtime verification)

Vaila leads with avatars. A user avatar (round-cropped image from config)
+ a "Welcome back, <user>" greeting is trivial (static image + one text
line, no data source) and adds a lot of "this is *my* lockscreen"
warmth to the composition.

- **Tier:** Python (cairo image + PangoCairo text) or even config-driven.
- No D-Bus, no network — pure config + `$USER`. Cheapest of all; consider
  bundling into the hero preset.

**Built as `python/examples/avatar.py` + `docs/examples/avatar.toml` after a
design round against a set of hyprlock reference rices.** The design lesson
from those references (and the fix to the first mockup): the avatar is NOT a
floating hero — it sits small (~100 px) on the center column, glued just
above the core's password indicator, with the greeting in the family glass
pill (a small user glyph + text) that visually pairs with the password box.
Decisions settled: glass pill on by default (`pill_color`, alpha 0 = bare
text — the status pills' key, so one theme covers the cluster);
`greeting = "auto"` gives time-of-day; zero-config defaults are GECOS full
name then `$USER`, and `~/.face` then a name-hash-tinted initials disc;
`layout = "stack" | "row"` (row = corner capsule at status-pill height).
Icon `icons/user.svg` added to the CC0 set.

### 5. dashboard embed (Home Assistant / Grafana) — COMMUNITY-DEMO, post-weather

"Glanceable dashboards on the lock screen" (is the door locked, thermostat,
who's home, energy; or a Grafana panel) is a real want with strong
r/homeassistant + r/unixporn overlap, and it fits the at-rest-picture
thesis (a dense colorful tile block). But it is a **different class** from
the universal widgets above and is deliberately scoped as
**community/showcase, NOT blessed first-party.** Reasons:

- **Niche + opinionated.** Everyone has music/battery/weather; a HA/Grafana
  dashboard is powerful but specific to those users and to *their* chosen
  panels/layout. This is exactly the "write your own plugin, no PR needed"
  freedom veiland exists to enable — the ideal *showcase* of the plugin
  system, not a widget the project maintains for everyone.
- **Two implementation paths, very different cost:**
  - **(a) Render the API yourself — the in-model path.** HA REST/WebSocket
    or Grafana JSON → draw tiles with cairo/PangoCairo. Same tier as
    weather (HTTP + cache + offline), just a richer source. You control
    every pixel; no web engine; no remote HTML. **This is the path to
    demo.**
  - **(b) Embed the real web dashboard.** Grafana "render panel to PNG" or
    a headless browser (Playwright/CEF) rasterizing a Lovelace URL. Drags a
    whole web-rendering stack — and remote HTML/JS — onto a
    security-sensitive lockscreen. **Explicitly community territory,
    documented-but-not-first-party**; too much attack surface for the
    security story veiland has built.
- **Security/privacy notes (do not gloss):** a dashboard plugin wants
  **network access from the locked screen** and **stored credentials** (a
  HA long-lived token / Grafana API key in plugin config, readable by
  same-UID code). This does NOT break the core boundary — the plugin still
  can't unlock, can't see the password (absent by construction) — but it
  DOES mean potentially-sensitive data is shown on a screen a passerby sees
  while locked, fetched with stored secrets. That is a user-facing privacy
  choice to surface in docs (e.g. offer a redacted/summary "locked view"),
  not a core-security hole. Path (b)'s remote-HTML rendering is the real
  surface-area concern and the reason it stays community-only.
- **Sequencing:** after weather (#3), because weather proves the
  HTTP-fetch + cache + offline-state + don't-block-the-pacer pattern that
  path (a) reuses. Ship path (a) as a flagship *"veiland can even show your
  Home Assistant"* demo — a strong follow-up Reddit post that sells the
  plugin-freedom pitch — without touching the security model.
- **Open:** the user has real HA/Grafana dashboards to look at; the panel
  density decides (a-viability: a few big tiles draw cleanly with cairo; a
  wall of graphs really wants image-embedding = path b = community).

## The real deliverable: a HERO PRESET, not loose widgets

The Reddit lesson is that the *composition* wins, not any single widget.
So the payoff is not "I added now-playing" — it is **one gorgeous default
scene** combining clock + status cluster + now-playing (+ avatar) over a
blurred wallpaper, shipped as a named preset, and *that* is the next
Reddit post. The widgets above are the ingredients; the preset is the
pitch. Build the ingredients #1→#4, then compose.

Two composition dependencies worth noting (both also Vaila-requested,
both core/theming not plugins — a PARALLEL track):

- **Blur** — Vaila commenters loved it ("sucker for blur"). A blurred
  wallpaper backdrop is a big chunk of the "polished" read. Likely a
  wallpaper-plugin option or a core compositing pass; needs its own
  scoping.
- **Presets + first-class wallpapers** — Vaila's top comment was "may I
  have the wallpapers?" and it ships 10 theme presets. A curated
  wallpaper set + a handful of named presets (one being the hero) is
  high-leverage ricing polish, independent of any widget.

## Parallel non-widget track (Vaila won here too — don't ignore)

Not plugins, but the Reddit comments prioritized these; log so they are
not forgotten:

- **Per-monitor wallpaper** (Vaila shipped it; a commenter gated adoption
  on it).
- **TOML includes** (pywal-color files — Vaila shipped it; ricers want
  colorscheme integration).
- **Password-box UX** (select-all / ctrl-a, hold-to-repeat backspace —
  both raised on Vaila).
- **Blur + presets + wallpapers** (above).
- **Fingerprint auth via PAM** — our own post floated it; broad appeal,
  but a core/auth change, not a widget. Separate security-sensitive round.

Logged 2026-07-20 from the hyprlock reference rices reviewed for the avatar
design round (steal later, none built now):

- **Two-tone stacked clock style** — hour over minute in two colors (an
  accent + white); a clock-plugin styling option, not a new widget.
- **`layout = "minimal"` for now-playing** — one quiet "song · artist" text
  line, bottom center; several references use it and it reads beautifully.
- **Frosted auth-card preset** — one glass card holding avatar, name, clock,
  and the password UI. Blocked on the core's password indicator becoming
  positionable via config; a hero-preset candidate, its own design round.

## Suggested order

1. **now-playing** (#1) — de-risks D-Bus + PangoCairo, biggest single
   visual win, closes the gap with the post that beat us.
2. **status cluster** (#2) — wifi + bluetooth reuse #1's D-Bus plumbing;
   caps/layout needs the small core-forward change.
3. **avatar + greeting** (#4) — cheap, bundle toward the preset.
4. **weather** (#3) — more plumbing; do once the cheaper wins are banked.
5. **Hero preset + a curated wallpaper/blur pass** — compose the
   ingredients, then the Reddit repost.
6. **dashboard embed (HA/Grafana), path (a)** — community/showcase demo
   AFTER weather (reuses its HTTP+cache pattern); the "plugin freedom"
   flagship, not a maintained first-party widget. Path (b) stays
   community-only.

The text plugin + veiland_text.py extraction (section below) is agreed but
unranked; its natural slot is BEFORE weather, since the companion's
font/text helpers also serve weather's temp/condition text.

Non-widget polish (per-monitor wallpaper, TOML includes, password-box UX)
runs in parallel as its own track — it is where Vaila also out-scored us.

## Open decisions (settle in-flight, per widget)

- jeepney vs dbus-next for the Python D-Bus client (now-playing decides
  it; wifi/bluetooth inherit).
- Weather provider (lean Open-Meteo, keyless).
- Whether caps/layout forwarding rides on `Configure` (extend) or a new
  small `ServerMessage`. Backwards-compatible either way; `Configure`
  extension is simplest.
- Icon-set licensing: keep first-party CC0/self-drawn so redistribution
  stays clean (battery set already is).

## markup plugin + veiland_text.py companion (agreed 2026-07-20)

The Python tier's general-purpose text engine, in two pieces, one work item.
This is the answer to "every text widget needs font config" and to the
hyprlock-style freeform label -- NOT a reason to grow avatar or now_playing.

**Naming, settled 2026-07-21 (read before touching any of the three names).**
There are THREE distinct text names and they are deliberately NOT the same:

- **`veiland-text` (Rust crate)** -- the internal GL text ENGINE (`Label`,
  `FontContext`) compiled into `veiland-core`, `veiland-label`, `veiland-clock`.
  A build dependency, invisible to users; nobody ever types it in a TOML. NOT a
  plugin. Do NOT move it into the core (clock/label are separate-process plugins
  that must not link the trusted core crate) and do NOT name anything else
  `veiland-text*`.
- **`veiland-label` (Rust plugin)** -- the STATIC styled-text plugin (one style
  per label, `text = "..."`), backed by the crate above. Used five times in
  `shinkai.toml`. Stays the lean tier: a static binary, needs NO Python/Pango
  stack at lock time.
- **`markup.py` -> installs as `veiland-markup` (Python plugin, THIS item)** --
  the DYNAMIC tier. Named for its headline feature (Pango `<span>` markup +
  `{variable}` substitution), which is exactly what `veiland-label` cannot do.
  `markup` not `text` (collides with the crate) and not `template` (too wide);
  `markup` names the actual mechanism and is two segments like every sibling.
  Source file `markup.py`, `Hello`/log name `"markup"` (unprefixed, matching the
  other Python examples), packaged install-name `veiland-markup` (the
  `veiland-<name>` binary convention -- a packaging-time concern, see below).

**It is ADDITIVE to `veiland-label`, not a replacement.** Do not "consolidate"
them: `veiland-label` is the no-Python-needed static tier (the property
`shinkai.toml` relies on -- a showcase scene must not require a Python
interpreter + GI typelibs on the lock path); `veiland-markup` is the
rich/dynamic tier (variables, inline markup) for text that changes or mixes
styles. Same lean-Rust vs rich-Python split the whole project already makes.

- **`veiland_text.py` companion -- DONE 2026-07-21 (the text plugin's first
  commit).** The promotion rule (veiland_svg, then veiland_dbus: build the
  companion when the boilerplate repeats) was met: avatar.py copied
  `_line_layout` + the ellipsized draw helpers from now_playing.py. Shipped as
  a no-op refactor: the companion holds the shaped/ellipsized single-line
  layout (`line_layout`), the top-left/centered/right-aligned draw helpers, and
  `font_from_config()` -> a frozen `FontSpec` reading the uniform
  `font_family`/`font_size`/`font_weight`/`italic` keys (names AND defaults
  matching the Rust label plugin: "Sans", 0.030 fraction-of-a-box). now_playing
  + avatar now import it; `mypy --strict` + ruff clean. `font_from_config` is
  present but NOT yet wired into any widget -- it waits on the uniform-font
  layering decision (below); text.py is meant to be its first consumer.
- **`markup.py` widget: one block of Pango MARKUP + variable substitution --
  BUILT 2026-07-21 (commit f9ba8bb), runtime-verified.** Inline
  sizes/weights/colors come free via `<span>` (no config-key styling DSL to
  invent). Variables substituted str.replace-style, never str.format: `{user}`,
  `{name}` (GECOS), `{host}`, `{time:%H:%M}`, `{date:%A %d %B}`. A tick
  re-renders only when the substituted string actually changed. Malformed markup
  -> Pango's parse error is caught (`Pango.parse_markup` pre-check), fall back to
  plain text; bad strftime -> the literal; the usual never-crash rules. A
  command-runner tier (`cmd[update:N]`-style) is explicitly DEFERRED (user-owned
  config running as the user, so no boundary issue -- just scope v1 does not
  need). Consumes `veiland_text.py`'s `font_from_config` (its first consumer) for
  the base font; `<span>` overrides it inline. Two calibration decisions settled
  while building, both non-obvious:
  - **Pango owns horizontal placement.** The layout width is the whole region
    and its alignment IS the widget's `halign`, so Pango already centers/left/
    right-justifies each line within the box. The widget draws at `x = 0` and
    only computes `y` for `valign` -- adding a manual halign offset on top
    double-shifts the block (the first-mockup bug). `get_pixel_extents().logical.x`
    already carries Pango's centering offset; don't re-add it.
  - **`font_size` defaults to 0.20 of the region HEIGHT, not font_from_config's
    0.030.** That 0.030 is calibrated for the Rust label's fraction-of-SURFACE; a
    markup region is a short anchored box, so the same fraction there renders ~7px
    (tiny). markup.py resolves the FontSpec from config, then swaps in 0.20 iff
    the user left `font_size` out (an explicit value is honoured). This is NOT a
    reason to change `font_from_config`'s default -- the fraction's meaning is
    per-widget (which box it multiplies), so the override lives in the widget.
- **Decision: avatar KEEPS its greeting.** The v2 design lesson was that
  avatar + pill read as one object with tight rhythm; splitting into two
  plugins would make users hand-align two regions. Both compositions stay
  expressible a la carte: `greeting = ""` gives the bare avatar, and
  text.py handles any freeform text beside it. Anything fancier than one
  greeting line belongs in text.py, which is also why avatar's text_size
  (below) stays a tiny knob, never markup.

## Deferred SDK extraction: `_pil_to_surface` (logged 2026-07-21)

The SECOND duplication, surfaced while extracting veiland_text.py.
`_pil_to_surface` (PIL RGB image -> a cairo ARGB32 ImageSurface: reorder
RGB->BGRA + full-alpha byte, pad rows when cairo wants a wider stride) is now
copy-pasted VERBATIM in `now_playing.py` and `avatar.py` -- the same repeats
that earned each companion. It was deliberately LEFT OUT of veiland_text.py:
it is IMAGE plumbing, not text, and dragging it into the text companion would
blur that file's one job.

- **Where it goes: an opt-in companion, NOT the core SDK.** The instinct
  "put it in the SDK" is right that it belongs in shared vendorable code, but
  `veiland_plugin.py` is deliberately the SINGLE stdlib+ctypes file with no
  heavy deps -- and `_pil_to_surface` needs Pillow, exactly the kind of dep
  that kept librsvg out (veiland_svg) and jeepney out (veiland_dbus). So the
  established pattern points at a small `veiland_image.py` companion (PIL ->
  cairo surface helpers), vendored only by widgets that decode images
  (now_playing's covers, avatar's picture, later weather/HA icons). Same move,
  third time.
- **Don't build it on two callers alone if a third isn't imminent** -- the rule
  is "extract when it repeats," and two is the threshold the other companions
  used, so this IS ripe; but it is a clean standalone commit, not part of the
  text work. Natural slot: alongside weather (#3), which decodes condition
  icons and would be the third caller, making the companion pay for itself
  immediately. Until then it stays duplicated (harmless, verbatim).

## Deferred (packaging): install Python widgets as `veiland-<name>` (logged 2026-07-21)

The `veiland-<name>` convention (`config.md`, CLAUDE.md) is about the INSTALLED
`$PATH` binary a user references by bare name -- it is a packaging-time concern,
NOT a source rule. Today the Python examples correctly sidestep it: their TOMLs
use the PATH form (`binary = "./python/examples/wifi.py"`, a `/`-containing path
used verbatim), because they run from the source tree and are not installed. So
there is NO convention violation to fix now, and the existing Python plugins
should NOT be renamed -- their source filenames (`wifi.py`) are Python module
names, their `Hello`/log names (`"wifi"`) are unprefixed like every example.

When packaging lands (deferred, see [[project_python_plugin_plan]]), the install
step should expose each Python widget as a `veiland-<name>` executable
(shebang + chmod +x -- see [[project_python_plugin_runtime_gotchas]]) so a user
references `binary = "veiland-wifi"` exactly as they do a Rust plugin, with no
idea or care that it is Python. Applies uniformly to ALL Python widgets at once
(battery/wifi/ethernet/bluetooth/now-playing/avatar/markup), decided in the
packaging pass -- not piecemeal now. `veiland-markup` is simply that packaged
name picked early (to settle the crate-collision question); until packaging its
TOML uses the path form like its siblings.

## Deferred config knobs: text_size + a uniform font key (logged 2026-07-20)

**Update 2026-07-21: `font_family` + `italic` are now SHIPPED across the Python
text tier** (markup, now_playing, avatar), threaded through `veiland_text`'s
`line_layout`/`draw_ellipsized*` via an optional `FontSpec` (family + italic
only; weight stays per-line, size stays per-callsite). `font_from_config` reads
the uniform `font_family`/`font_size`/`font_weight`/`italic` keys. So the
`font_family` bullet below is DONE for the Python tier; `font_size` and
`text_size` remain deferred (see the two notes below for why size is the harder
half). The `[palette]`-layering question is untouched: family shipped as a
per-plugin key, which does NOT foreclose a later shared theme value.

Two text knobs surfaced while reviewing the avatar widget; neither is built.

- **`text_size` (avatar, small, build on demand).** Today every avatar
  dimension derives from the region height (pill 30% of h, text 44% of the
  pill), so text and avatar can only grow together. `text_size` = an optional
  fraction-of-region-height for the greeting text (default exactly today's
  derived value), the same fraction-of-a-box model label/clock font sizes
  use. The pill already wraps the measured text, so glyph/padding/capsule
  follow automatically; clamp so avatar + pill never overflow the region;
  bad value -> default + one stderr line. Companion tweak, same visit:
  avatar-only mode (`greeting = ""`) should let the disc FILL the region
  (diameter = min side, minus ring room) instead of keeping the 60%-of-h
  size that exists to leave room for a pill that isn't there.
- **`font_family` (uniform, needs a family-wide decision -- do NOT build
  per-widget).** The RUST text plugins already expose this: label has
  `font_family` + `font_size` (fraction of surface height) + letter_spacing
  + italic, clock has `time_font_size`/`date_font_size`. It is the PYTHON
  tier that hardcodes Pango "sans-serif" (avatar, now_playing); ricers ask
  "what font" immediately (a top Vaila comment). Per-widget it is trivial
  (FontDescription.set_family, fontconfig falls back gracefully =
  untrusted-input safe), but like pill_color it must be ONE decision applied
  uniformly -- and the key names should match the Rust plugins' existing
  `font_family`/`font_size`, not invent new ones. It also brushes the same
  layering question as the deferred `[palette]` theming: per-plugin key vs
  shared theme value. Settle that layering first, then it is an afternoon
  across the family; the parser's home is `font_from_config()` in
  `veiland_text.py` (see the text-plugin section above).

## Deferred: text sizes are nominal-px, so fonts don't scale the same (logged 2026-07-21)

Surfaced right after `font_family` shipped: a user set a cursive font
(Dancing Script) on now_playing and the text "did not scale the same" as Sans.
Correct observation, and the diagnosis is worth keeping precise because it is
TWO separable problems, not one:

- **(inherent) Visual size varies per font at equal nominal px.** Every text
  line is sized by `set_absolute_size(px)` where `px` is a fixed fraction of the
  card (`card_w * 0.058` for the now_playing title, `pill_h * 0.44` for the
  avatar greeting, etc.). That px is the font's NOMINAL size (em/ascent box), not
  the height of the actual glyphs -- and how much real letter sits inside that
  box is a per-font property (cap-height / x-height). Sans nearly fills its em;
  Dancing Script spends much of its height on loops/slant, so the same px reads
  smaller and runs to a different width. This is normal typography (every engine
  does it); our layout just leans on it IMPLICITLY by having tuned those
  fractions to Sans metrics.
- **(fixable) The layout does not size or reflow to MEASURED extents.** The only
  `get_pixel_extents` call in now_playing sizes the pause-badge pill; the
  title/artist lines are placed at fixed geometry fractions and rely on
  end-ellipsization to cut overflow, never measuring the rendered line. The
  principled fix is to size text so a MEASURED metric (cap-height, or the line's
  ink height) hits a target fraction of the box -- then "title = 8% of the card"
  means the same VISUAL height for any font. It belongs in the shared
  `veiland_text` sizing path so markup + now_playing + avatar inherit it at once.

Three options were on the table when revisiting: (a) a per-widget `text_scale`
multiplier (default 1.0) as a pragmatic dial the user bumps per font -- cheapest,
no metrics math, but does not auto-normalize; (b) the measured-metric sizing
above -- the real fix, its own focused pass touching `veiland_text`; (c) wire
`font_size` (the fraction) to actually drive now_playing/avatar text the way
markup already does (today both IGNORE `font_size` and derive every size from
geometry -- so setting `font_size` in a now_playing/avatar TOML currently does
nothing; that is deliberate but surprising, and worth at least documenting). Any
of these also answers the recurring "how do I make the now-playing text bigger"
ask; until one lands, the only lever is a bigger `region` (the whole card scales
together).

**Chosen 2026-07-22: option (a), `text_scale`.** The pragmatic dial won -- design
spec written up FIRST (before code) in `docs/plans/text-scale.md`: config key,
[0.25, 4.0] bounds, parser home (`text_scale_from_config` in `veiland_text.py`),
which px it multiplies (now_playing title/artist/times + avatar greeting/initials;
NOT chrome like the pause badge; NOT markup, which has font_size), the overflow
decision, and the verify plan. The measured-metric fix (b) stays deferred. See
that doc before building.

## References

`python-sdk.md` (SDK + PR C now-playing spec), `veiland-shader.md` (GPU
tier), and memory: [[project_python_plugin_plan]],
[[project_python_svg_widgets]], [[project_anchor_region_fractional]],
[[project_dbus_placement]], [[project_recording_gallery_gifs]] (how to
capture the eventual hero screenshot/GIF).
