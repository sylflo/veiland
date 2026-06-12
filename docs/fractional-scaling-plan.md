<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Fractional scaling (`wp_fractional_scale_v1`) — implementation plan

## The problem

`wl_output.scale` is an integer (1, 2, 3). Compositors running at fractional
scale (1.25×, 1.5×, 2.5× — common on HiDPI laptops) report `scale = 1` on
that legacy path. Veiland takes that `1` literally: it spawns a buffer whose
pixel dimensions match the logical surface size (e.g. 1706×960 on a 1.5× 
2560×1440 panel), and the compositor upscales the result. Text and UI look
visibly blurry.

`wp_fractional_scale_v1` is a separate Wayland protocol that delivers the
true scale as a rational number (`numerator / 120`, so 1.5× = 180/120). The
fix is to bind this protocol and use its value when available, falling back to
`wl_output.scale` when it isn't.

## How it works in Wayland

- The global `wp_fractional_scale_manager_v1` is advertised in the registry.
- For each lock surface's `wl_surface`, attach a `wp_fractional_scale_v1`
  object via `get_fractional_scale(wl_surface)`.
- The compositor sends `preferred_scale(u32 scale120)` events on that object,
  where the actual scale = `scale120 / 120.0`. For integer scales this is
  120 (1×), 240 (2×), etc.
- The event arrives asynchronously; the first one typically arrives around the
  same time as the `ext_session_lock_surface_v1.configure` event.

## What does NOT change

- **Plugin behaviour.** Plugins already receive physical pixel dimensions in
  `region_w`/`region_h` and size everything as fractions of those. A plugin
  that runs correctly at 3840×2160 (integer 2× on 4K) will run correctly at
  3840×2160 (fractional 1.5× on a different panel). No plugin code changes.
- **The `Configure.scale` field semantics.** It exists for converting
  *internal* logical values (font sizes, shadow radii) into physical pixels.
  Plugins that use `scale` for that purpose (currently none of the reference
  plugins — they all use surface-height fractions) would need updating too,
  but since none do, the field update is mechanical and non-breaking in
  practice.
- **The wire format structure.** `Configure.scale` stays `u32`. We change its
  value range (from `1..=3` to `1..=9999`, representing 120ths of the scale)
  and rename the field to `scale_120` to make the unit explicit. This IS a
  protocol breaking change and requires a protocol version bump — see below.

## Protocol versioning decision

`protocol.md` §10 says "fields are never appended in place; introduce a new
variant or bump the version." The cleanest approach here:

- In `Configure`, rename `scale: u32` → `scale_120: u32` with range `1..=9999`
  (120 = 1×, 240 = 2×, 180 = 1.5×; 9999/120 ≈ 83× is implausibly large but
  safe as an upper bound).
- The version handshake already handles this: a v1 plugin connecting to a v2
  host sees an unrecognised server version and closes the socket. All reference
  plugins ship with the host in this repo so they all bump together.
- Update the `configure_wire_format` test (the hardcoded byte sequence stays
  identical except the `scale` field now encodes `120` = `0x78 0x00 0x00 0x00`
  for a 1× output — same as before since `1` in v1 becomes `120` in v2 for 1×).

**Transition in the host:** wherever the core currently reads `wl_output.scale`
and passes it as `scale: 1/2/3`, it will pass `scale_120: 120/240/360`. When
a fractional scale event arrives, it passes `scale_120: <compositor value>`.
Plugins convert back: `physical_px = logical_px * (scale_120 as f32 / 120.0)`.
The reference plugins don't use `scale` for anything today, so the conversion
is in their log line only — add it there for completeness.

---

## Files touched

| File | Change |
|---|---|
| `veiland-protocol/src/server.rs` | Rename `scale` → `scale_120`, update range to `1..=9999`, update tests |
| `veiland-core/src/app/mod.rs` | Store fractional scale on `LockSurface`; update `spawn_plugins_for_output` and all `Configure` construction sites to use `scale_120` |
| `veiland-core/src/app/output.rs` or `app/mod.rs` | Bind `wp_fractional_scale_manager_v1`; attach per-surface `wp_fractional_scale_v1`; handle `preferred_scale` events |
| `plugins/clock/src/main.rs` | Update log line that prints `scale`; no render change needed |
| `plugins/label/src/main.rs` | Same — log line only |
| `docs/protocol.md` | Document `scale_120` field, new version, fractional scale encoding |
| `docs/plugin-api.md` | Update HiDPI section |

---

## Step 1 — Protocol change: `scale` → `scale_120` (`veiland-protocol`)

### `server.rs` — `Configure` struct

```rust
/// Output scale as 120ths (matches `wp_fractional_scale_v1`'s encoding).
/// 120 = 1×, 240 = 2×, 180 = 1.5×. Use `scale_120 as f32 / 120.0` to get
/// the float multiplier. Range: 1..=9999 (0.0083× to 83×; implausibly
/// large values are clamped by the host before sending).
pub scale_120: u32,
```

### Encoder: `write_u32_le(out, self.scale_120);` (unchanged position in stream)

### Decoder: accept range `1..=9999`:

```rust
let (scale_120, buf) = read_u32_le(buf)?;
if !(1..=9999).contains(&scale_120) {
    return Err(ProtocolError::OutOfRange);
}
```

### Protocol version constant

No bump needed — not in production yet. The rename is a breaking wire change
but all consumers (core + reference plugins) live in the same repo and are
updated together.

### Tests to update

- `configure_wire_format`: the `scale` byte sequence `0x01, 0x00, 0x00, 0x00`
  becomes `0x78, 0x00, 0x00, 0x00` (120 = 0x78) and `valid_configure()` sets
  `scale_120: 120`.
- `configure_scale_zero_rejected`: set `scale_120 = 0`, still rejected.
- `configure_scale_too_large_rejected`: set `scale_120 = 10000`, rejected.
- `configure_values_at_max_edge_accepted`: set `scale_120 = 9999`, accepted.
- Add `configure_scale_fractional_accepted`: set `scale_120 = 150` (1.25×),
  round-trips correctly.

---

## Step 2 — Bind `wp_fractional_scale_manager_v1` in `AppData`

### `AppData` struct (`main.rs`)

Add:

```rust
fractional_scale_manager: Option<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1>,
```

`Option` because the compositor may not advertise this global (e.g. older
compositors). When `None`, we fall back to `wl_output.scale * 120`.

### Registry handler

SCTK's `delegate_noop!` is the simplest binding for globals we just want to
hold a reference to. Alternatively, handle `global` in the `RegistryHandler`
impl manually. The simplest path: after `registry_queue_init`, do a manual
bind:

```rust
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1;

let fractional_scale_manager = globals
    .bind::<wp_fractional_scale_manager_v1::WpFractionalScaleManagerV1, _, _>(
        &qh, 1..=1, ()
    )
    .ok(); // None if compositor doesn't support it
```

Initialise `AppData.fractional_scale_manager` with this.

---

## Step 3 — Per-surface fractional scale tracking

### `LockSurface` struct (`app/mod.rs`)

Add:

```rust
/// Scale as 120ths, sourced from `wp_fractional_scale_v1.preferred_scale`
/// when available, or `wl_output.scale * 120` as fallback. Updated
/// asynchronously; the first value is set during surface creation using
/// whatever integer scale is known at that point.
pub(crate) scale_120: u32,

/// The `wp_fractional_scale_v1` object attached to this surface's
/// `wl_surface`. `None` when `wp_fractional_scale_manager_v1` was not
/// advertised by the compositor.
pub(crate) fractional_scale:
    Option<wp_fractional_scale_v1::WpFractionalScaleV1>,
```

### `create_lock_surface_for_output` (`app/mod.rs`)

After creating the `SessionLockSurface`, attach the fractional scale object if
the manager is available:

```rust
let fractional_scale = self.fractional_scale_manager.as_ref().map(|mgr| {
    mgr.get_fractional_scale(lock_surface.wl_surface(), &self.qh, ())
});
```

Initialise `scale_120` to `wl_output.scale * 120` (the same integer value the
core used before, now in 120ths) so the initial Configure sent at spawn time
is valid even before the `preferred_scale` event arrives.

### `preferred_scale` event handler

Implement `Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, ()> for AppData`:

```rust
fn event(
    state: &mut AppData,
    proxy: &wp_fractional_scale_v1::WpFractionalScaleV1,
    event: wp_fractional_scale_v1::Event,
    _: &(),
    _: &Connection,
    _: &QueueHandle<AppData>,
) {
    if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
        // Find the LockSurface that owns this fractional_scale object.
        let Some(entry) = state.lock_surfaces.iter_mut()
            .flatten()
            .find(|s| s.fractional_scale.as_ref()
                .map(|fs| fs == proxy)
                .unwrap_or(false))
        else {
            return;
        };

        let new_scale_120 = scale.clamp(1, 9999);
        if entry.scale_120 == new_scale_120 {
            return;
        }
        entry.scale_120 = new_scale_120;
        entry.needs_paint = true;

        // Find this surface's output_idx and resend Configure to its plugins.
        // (same pattern as resend_configure_region_for_output)
    }
}
```

Finding the output_idx from a `LockSurface` reference is awkward — the simplest
approach is to iterate with `enumerate()` and call
`resend_configure_scale_for_output(output_idx, new_scale_120)` (a new helper
mirroring the existing `resend_configure_region_for_output`).

---

## Step 4 — Thread `scale_120` through Configure construction

Every place that builds a `Configure` in the core currently passes `scale`
(from `wl_output.scale`). Change these to `scale_120` (from
`LockSurface::scale_120`).

### `spawn_plugins_for_output` (`app/mod.rs`)

Currently reads `wl_output.scale` → converts to `u32` → passes as `scale`.
Change to read `lock_surfaces[output_idx].scale_120` directly (it was already
initialised from the integer scale in step 4).

### `resend_configure_region_for_output` (`app/mod.rs`)

Add a parallel helper `resend_configure_scale_for_output(output_idx, scale_120)`
that overrides the `scale_120` field (the same clone-override-send-store
pattern as the region helper).

### `process_periodic_tick` (`app/mod.rs`)

No change needed — it clones `slot.last_configure` and only overrides time
fields; `scale_120` carries forward unchanged.

---

## Step 5 — Update reference plugins (log lines only)

Both `plugins/clock/src/main.rs` and `plugins/label/src/main.rs` log
`scale={}` from `first_configure.scale`. Change to `scale_120={}` matching the
renamed field. No render logic changes — both plugins already size everything as
fractions of `region_h`/`region_w`.

---

## Step 6 — Update docs

### `docs/protocol.md`

- §7.1 `Configure`: update `scale` row to `scale_120 (u32, 120ths; 120=1×,
  180=1.5×, 240=2×)`, update range, add sentence about the encoding.

### `docs/plugin-api.md` HiDPI section

Currently documents integer-only scale. Update to explain `scale_120`, show
the conversion `scale_120 as f32 / 120.0`, and note that `region_w`/`region_h`
are already physical pixels so no multiplication is needed there.

---

## Suggested commit sequence

1. `veiland-protocol`: rename `scale` → `scale_120`, update tests.
2. `veiland-core`: bind fractional scale manager, add `LockSurface` fields,
   attach per-surface object, implement `preferred_scale` handler.
3. `veiland-core`: thread `scale_120` through all Configure construction sites;
   add `resend_configure_scale_for_output` helper.
4. Plugins: update log lines.
5. Docs: `protocol.md` + `plugin-api.md`.

Step 1 must land before steps 2–3 (protocol rename must compile first).
Steps 4 and 5 can happen together with step 3 or after.

---

## What to test manually

On a compositor with fractional scaling configured (e.g. `monitor=eDP-1,
1920x1200@60, 0x0, 1.5` in Hyprland, or `output eDP-1 scale 1.5` in Sway):

1. Before the fix: blurry text on the fractional-scale output, sharp on 1×.
2. After the fix: text sharp on both. The `preferred_scale` log line should
   show a non-multiple-of-120 value (e.g. `scale_120=150` for 1.25×).
3. On a machine with only integer-scale outputs: no regression; `scale_120`
   values are multiples of 120; fallback path (no fractional scale manager)
   also produces correct results.
