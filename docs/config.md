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

### `[plugin.config]` (table, optional, schema-only in M6)

A pass-through table for plugin-specific settings. Veiland-core
parses this so configs that include such tables don't break, but
it does *not yet* serialize or export the contents — plugins
cannot read them.

When a real plugin needs this (the M7 clock plugin will want a
timezone string), the host will serialize the table to JSON and
hand it to the plugin via the `VEILAND_PLUGIN_CONFIG` environment
variable. Writing `[plugin.config]` tables in your config now is
safe — they'll be ignored, but they won't break anything, and
they'll become live when the plugin side catches up.

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

## 5. Multi-monitor

In the current implementation, **every plugin runs on every
output**. A `[[plugin]]` entry produces one plugin process whose
single texture is composited onto every lock surface (one per
output). Regions are in lock-surface pixel coordinates, not
output-aware coordinates.

This has two consequences worth flagging:

- On a multi-monitor setup with different resolutions, a region of
  `(100, 100, 300, 80)` lands at the same absolute pixel position
  on each output — which looks small on a 4K monitor and normal on
  a 1080p one.
- A wallpaper plugin can't differ per output.

These are known limitations. A future milestone will add per-output
plugin instances with output-aware regions. When that lands, configs
will gain an optional `monitors = ["DP-1", "DP-2"]` field per
plugin entry, naming Wayland outputs.

**Forward-compatibility commitment**: when `monitors` is introduced,
the *absence* of the field will mean "all outputs" — the same
behaviour as today. Configs written for the current implementation
will keep working unchanged after that milestone lands.

## 6. Things that aren't configurable

By design:

- **The lock-surface clear color.** Black. Plugins compose on top.
- **Whether to load PAM.** Always yes; PAM is the only auth path.
- **The PAM service name.** Hardcoded as `veiland`. Your
  `/etc/pam.d/veiland` file controls the actual auth chain.
- **Which fence-sync extension to use.** Detected at startup; fast
  path if `EGL_ANDROID_native_fence_sync` is available, slow path
  otherwise.

Things that *will* become configurable in future milestones but
aren't yet:

- Per-output plugin instances (the `monitors = [...]` field
  described in §5).
- Per-plugin custom settings (`[plugin.config]` is parsed but not
  yet read by plugins — see §3).

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

## 8. See also

- `docs/protocol.md` — the plugin ↔ host wire protocol. Read if
  you're writing a plugin.
- `docs/examples/m6-boxes.toml` — the M6 test fixture, showing a
  three-plugin overlapping-region setup.
