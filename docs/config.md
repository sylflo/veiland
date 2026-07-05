<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Veiland config file

This document is the source of truth for veiland-core's config file
(`config.toml`). The Rust types in `veiland-core/src/config.rs` are
*an* implementation; if the code and this document disagree, the
document wins and the code is a bug.

## 1. Location

veiland-core looks for its config in this order:

1. `$VEILAND_CONFIG` if set — the full path to a config file. Intended
   for development and testing; lets you switch configs without
   touching `~/.config/`.
2. `$XDG_CONFIG_HOME/veiland/config.toml` if `$XDG_CONFIG_HOME` is set.
3. `$HOME/.config/veiland/config.toml` otherwise.

If none of `$VEILAND_CONFIG`, `$XDG_CONFIG_HOME`, and `$HOME` are
set, veiland-core refuses to start with `no config-file location
available`.

## 2. What happens when the file is missing or broken

- **Missing file**: not fatal. veiland-core logs a warning, runs with
  zero plugins, and shows only the lock-surface clear color. You can
  still unlock; you just have no compositing layers. This is so a
  fresh install (or a wiped `~/.config/`) doesn't refuse to lock.
- **Malformed TOML**: fatal. veiland-core logs the parse error
  (with line and column from the `toml` crate) and exits with
  failure.
- **Schema mismatch** (e.g. `z_index = "high"` instead of an
  integer, missing required field): fatal. Same shape as malformed
  TOML — logged, refused to start.
- **Schema valid but post-validation fails** (empty plugin name,
  duplicate plugin name): fatal. Logged with the offending entry,
  refused to start.

The threshold for "fatal" is "the user wrote something and we can't
make sense of it." A missing file means "the user didn't write
anything," which is a different state.

## 3. Schema

The file is a TOML document with one or more `[[plugin]]` entries.
Each entry declares one plugin to spawn at startup.

```toml
[[plugin]]
name = "wallpaper"                # required
binary = "/usr/bin/veiland-wallpaper"  # required
z_index = 0                       # required
region = { x = 0, y = 0, w = 1920, h = 1080 }  # optional

[[plugin.config]]                 # optional, plugin-specific
# arbitrary keys here, passed through to the plugin
image = "/home/alice/Pictures/wallpaper.jpg"
```

### `name` (string, required)

Used in host log lines and to disambiguate `[[plugin]]` entries.
Must be non-empty and unique within the config.

This name is *not* the binary's filename and *not* the plugin's
self-declared `Hello` name. It's the user's chosen label for "this
plugin instance in my config." If you have two clock plugins in
different timezones, you'd use names like `clock-paris` and
`clock-tokyo` to tell them apart in logs even though they share a
binary.

### `binary` (string, required)

The plugin's executable. Two forms:

- **A bare name** (no `/`, e.g. `veiland-clock`) is resolved by the
  core: first beside the locker itself (the directory `veiland` was
  installed into), then by searching `$PATH`. This is the portable,
  copy-paste form — it works regardless of whether your distro installs
  to `/usr/bin` or, on NixOS, a `/nix/store/.../bin` directory, because
  the reference plugins always ship in the same directory as `veiland`.

- **A path containing a `/`** (absolute `/usr/bin/veiland-clock`, or
  relative `target/debug/veiland-clock`) is used verbatim — no lookup.
  Use this to point at a specific build, e.g. a `target/debug` binary
  while developing.

There is no shell involved and no tilde (`~`) expansion in either form.
Resolution happens in the core before spawning; whichever file is chosen
is then invoked with `execv` directly.

If the binary can't be resolved, doesn't exist, or isn't executable, the
spawn fails at runtime. veiland-core logs the failure and continues with
the other plugins; the failed plugin's region falls back to the
lock-surface clear color (black).

### `z_index` (integer, required)

The plugin's depth in the composite order. **Lower = drawn first =
visually behind.** A plugin at `z_index = 0` sits behind a plugin at
`z_index = 10`; the latter covers the former wherever their regions
overlap.

Ties (two plugins at the same `z_index`) are broken by config-file
order — the entry that appears first is drawn first, and so sits
behind. The sort is stable; this rule is reliable.

Negative values are valid and useful. A wallpaper at `z_index =
-100` is unambiguously "always behind everything," even if a future
plugin gets a tiny positive z_index.

### `region` (table, optional)

The screen rectangle this plugin draws into. Pixel coordinates,
relative to the lock surface's top-left.

```toml
region = { x = 100, y = 200, w = 400, h = 80 }
```

- `x`, `y` (integers): top-left corner. `(0, 0)` is the lock
  surface's top-left.
- `w`, `h` (positive integers): width and height in pixels.

If `region` is omitted, the plugin draws to the entire lock
surface.

Regions are *not* validated against the surface size at load time
(the surface size isn't known yet). Off-screen or oversized regions
are clipped by GL at render time — they don't crash, but their
off-screen pixels are wasted GPU work. Coordinates with absolute
value greater than 8192 trigger a "this looks like a typo" warning
at config load; it's a soft check, not a rejection.

The plugin's own buffer size and the region size are independent.
A plugin can render into a 64×64 dmabuf and have the host scale
it across a 400×80 region (or vice versa). What the plugin
*knows* about its region — via the `Configure` message — is
currently the full lock surface; that mismatch is tracked work for
a future milestone.

### `monitors` (array of strings, optional)

The Wayland outputs this plugin runs on, named by their
`xdg_output.name` strings (e.g. `"DP-1"`, `"HDMI-A-1"`, `"eDP-1"`).
Look them up with `hyprctl monitors` (Hyprland) or
`swaymsg -t get_outputs` (Sway).

```toml
monitors = ["DP-1", "HDMI-A-1"]
```

If the field is **absent**, the plugin runs on every connected output
— one independent plugin process per output. If the field is
**present**, only the outputs whose names appear in the list spawn
an instance.

Rules:

- **Case-sensitive, exact match.** `"DP-1"` is not the same as
  `"dp-1"`. The compositor's exact spelling wins.
- **Empty list is rejected at config load** with an error message.
  An empty list is ambiguous (did you mean "no outputs"? then why
  declare it?); omit the field instead if you want all-outputs
  behaviour.
- **Unknown names log a warning at spawn time and produce zero
  instances of that plugin** — they do not fail the locker start.
  A typo in `monitors` shouldn't lock you out of your machine.
- **Output identity comes from the plugin protocol's `Configure`
  message** (see `docs/protocol.md` §7.1). A plugin's per-output
  instance learns which output it's serving via `Configure.output_name`
  and can vary its rendering accordingly (different wallpaper per
  screen, different timezone clock per screen, etc.).

### `[password]` (table, optional)

Controls the password field that appears as the user types: a rounded
input **box** with one filled dot per buffered character centred inside
it. The whole field is painted by the core on top of any plugins (soft
trust-region — see `CLAUDE.md` §"Threat model" for the threat-model discussion);
plugins never see keystrokes or the character count, so the field's
appearance is the only thing config can touch.

Every field is optional; missing `[password]` table → all defaults,
which render the box-and-dots field shown below.

```toml
[password]
# Position (the box, and the dots auto-centre inside it)
x = 960            # surface px; omit → centred per surface
y_percent = 75     # % of surface height (0..=100); omit → 75

# The input box
show_box = true             # draw the box at all; false → bare dots
box_width = 400             # surface px; clamped [1, 8192]
box_height = 90             # surface px; clamped [1, 8192]
outline_thickness = 2       # surface px; clamped [0, box_height/2]
rounding = -1               # corner radius px; -1 = full pill
inner_color = "rgba(34, 41, 56, 0.55)"    # box fill
outer_color = "rgba(180, 190, 210, 0.55)" # box outline

# The dots
dot_diameter = 12                          # surface px; clamped [1, 100]
dot_spacing = 20                           # centre-to-centre px; clamped [1, 200]
max_dots = 32                              # row caps here; clamped [1, 256]
dot_color = "rgba(220, 220, 220, 1.0)"     # dot fill

# The placeholder (shown centred in the box when nothing is typed)
placeholder_text = "Enter to remember..."  # "" disables it
placeholder_color = "rgba(200, 205, 215, 0.6)"
placeholder_font_family = "Sans"           # CSS-style family name
placeholder_font_size = 18                 # surface px; clamped [1, 512]
```

**Colours** use a CSS-style `rgba(r, g, b, a)` string: `r`/`g`/`b`
are integers `0..=255`, `a` is a float `0.0..=1.0`. `rgb(r, g, b)`
(alpha implied `1.0`) is also accepted. Out-of-range channels are a
load error (a typo worth surfacing); alpha outside `0..=1` is clamped.
Gradients are not supported — one flat colour per field.

Position:

- **`x`** (integer, optional). Horizontal centre of the field in
  surface-pixel coordinates. If omitted, the field is centred on each
  surface's `width / 2` — different absolute pixel positions on
  different-width monitors, same surface-relative position. No clamp:
  values that put the field off-screen are user error but not unsafe.
- **`y_percent`** (integer, optional, default `75`). Vertical position
  as a percentage of surface height. `0` is the top edge; `100` is
  the bottom. Clamped to `[0, 100]` at load time with a warning if
  out of range — out-of-range values don't fail the locker start.

The box:

- **`show_box`** (bool, optional, default `true`). Draw the input box.
  Set `false` for the pre-box look: bare dots floating on the
  wallpaper, positioned directly by `x`/`y_percent`. When `true`, the
  dots auto-centre inside the box, so `x`/`y_percent` position the box
  and the dots follow.
- **`box_width`** / **`box_height`** (integer, optional, defaults `400`
  / `90`). Box size in surface pixels. Each clamped to `[1, 8192]`.
- **`outline_thickness`** (integer, optional, default `2`). Outline
  width in surface pixels. `0` draws fill only (no outline). Clamped to
  `[0, box_height/2]` — a thicker outline would consume the box.
- **`rounding`** (integer, optional, default `-1`). Corner radius in
  surface pixels. The sentinel `-1` means a **full pill** (radius =
  `box_height / 2`, fully rounded ends — the default look). Any other
  value is clamped to `[0, min(box_width, box_height) / 2]`; `0` is a
  sharp rectangle.
- **`inner_color`** (colour, optional, default `rgba(34, 41, 56, 0.55)`).
  Box fill. A translucent fill lets the wallpaper show through.
- **`outer_color`** (colour, optional, default
  `rgba(180, 190, 210, 0.55)`). Box outline.

The dots:

- **`dot_diameter`** (integer, optional, default `12`). Diameter of
  each dot in surface pixels. Clamped to `[1, 100]`.
- **`dot_spacing`** (integer, optional, default `20`). **Centre-to-
  centre** stride between consecutive dots in surface pixels — not
  the gap between edges. With diameter 12, the default leaves an
  8-px gap. Clamped to `[1, 200]`.
- **`max_dots`** (integer, optional, default `32`). Cap on the number
  of visible dots. Beyond this, the row freezes — the user keeps
  typing (the password buffer keeps filling) but the indicator stops
  growing. Clamped to `[1, 256]`. Keeps the row from overflowing the
  box on long passwords.
- **`dot_color`** (colour, optional, default `rgba(220, 220, 220, 1.0)`).
  Dot fill.

The placeholder:

- **`placeholder_text`** (string, optional, default
  `"Enter to remember..."`). Shown centred in the box before anything
  is typed; the dots replace it on the first keystroke. Set to `""`
  to disable it (the box stays empty). Rendered by the core via
  `veiland-text` — this is the one piece of text the trusted core draws
  itself, which pulls a font stack (cosmic-text + fontdb) into the core
  process. fontdb scans the system fonts at startup (a few tens of ms).
  Low-risk (fonts are static data, parsed by a library, never executed),
  but it is more attack surface than a text-free core; disable the
  placeholder if you'd rather the core touch no fonts.
- **`placeholder_color`** (colour, optional, default
  `rgba(200, 205, 215, 0.6)`). A dim translucent grey reads as a hint
  rather than a typed value.
- **`placeholder_font_family`** (string, optional, default `"Sans"`).
  CSS-style family name (e.g. `"Liberation Sans"`); falls back to the
  system sans-serif if the name doesn't resolve.
- **`placeholder_font_size`** (integer, optional, default `18`).
  Surface pixels. Clamped to `[1, 512]`. Not scaled by output DPI yet
  (same as the box dimensions).

**Not yet configurable** (v2+): fade-on-empty, the authentication-state
colour flashes (`check`/`fail`/`capslock`/`numlock`), gradient colours,
per-monitor positioning, and scale-factor support. The same `[password]`
config applies to every monitor's lock surface.

### `[plugin.config]` (table, optional)

A pass-through table for plugin-specific settings. At spawn time
the host serialises the table to JSON and exports it to the plugin
process as `VEILAND_PLUGIN_CONFIG`. Plugins parse it however they
like — `serde_json` is the obvious choice. The schema is the
plugin's own concern; veiland-core does not interpret the
contents.

Plugins that do not declare a `[plugin.config]` table see
`VEILAND_PLUGIN_CONFIG` unset and should fall back to whatever
defaults they document.

## 4. Worked examples

### One plugin filling the screen

```toml
[[plugin]]
name = "gradient"
binary = "/usr/bin/veiland-gradient"
z_index = 0
# region omitted => full lock surface
```

### Two plugins, side by side

```toml
[[plugin]]
name = "gradient-left"
binary = "/usr/bin/veiland-gradient"
z_index = 0
region = { x = 0, y = 0, w = 960, h = 1080 }

[[plugin]]
name = "gradient-right"
binary = "/usr/bin/veiland-gradient"
z_index = 0
region = { x = 960, y = 0, w = 960, h = 1080 }
```

Same binary, two instances, different regions.

### Wallpaper, clock, password indicator

```toml
[[plugin]]
name = "wallpaper"
binary = "/usr/bin/veiland-wallpaper"
z_index = -100   # always behind everything
# full screen

[[plugin]]
name = "clock"
binary = "/usr/bin/veiland-clock"
z_index = 10
region = { x = 100, y = 100, w = 300, h = 80 }

[[plugin]]
name = "password-prompt"
binary = "/usr/bin/veiland-password-prompt"
z_index = 20
region = { x = 660, y = 500, w = 600, h = 80 }
```

(Those exact plugin names are illustrative — see the reference plugins
for the real binaries.)

### A test fixture for the compositor

See `docs/examples/boxes.toml` for the staircase of overlapping
red/blue/green test plugins. That
fixture exercises region clipping, z-index ordering, and alpha
blending end-to-end.

### Password field overrides

Defaults are usually what you want; the table exists for when
they're not. A minimal repositioning:

```toml
[password]
y_percent = 50    # halfway down instead of 75%
dot_diameter = 16 # larger dots
```

The bare-dots look (no box), matching the pre-box behaviour:

```toml
[password]
show_box = false
```

Recreating the reference "Your Name" mockup — a dark translucent pill
with a faint light outline and white dots, sitting low on the scene:

```toml
[password]
y_percent = 72
box_width = 440
box_height = 70
outline_thickness = 2
rounding = -1                              # full pill
inner_color = "rgba(28, 36, 52, 0.45)"
outer_color = "rgba(150, 170, 200, 0.45)"
dot_color = "rgba(235, 238, 245, 1.0)"
dot_diameter = 10
dot_spacing = 22
```

A live fixture exercising the field over a plugin (the soft
trust-region paint order) is at `docs/examples/password.toml`.

## 5. Multi-monitor

Each `[[plugin]]` entry produces **one independent plugin process
per matching output**. A plugin with `monitors = ["DP-1"]` runs only
on DP-1. A plugin that omits the `monitors` field runs on every
connected output, with each output getting its own process — the
processes are independent and don't share state. This is the right
shape for wallpapers that should differ per screen, clocks that
should show different timezones per screen, and so on.

The plugin learns which output it's serving via the `output_name`
field on `Configure` (see `docs/protocol.md` §7.1). Plugins that
need to vary their rendering per monitor key off that string.

### Regions are per-output, in surface pixel coordinates

A region of `(100, 100, 300, 80)` lands at the same absolute pixel
position — top-left at `(100, 100)` — on each output the plugin runs
on. On a 4K monitor that looks small; on a 1080p monitor it looks
normal. The host does not currently translate regions to
output-aware coordinates ("anchor top-right, 20% of output width");
that's a planned extension.

If you want a clock in the top-left of each monitor at sizes that
make sense for each monitor's resolution, write one `[[plugin]]`
entry per monitor with the appropriate region and a `monitors`
selector naming that one output.

### Process count

A config with N `[[plugin]]` entries and M connected outputs
produces up to N × M plugin processes (fewer if some entries use
`monitors` to narrow). This is the honest cost of per-output
isolation. For plugins that are cheap to run (a static icon, a
clock), running M instances costs almost nothing. For plugins
where per-output is genuinely wasteful, use a `monitors` selector
naming a single output.

### Hot-plugging a monitor

When a monitor is connected mid-lock, the host spawns plugin
instances for it (one per matching entry, same rules as startup).
When a monitor is disconnected, its plugin instances are torn down
cleanly. Plugins are fresh processes on each hotplug-in; they do
not carry state across plug/unplug cycles. If a plugin needs
state continuity (a wallpaper that remembers its zoom level, say),
persist it externally (a file under `$XDG_RUNTIME_DIR`, e.g.).

## 6. Things that aren't configurable

By design:

- **The lock-surface clear color.** Black. Plugins compose on top.
- **Whether to load PAM.** Always yes; PAM is the only auth path.
- **The PAM service name.** Hardcoded as `veiland`. Your
  `/etc/pam.d/veiland` file controls the actual auth chain.
- **Which fence-sync extension to use.** Detected at startup; fast
  path if `EGL_ANDROID_native_fence_sync` is available, slow path
  otherwise.
- **The password field's animation and auth-state feedback.**
  Position, sizing, the box (fill/outline/rounding), the placeholder
  text, and all the colours are configurable via `[password]` (see §3).
  What's *not* configurable: animation, the failure-flash / capslock /
  numlock colour changes, and gradient colours. Those are deferred.

## 7. Pitfalls

- **Tilde expansion doesn't happen.** `binary = "~/bin/foo"` is a
  literal path containing a `~`; veiland will try to spawn it and
  fail. Use absolute paths.
- **Environment variables don't expand.** `binary = "$HOME/bin/foo"`
  is a literal path with a `$` in it. Same fix: absolute paths.
- **Empty `name` or duplicate `name`** are rejected at load time.
  The error message names the offending entry; check the index
  (`[[plugin]] #2`) against your file.
- **Three plugins each at 60Hz** is currently the soak-test
  workload. The locker handles it cleanly; nothing prevents you
  from declaring more, but the host's frame pacing isn't yet
  rate-limited and very high plugin counts may eat CPU.
- **A `monitors` name that doesn't match any connected output**
  produces zero instances of that plugin and a warning at startup;
  the locker still runs. Check `hyprctl monitors` (Hyprland) or
  `swaymsg -t get_outputs` (Sway) for the exact names your
  compositor uses. Names are case-sensitive (`DP-1`, not `dp-1`).
- **A multi-monitor setup with N plugins** produces up to N × M
  child processes (M = output count). `pgrep -af veiland` shows
  them all; this is per-output isolation by design, not a leak.
- **`[password]` `dot_spacing` is centre-to-centre stride, not the
  gap between edges.** With `dot_diameter = 12` and `dot_spacing =
  8` you get *overlapping* dots, not 8-px-wide gaps. Default 20
  with diameter 12 yields the visually-natural 8-px edge gap.
- **`[password]` `x` defaults to "centre of this surface", not a
  fixed pixel.** On a multi-monitor setup with different-width
  outputs, an absent `x` means each monitor's row sits at its own
  centre. Set `x` explicitly if you want the same absolute pixel
  position on every monitor (note: the row may be off-centre or
  off-screen on monitors with widths below `2 * x`).

## 8. See also

- `docs/protocol.md` — the plugin ↔ host wire protocol. Read if
  you're writing a plugin.
- `docs/examples/boxes.toml` — a test fixture showing a
  three-plugin overlapping-region setup.
