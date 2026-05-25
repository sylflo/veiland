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

The path to the plugin's executable. veiland-core invokes
`execv(binary, ...)` directly — no shell, no `$PATH` lookup, no
tilde expansion. Write the full path.

If the binary doesn't exist or isn't executable, the spawn fails at
runtime. veiland-core logs the failure and continues with the other
plugins; the failed plugin's region falls back to the lock-surface
clear color (black).

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

Controls the password-indicator dots that appear as the user types.
The indicator is one filled grey circle per buffered character,
painted by the core on top of any plugins (soft trust-region — see
`docs/m9-plan.md` Q1 for the threat-model discussion).

Every field is optional; missing `[password]` table → all defaults.

```toml
[password]
x = 960          # surface px; omit → centred per surface
y_percent = 75   # % of surface height (0..=100); omit → 75
dot_diameter = 12   # surface px; clamped [1, 100]
dot_spacing = 20    # surface px, centre-to-centre stride; clamped [1, 200]
max_dots = 32       # row caps here; clamped [1, 256]
```

- **`x`** (integer, optional). Horizontal centre of the dot row in
  surface-pixel coordinates. If omitted, the row is centred on each
  surface's `width / 2` — different absolute pixel positions on
  different-width monitors, same surface-relative position. No clamp:
  values that put the row off-screen are user error but not unsafe.
- **`y_percent`** (integer, optional, default `75`). Vertical position
  as a percentage of surface height. `0` is the top edge; `100` is
  the bottom. Clamped to `[0, 100]` at load time with a warning if
  out of range — out-of-range values don't fail the locker start.
- **`dot_diameter`** (integer, optional, default `12`). Diameter of
  each dot in surface pixels. Clamped to `[1, 100]`.
- **`dot_spacing`** (integer, optional, default `20`). **Centre-to-
  centre** stride between consecutive dots in surface pixels — not
  the gap between edges. With diameter 12, the default leaves an
  8-px gap. Clamped to `[1, 200]`.
- **`max_dots`** (integer, optional, default `32`). Cap on the number
  of visible dots. Beyond this, the row freezes — the user keeps
  typing (the password buffer keeps filling) but the indicator stops
  growing. Clamped to `[1, 256]`. Keeps the row from running off the
  screen on long passwords.

Colours, animations, failure-flash, per-monitor positioning, and
scale-factor support are **not** configurable in v1; all tracked
for M11+ in `docs/m9-plan.md`'s "Deferred to post-M9" section.

The same `[password]` config applies to every monitor's lock
surface. If you need DP-1 to show its dots top-left while HDMI-A-1
shows them centred, that's M11+.

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

(None of those plugins exist yet; they'll arrive in M7+.)

### A test fixture for the M6 compositor

See `docs/examples/m6-boxes.toml` for the staircase of overlapping
red/blue/green test plugins used during M6 development. That
fixture exercises region clipping, z-index ordering, and alpha
blending end-to-end.

### Password indicator overrides

Defaults are usually what you want; the table exists for when
they're not. A minimal repositioning:

```toml
[password]
y_percent = 50    # halfway down instead of 75%
dot_diameter = 16 # larger dots
```

A no-op example (all defaults made explicit), useful as a starting
point to tweak:

```toml
[password]
# x omitted = centred per surface (different absolute pixel on
# different-width monitors, same surface-relative position)
y_percent = 75
dot_diameter = 12
dot_spacing = 20
max_dots = 32
```

A live fixture exercising the indicator over a plugin (the soft
trust-region paint order) is at `docs/examples/m9-password.toml`.

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
- **The password-indicator colour, shape, and animation.** Hardcoded
  to filled light-grey circles (`RGB(220, 220, 220)`), no animation,
  no failure-flash. Position and sizing are configurable via
  `[password]` (see §3); the rest is deferred to M11+.

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
- `docs/examples/m6-boxes.toml` — the M6 test fixture, showing a
  three-plugin overlapping-region setup.
