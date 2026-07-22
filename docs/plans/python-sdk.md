# Plan: Python plugin track — SDK, examples, history

Status (2026-07-18): Step 0 done (demo verified on both boxes). **PR A
landed** (`c12b9fb` — codec + Connection + golden-byte tests), followed by
a Python tooling commit (`6d55870` — ruff + mypy --strict + pytest wired
into `nix flake check` and the git hooks; config in `python/pyproject.toml`).
**PR B landed** (GbmDevice + LinearBuffer + FramePacer + the battery example
rewritten on the SDK; the no-SDK original moved to
`docs/examples/battery_nosdk.py`) — runtime-verified rendering on a real
locked session. **PR C is next.** Three PRs (A-C), done one-per-context in
the order below; the PR C section below is still forward-looking spec and
gets folded down to "done" when it lands.

**This is the single plan doc for the Python plugin track.** It absorbed
the earlier `python-plugin-support.md` on 2026-07-18 (that file has been
deleted); read this file first in each new session, nothing else is
required.

## Context

`battery.py` (tracked at the repo root on master, 308 lines, committed
in `6dfee2b` together with `docs/examples/battery_python.toml`) proved
a plugin can be pure Python: it speaks the wire protocol
(`docs/protocol.md`) directly, allocates a linear GBM buffer via ctypes,
draws with Pillow, and paces frames by hand. That validates the "the
interface is the protocol, not the crates" claim. Of those 308 lines,
~65 are the widget; the rest is ceremony (codec ~40, handshake ~20,
ctypes GBM ~60, premultiply+upload ~30, event loop with pacing state
machine ~80). The SDK's job is to make authors write the 65.

Positioning (settled in the 2026-07-18 design discussion): the Python
SDK is the **CPU widget tier** of the authoring ladder — config/presets
→ shader files (veiland-shader) → Python widgets → Rust. Battery
status, avatars, calendars, a now-playing card. GPU-intensive content
is served by the Rust SDK and veiland-shader; GPU-from-Python is NOT an
SDK goal.

## History — landed work (all on master)

- **GL error observability** (`fb73814`): `VEILAND_GL_DEBUG=1` enables
  `GL_KHR_debug` callback where available plus a `check_gl(label)`
  fallback at the fragile boundaries; diagnostics only, zero cost off.
- **NVIDIA external-only dmabuf sampling** (`a30a1e6`): NVIDIA marks
  LINEAR/CPU-written dmabufs external-only — `eglCreateImage` succeeds
  but the `GL_TEXTURE_2D` bind fails, previously unchecked, sampling
  opaque black. Fix detects the failed bind, retries on
  `GL_TEXTURE_EXTERNAL_OES` (fresh texture name — NVIDIA fixes a
  texture's target at first bind), records the target on `GlTexture`,
  and composites with a `samplerExternalOES` program variant. Without
  this, every CPU plugin is invisible on NVIDIA.
- **Spawn-size fix** (`spawn-on-configure.md`, implemented): plugins
  spawn eagerly with the output's current-mode dimensions; the
  lock-surface configure resend remains as a correction path. The 4K
  regression detector is already reverted to the 1080p fallback in
  `host_spawn.rs`.
- **Step 0 — demo verification (2026-07-18): DONE.** Hand-rolled
  `battery.py` renders on both boxes against current master — card at
  top-left (its designed 40 px-inset placement) on Intel and on NVIDIA,
  the latter exercising the TEXTURE_EXTERNAL_OES path with a real CPU
  plugin for the first time. This is the baseline PR B compares
  against.

## Host region-placement fix — LANDED (2026-07-19, branch `region-dimension-fix`)

**Configure sends the surface, not the region** (the "Open host issue"
this section used to defer) is FIXED. Two commits on branch
`region-dimension-fix` (pushed to origin; not yet merged to master):

- **Part A — dimension fix (`0f56130`).** `region::configure_dims`
  decides what Configure carries: full surface (0, 0, w, h) when no
  region is declared (byte-identical to before — every shipped plugin
  unchanged), the region's own (x, y, w, h) when one is, so the plugin
  allocates a region-sized buffer and the composite is the identity
  transform, not a stretch. Applied at both Configure sites
  (`host_spawn.rs` initial, `app/mod.rs` resend). Verified on the 1080p
  NVIDIA box with a diagnostic probe: circle aspect 1.13→1.00, border
  1px→6px.
- **Part B — anchor placement (`44f52ff`).** Anchored region form:
  `region = { halign, valign, width, height, margin }`. halign/valign
  are `left|center|right` / `top|center|bottom` (screen-relative, no
  container); width/height/margin are FRACTIONS of the surface (bare
  floats, the label/clock text model — NOT `anchor = "top-right"` string
  tokens, and NOT hyprlock's `"%"` suffixes). Resolved host-side against
  each output's live size and kept unresolved on the slot so a mode
  change re-anchors. The two forms (anchored vs explicit pixel
  `{ x, y, w, h }`) are mutually exclusive; a mix is a config error.
  Verified live on BOTH boxes (1080p NVIDIA + 4K Intel), multi-monitor,
  and hotplug/replug: one `battery_svg.toml` hugs the top-right at the
  same relative size everywhere.

Layer 2 (plugin centres content in its own region-sized buffer) shipped
with Part A: `battery_svg.py` dropped its `buf.width - badge_gap` screen
math for plain `buf.width/2` centring, and `battery_svg.toml` uses the
anchored form. The old "**no region-assigned Python example**" constraint
is LIFTED — `battery_svg.toml` is exactly that example now.

Remaining: merge `region-dimension-fix` to master (pending final sign-off).
See [[project_region_placement_bug]] and [[project_anchor_region_fractional]].

## Decisions settled — do not re-litigate

- **CPU-first; no GL surface in v1.** Python's niche is widgets; the
  GPU-from-Python demo (see "Optional future" below) no longer gates
  anything. If a GL analog is ever wanted it is additive to this SDK,
  decided on demand.
- **No shm buffer path** (settled during the demo work): it doesn't
  remove work (it *adds* a second host sampling path), the allocation
  ceremony it would simplify belongs in the SDK instead, and it didn't
  fix the NVIDIA bug. shm only becomes interesting later as a
  *sandboxing* move (plugins with no `/dev/dri` access) — not a goal
  today.
- **Drawing-library agnostic.** The SDK ends at protocol + pacing +
  "here is a writable premultiplied-BGRA memoryview with a stride." No
  blessed drawing library. PIL, cairo, and QImage can all fill that
  interface:
  - cairo `FORMAT_ARGB32` and QImage `Format_ARGB32_Premultiplied`
    match the ARGB8888 little-endian dmabuf layout byte-for-byte,
    premultiplied included — zero conversion.
  - PIL has no premultiplied concept, so the SDK ships a
    premultiply+copy convenience (`upload(pil_image)`) on top of the
    raw interface, not instead of it.
- **Pure Python reimplementation, NOT PyO3/maturin bindings over the
  Rust crates.** (Asked and settled 2026-07-18.) A compiled extension
  would require wheels or a Rust toolchain from exactly the audience
  the SDK exists to spare, and would kill single-file vendoring
  (binary wheels are also NixOS pain). The overlap is an illusion
  anyway: `veiland-plugin` is the GL path — the CPU `LinearBuffer`
  primitive doesn't exist in Rust to be wrapped — and the only truly
  shared logic is the ~150-line codec, the cheapest part. The second
  independent implementation of `protocol.md` is a feature: the doc
  declares itself the source of truth, and an implementation from the
  doc is what proves it. Sync strategy: golden-byte tests derived from
  the doc, cross-checked against the Rust tests. API relationship to
  the Rust SDK: same nouns, same lifecycle, same protocol semantics;
  idiomatic surface per language (context managers, dataclasses,
  fd-multiplexing pacer).
- **Zero dependencies in the SDK itself.** stdlib + ctypes only
  (libgbm loaded at runtime, with the `libgbm.so.1` NixOS soname
  fallback from battery.py). Examples may use Pillow / pycairo /
  PyGObject / a D-Bus lib — imported by the example, never by the SDK.
- **Single vendorable file: `python/veiland_plugin.py`.**
  Copy-one-file-into-your-dotfiles is a feature (no pip, works on
  NixOS). PyPI publishing and packaging polish deferred to packaging
  time, per the usual rule. **Vendoring is the *default*, not the *only*
  path — PyPI is additive, not either/or** (raised 2026-07-18). At
  packaging time a `pyproject.toml` ships this same file so
  `pip install veiland-plugin` works for people on normal distros who
  want dependency management; drop-in stays first-class for NixOS /
  dotfiles / air-gapped / zero-dep cases. Vendoring is safe to keep as
  the default even post-PyPI precisely because the file is stdlib-only
  (no dependency closure to miss updates on) — the same NixOS/no-wheels
  reasoning that killed the PyO3 idea. No packaging scaffolding lands
  before the SDK stabilizes (after PR C).
- **Imperative primitives, not a framework** — same philosophy as the
  Rust SDK, stated in `plugin-api.md`. `Connection`, `GbmDevice`,
  `LinearBuffer`, `FramePacer`; the author owns `main()`. No
  `run_plugin()`.
- **Python >= 3.9** (`socket.send_fds` / `recv_fds`).
- **The pacer multiplexes external fds and timeouts.** `select` over
  the socket plus caller-supplied fds, with an optional timeout event.
  This is how D-Bus connections and refresh timers integrate without
  threads — the "second event source" question from the MPRIS design
  round dissolves in Python.
- **CPU plugins are inherently slow-path.** The buffer is complete
  before `sendmsg` by construction; never attach a fence fd, always
  exactly 1 fd per Buffer. The host capability bit is read (and
  reserved bits still fail closed) but fence support is unused.
- **v1 examples are read-only widgets.** Interactive controls
  (play/pause buttons) are blocked on a pointer/click protocol message
  that does not exist; that is its own future design round and
  explicitly not part of this plan.

## The subtle bits the SDK must own (extracted from battery.py)

These are the parts a plugin author gets wrong without an SDK; each is
implemented inline in battery.py today and moves into the SDK:

- **Handshake**: version u32s, reserved-capability-bits fail-closed
  (protocol.md §5.1), Hello before any heavy imports or device setup
  (the 2 s spawn budget).
- **The reconfigure drain**: on a resize, wait for the in-flight
  buffer's release *while still honouring* FrameDone (keep the cue) and
  Shutdown (exit) — battery.py's inner `while not released` loop, the
  single easiest thing to get wrong in a hand-rolled plugin.
- **The pacing state machine**: `released` / `have_cue` / `dirty`,
  render only when all three allow, `FrameDone` may arrive before or
  after `BufferReleased` (protocol.md §8 note).
- **Stride discipline**: the *map* stride (returned by `gbm_bo_map`,
  used for CPU writes) and the *bo* stride (sent in the Buffer message)
  are distinct values that happen to often agree; use each in its
  place. Rows are written top-down.
- **Fd lifetime**: the bo's exported fd is kept for the buffer's
  lifetime (re-sends reuse id 0 with the same fd), closed together with
  `gbm_bo_destroy` on resize/teardown. `send_fds` dups per message.
- **Transport**: one `recv` of 64 KiB per message (SOCK_SEQPACKET
  framing), EOF anywhere = host gone = clean exit, never an exception
  trace.

## API sketch (shape is settled; exact signatures decided in-flight)

```python
import veiland_plugin as vp

conn = vp.Connection.connect("battery", "0.1.0")  # env fd, handshake, Hello
cfg  = conn.wait_for_configure()                  # Configure dataclass
dev  = vp.GbmDevice()                             # render node, explicit like Rust's GbmEgl
buf  = vp.LinearBuffer(dev, cfg.width, cfg.height)  # ARGB8888 + LINEAR

pacer = vp.FramePacer.on_demand()                 # or .self_paced()
for ev in pacer.events(conn, timeout=30.0, extra_fds=[]):
    if ev.kind is vp.Event.RENDER:
        with buf.map() as (mem, stride):          # writable memoryview + map stride
            draw_with_cairo(mem, stride)          # zero-copy; or buf.upload(pil_img)
        conn.send_buffer(buf)
        pacer.submitted()
    elif ev.kind is vp.Event.RECONFIGURE:         # drain already handled by pacer
        buf = buf.resize_or_keep(dev, ev.configure)
        pacer.mark_dirty()
    elif ev.kind is vp.Event.TIMEOUT:             # periodic refresh hook
        pacer.mark_dirty()
    elif ev.kind is vp.Event.FD_READY:            # e.g. the D-Bus socket
        handle_dbus(ev.fd); pacer.mark_dirty()
    elif ev.kind is vp.Event.SHUTDOWN:
        break
```

The zero-copy path: `buf.map()` wraps the `gbm_bo_map` pointer in a
memoryview; `cairo.ImageSurface.create_for_data(mem, FORMAT_ARGB32, w,
h, stride)` draws Pango-shaped text straight into GPU-visible memory.
`upload(pil_image)` is the copying convenience for PIL users.

---

## PR A — codec + Connection (no GBM, fully testable)

`python/veiland_plugin.py` with the codec (all client/server messages,
field validation, reserved-caps fail-closed), `Connection`
(connect / hello / wait_for_configure / recv_event / send_buffer via
`send_fds`), and the `Configure` dataclass. Plus
`python/tests/test_codec.py`: pytest golden-byte vectors hand-derived
from `protocol.md` §3-7 (cross-check byte strings against
`veiland-protocol`'s Rust tests where they exist), round-trips, and the
fail-closed cases (reserved caps, short payloads, bad UTF-8). Dev shell
gains python3 + pytest.

**Verify.** `pytest python/tests` passes. No runtime component yet —
integration lands with PR B.

## PR B — GbmDevice + LinearBuffer + FramePacer + battery on the SDK — DONE

Landed: the ctypes GBM block (device open with the NixOS `libgbm.so.1`
fallback, LINEAR ARGB8888 alloc, a `map()` context manager, fd export),
`LinearBuffer` with the zero-copy `map()` plus the PIL premultiply/
`upload()` convenience, and `FramePacer` with the full released/have_cue/
dirty state machine, the reconfigure drain, and the select() fd/timeout
multiplexing. `python/examples/battery.py` is the rewrite on the SDK; the
hand-rolled original moved to `docs/examples/battery_nosdk.py` as the "no
SDK, just the protocol" reference (see "Loose ends"). No wire-codec change.

**Two traps found in implementation (both runtime-only, invisible to PR
A's tests) — worth knowing for any future script plugin:**
- **The plugin file needs the execute bit.** The host launches a plugin by
  `exec`ing its `binary` path, so a script plugin without `chmod +x` fails
  at spawn with `Permission denied (os error 13)` and an empty layer — the
  `#!/usr/bin/env python3` shebang alone is not enough. `Write`-created
  files are `0644`; `git` tracks the mode, so commit it `100755`.
- **`memoryview` over a ctypes buffer must be `.cast("B")` before slice
  assignment.** A memoryview of a `c_char`/`c_ubyte` array reports a
  non-byte format (`<c`), and `mem[a:b] = data` raises
  `NotImplementedError: unsupported format` on it. `LinearBuffer.map()`
  casts to `"B"` (unsigned byte) so `upload()`'s row copy works. Zero-copy
  — the cast only reinterprets the format.

**Verify (user runs, both boxes: Intel/Mesa + NVIDIA).**
- SDK battery widget renders identically to the hand-rolled baseline
  established in Step 0, on both boxes. *(Verified rendering on a real
  locked session 2026-07-19; card at top-left as before.)*
- Mid-lock resize (scale change or the hotplug repro configs) → widget
  reallocates and continues; no stall, no crash.
- `kill -9` the plugin → region falls back, locker unaffected. *(Observed:
  host logs the disconnect, reaps the child, session stays locked.)*
- Timer tick works: battery percentage refreshes without any host
  message (TIMEOUT event path).

## PR C — now-playing example + short README

`python/examples/nowplaying.py`, the flagship: pycairo + PangoCairo
drawing zero-copy via `map()` — album art, title/artist with real
shaping and end-ellipsization (long CJK titles are the test case),
progress bar. MPRIS over D-Bus with a pure-Python client lib (jeepney
or dbus-next — decide in-flight; example-only dependency), its socket
fed to the pacer via `extra_fds`, a ~1 s TIMEOUT tick for the progress
bar. Album art from `mpris:artUrl` (file:// URLs; decode via PIL,
example-only dep). No player running → a quiet "nothing playing" state.
Read-only by design (see settled decisions).

`python/README.md`: vendoring instructions (copy the file), the buffer
contract (premultiplied BGRA, stride, top-down), the ladder positioning
(when to use Python vs Rust vs veiland-shader), minimum Python version.
Short, per the docs-at-publish-time rule; PyPI is a packaging-time
task.

**Verify (user runs, both boxes).**
- Card tracks a real player (mpv/spotify): art, metadata, progress.
- Pause/resume reflected within a tick; track change updates art.
- A long Japanese title ellipsizes cleanly (Pango shaping proof).
- No player → placeholder state, no crash; player quits mid-lock →
  same.
- Journal clean of tracebacks throughout.

---

## Optional future (not scheduled): GPU-from-Python demo

The old plan's PR 2, kept here so the idea isn't lost. A second Python
plugin that renders with OpenGL into a GBM buffer with a fence fd — the
fast path, proving Python isn't limited to CPU. If ever done: prototype
raw ctypes EGL/GL vs an existing binding (moderngl / PyOpenGL) and pick
whichever is less painful; reference the Rust SDK's `submit_frame` /
`SyncFence` and protocol.md §5.1/§6.2 for the fence handshake. It gates
nothing and nothing gates it.

## Loose ends

- **`battery.py`'s permanent home — RESOLVED at PR B (2026-07-18).**
  Moved from the repo root to `docs/examples/battery_nosdk.py`
  (`git mv`, mode preserved), next to its `battery_python.toml`. It
  stays the "no SDK, just the protocol" reference; the SDK rewrite lives
  at `python/examples/battery.py`, and `battery_python.toml`'s `binary`
  points at the SDK version. (History: the file was TRACKED ON MASTER at
  the root via `6dfee2b`; an earlier note wrongly said it lived only on
  a branch.)
- **The `python-plugin-demo` branch (local + origin) is fully
  redundant**: its sole commit `a0d015a` is a smaller stale duplicate
  of master's `a30a1e6`, and every real demo commit (`6dfee2b`,
  `d982a3b`) is in master's history. Safe to delete both refs; nothing
  unique remains on them.
- **`plugin-python.md`** (`~/Downloads/`): the uncommitted Reddit-demo
  companion to battery.py, slightly stale vs. the script. Stays a
  Reddit draft unless deliberately committed; if it is, re-sync it
  then. Not required by any PR above.

## Out of scope

- GL-from-Python (see "Optional future" — no longer gating anything).
- PyPI / nixpkgs / distro packaging of the SDK (packaging time). See
  "Deferred: Python plugin distribution" below for what specifically
  waits until then.
- The Configure region/surface host fix (deferred until after the SDK;
  see "Open host issue" above).
- Pointer/click protocol message and interactive widgets (own design
  round; benefits all SDKs at once when it happens).
- A `run_plugin()` framework, threads, or an async variant.
- An shm buffer path (rejected; see settled decisions).
- QtQuick-from-Python (possible via the software-backend
  QQuickRenderControl grab, documented in the design discussion; stays
  community/demo territory).

## Deferred: Python plugin distribution (post-PR C, do NOT start now)

Today a Python plugin is wired in with an absolute `binary` path plus the
execute bit in its TOML (`battery_python.toml` points at
`python/examples/battery.py`). That is fine for an in-repo *example* but is
not how a shipped plugin should be referenced. Two separable follow-ups,
both explicitly held until after the SDK stabilizes (PR C done); neither
blocks tagging:

- **(a) SDK distribution** — already decided in settled decisions:
  vendoring the single `veiland_plugin.py` file stays the default, PyPI
  (`pip install veiland-plugin`) is additive at packaging time. No
  scaffolding before PR C.
- **(b) Host: resolve script plugins by name (optional).** A *host*
  feature, independent of SDK packaging: let a config say
  `binary = "veiland-battery"` and have veiland-core resolve it from a
  plugin search path, exactly as compiled `veiland-*` plugins already
  resolve on `PATH` today (the script plugins miss out only because they
  are not installed binaries). Removes the hardcoded absolute path. Can
  land any time as its own host change; does not gate or wait on (a).

Sequence: **PR C first** — its pycairo/PangoCairo now-playing example is
what actually proves the `map()` zero-copy path is drawing-library-
agnostic (battery only exercises the PIL `upload()` convenience). Revisit
distribution after, when two Python examples have made the absolute-path
friction concrete. Note the exec-bit + `.cast("B")` traps recorded in the
PR B section — packaging must not lose the execute bit.

## Why this order

- PR A first because the codec is the only part that is fully testable
  without a compositor, and golden-byte tests against the spec catch
  the embarrassing bugs (endianness, string encoding) before any GPU
  variable enters the picture.
- PR B is the end-to-end proof and the SDK's reason to exist — the
  line-count drop of battery.py is the acceptance test.
- PR C is the payoff piece: it exercises every deliberate design choice
  (zero-copy map, fd multiplexing, timeout ticks, library-agnosticism)
  in one example, and doubles as the demo for the "Python widgets" tier
  of the story.

Each PR verified on both dev boxes. The user runs builds/tests;
verification steps are written for the user to execute.

---

# Status-icon widgets via SVG (Python) — plan

Added 2026-07-19. Motivated by the reference lockscreen (Veila,
github.com/naurissteins/Veila): a small top-right status cluster —
battery, wifi strength, bluetooth, keyboard-layout badge. Veila draws
these from **SVG** icons (it is a web/Qt-style locker with an SVG
renderer on hand). We want the same visual result and, more
importantly, the same low authoring barrier: "anyone who can write an
`if/else` and load a file can make a status widget."

## The decision and why (settled 2026-07-19, do not re-litigate)

**All Python. No Rust. No change to either core SDK.**

The reasoning, walked through in the design discussion:

- **A status icon is a static image swapped by state.** The SVG itself
  never animates; the only dynamic part is an `if/else` choosing *which*
  file to draw (`battery-25/50/75/100.svg`, `battery-charging.svg`;
  `wifi-0/1/2/3.svg`). "Draw an unchanging image, occasionally" is the
  textbook CPU-widget-tier task.
- **Python already does this with zero new SDK surface.** The Python SDK
  is CPU-pixel-native: `buf.map()` yields a writable cairo-compatible
  memoryview, and librsvg renders an SVG *straight onto a cairo context*
  that writes into it. Nothing is added to `veiland_plugin.py` — the SVG
  helper is author-side code on top of `map()`, exactly like the cairo
  battery proved in `9e400fa`.
- **Rust would need new core surface for no benefit.** The Rust reference
  plugins render with OpenGL (`gl` + `veiland-text` glyph atlas); there is
  NO CPU-pixel / texture-from-pixels path in `veiland-plugin` (verified:
  `gl.rs` has no texture helper, `buffer.rs` only exposes
  `bind_for_rendering`). A Rust SVG widget would mean rasterize (resvg) →
  upload as a GL texture → textured quad, i.e. *adding a texture-upload
  helper to the Rust SDK*. That is real new SDK surface to do badly what
  the Python tier already does well. Rust stays the tier for GPU-heavy /
  animated content (raymarcher, particles), where it earns its keep.
- **Performance is a non-issue.** These render `on_demand()` — once at
  spawn, then a ~30 s timeout re-read, plus the rare bucket crossing:
  ~2 renders/minute, not 60/s. Each render is librsvg (C, sub-ms for a
  small icon) + a cairo blit (microseconds); Python never touches a pixel
  (it picks the file and orchestrates C libraries). The Python-CPU
  slowness that matters is 60fps full-surface redraws — the exact case
  that is Rust+GPU by design, and the exact opposite of a status icon.
  One-time cost: librsvg/PyGObject import at spawn (tens of ms, inside the
  handshake budget, invisible after).

**SVG never enters `veiland_plugin.py`.** The core SDK stays the single
vendorable stdlib+ctypes file (settled decision, load-bearing for
vendoring). SVG support is a *companion*, opt-in, carrying its own dep.

## Dependency

librsvg via PyGObject (`gi.repository.Rsvg`) — renders SVG directly onto a
cairo context, so it composites into `buf.map()` with no extra copy.
Example/companion-only; never an SDK dep. On NixOS the dev shell needs
`pkgs.librsvg` + gobject-introspection + the Rsvg typelib on
`GI_TYPELIB_PATH`.

**This is the same PyGObject stack PR C later needs for PangoCairo — and
that is now an argument for doing SVG FIRST, not after** (order revised
2026-07-19, see Sequence). PR C tangles PyGObject with D-Bus, album art,
and Pango all at once; if the finicky NixOS typelib wiring misbehaves you
would be debugging it *through* D-Bus logic. Landing the SVG battery first
sorts out "does `gi.repository.Rsvg` load, is the typelib on the path" as a
standalone problem under a trivial widget. PR C then inherits a known-good
PyGObject and only has to debug D-Bus. So the dep moves EARLIER on purpose;
it is a de-risking move, not a cost.

## Deliverables

- **`python/veiland_svg.py`** — the opt-in companion helper. Vendored
  *alongside* `veiland_plugin.py` by authors who want SVG (two files, not
  one; the core stays one-file). Surface (exact signatures decided
  in-flight): load an SVG (`load_svg(path) -> handle`, cached), and draw
  it onto a cairo context at a position/size
  (`draw_svg(cr, handle, x, y, w, h)`), scaling to fit. Zero-copy: it
  renders onto the caller's cairo context, which is already backed by
  `buf.map()`. Typed errors (missing file, parse failure), never a
  traceback — same standard as the SDK.
- **`python/examples/battery_svg.py`** — the worked example and the
  copy-me template: the `if/else` state swap into `draw_svg`, drawn as a
  small top-right pill matching the reference. This is what a non-expert
  author clones for wifi/bluetooth. Ships a small first-party icon set
  under `python/examples/icons/` (battery buckets + charging), CC0 /
  self-drawn so redistribution is clean.
- **`python/examples/wifi_svg.py`** (follow-on) — same pattern, signal
  buckets. Its data source is NetworkManager over D-Bus, so it reuses PR
  C's D-Bus-in-Python plumbing (`extra_fds` pacer integration). Do NOT
  invent D-Bus here before PR C settles the pattern.
- **`python/examples/bluetooth_svg.py`** (follow-on) — bluez over D-Bus;
  on/off + connected state. Same D-Bus-after-PR-C rule.
- Separate composable plugins (settled), each a small pill positioned via
  config — the label/clock model, à la carte in the user's TOML. NOT one
  `veiland-statusbar` mega-plugin.

## Sequence (revised 2026-07-19 — SVG battery FIRST, before PR C)

Original plan put PR C first "so PyGObject comes for free with Pango." That
was a weak reason: **battery-SVG needs nothing from PR C** (no D-Bus — plain
`/sys/class/power_supply` reads, already in `battery.py`; no Pango — a
bucketed icon has no shaped text). The SVG piece is also far smaller and
more certain than the whole D-Bus/MPRIS/art/Pango machine, and doing it
first de-risks the PyGObject dep in isolation (see Dependency). So:

1. **librsvg/PyGObject dev-shell wiring — DONE (`b818ff5`, 2026-07-19).**
   Added `pkgs.librsvg` + `pkgs.gobject-introspection` + `ps.pygobject3` +
   `GI_TYPELIB_PATH` + `librsvg`/`glib` on `LD_LIBRARY_PATH` to BOTH python
   envs (dev shell + CI check). Verified in the dev shell: the typelib
   resolves (librsvg-2.62.1) and a trivial SVG renders onto a real cairo
   surface (the smoke test asserts pixels, proving the pycairo<->Rsvg foreign
   bridge, not just `import Rsvg`). CI gi-wiring is parity only — the gate
   (ruff/mypy/pytest) never imports gi, so it stays green without it.
2. **`veiland_svg.py` helper + `battery_svg.py` + icon set — DONE (`5ff344b`,
   2026-07-19).** `veiland_svg` renders librsvg onto a caller-supplied cairo
   context (`load_svg`/`draw_svg`), plus two pure-cairo conveniences added
   in-flight so a status widget is two calls not twenty lines: `draw_pill`
   (translucent circular chip) + `draw_svg_centered`. Both take a `cr`, never
   a `LinearBuffer` — the helper stays decoupled from the SDK's buffer type
   (still primitives you drive, not a framework). `battery_svg.py` is the
   copy-me `if/else` battery, a small top-right pill; ships a self-drawn CC0
   icon set (`python/examples/icons/`, buckets + charging) + `battery_svg.toml`.
   **Runtime-verified on the NVIDIA box** on a real locked session (the harder
   box — exercises the TEXTURE_EXTERNAL_OES CPU-dmabuf path). Intel/Mesa
   confirmation still pending as of this note.
3. **PR C (now-playing)** — now inherits a known-good PyGObject; only D-Bus
   + Pango + album art remain to debug, not first-time typelib pain too.
4. **wifi + bluetooth examples** — after PR C, reusing its D-Bus plumbing
   and battery's pill style. Then the real 3-icon hero screenshot, all
   widgets live.

### Region-placement fix — LANDED 2026-07-19 (was: bug surfaced here)

The region-placement bug that surfaced while writing `battery_svg.py`
(the pill self-positioned with `buf.width - badge_gap` because a region
plugin got a full-surface buffer that the host then *stretched* into the
region quad) is FIXED — see "Host region-placement fix — LANDED" near the
top of this file for the full write-up. In short:

- **Layer 1 correctness** (Part A `0f56130`): Configure carries region
  dims via `region::configure_dims`; the stretch becomes an identity
  transform. `region = None` stays byte-identical.
- **Layer 1 ergonomics** (Part B `44f52ff`): anchored regions
  (`halign`/`valign` + fraction-of-surface `width`/`height`/`margin`),
  resolved per-output so one config works on 1080p/1440p/4K. NB the shape
  landed as two per-axis keywords `halign`/`valign` (hyprlock vocabulary,
  verified), NOT the single `anchor = "top-right"` token this section
  originally sketched — see [[project_anchor_region_fractional]] for why.
- **Layer 2** (rode with Part A): `battery_svg.py` centres the pill in its
  own region-sized buffer (`cx = buf.width/2`); `battery_svg.toml` uses the
  anchored form and is the first region-assigned Python example. Kept
  mode 100755.

Verified live on both boxes + multi-monitor + hotplug. Remaining: merge to
master. See [[project_region_placement_bug]].

## Explicitly out of scope / deferred

- **Rust status widgets / a Rust SVG path.** Not happening for this — it
  would need a texture-upload helper in `veiland-plugin` for no benefit
  over the Python tier. If GPU-drawn status content is ever wanted it is a
  separate, justified SDK change, decided on demand.
- **Keyboard-layout badge ("EN").** The odd one out: a plugin cannot see
  the layout today — no protocol message carries it, so this needs a
  *core* change (host forwards the active layout to plugins), its own
  small design round. Not a plugin-only task; tracked separately, blocks
  nothing above.
- **A config-driven zero-code `veiland-svg` plugin** (map value→file→bucket
  thresholds entirely in TOML). Tempting for pure non-coders, but it means
  inventing a config DSL for value→file mapping — real design that gets
  complex fast. Revisit only after the helper-based examples exist and we
  know which config knobs actually matter. The helper + `if/else` example
  is the honest first answer.
- **A rendered (vector-path) battery** instead of SVG-swap. We chose
  SVG-swap for the low authoring barrier; the cairo-drawn battery
  (`battery_cairo.py`) stays as the "draw it yourself" reference.

## Icon-strategy note (for the eventual README)

Two icon patterns to document, so authors pick the right one:
- **Dynamic value with continuous fill drawn by hand** (the cairo battery)
  — most control, no assets, but you write the geometry.
- **State-bucketed SVG swap** (this plan) — trivial `if/else`, needs an
  icon set + the `veiland_svg` companion + librsvg. The approachable path.
For a whole tray of varied *static* icons, an icon font (Nerd Fonts /
Material Symbols via cairo's font API) is a third option — cheap per icon,
but a font-file dependency and brittle codepoints; not pursued now.
