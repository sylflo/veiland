# Hotplug refactor: track outputs by registry ID

## Problem

Veiland currently tracks `LockSurface` entries by **output name** (the
`xdg_output.name` string, e.g. `"DP-1"`). This causes a special case: when
Hyprland unplugs a monitor, it sometimes re-advertises the *surviving*
monitor's `wl_output` under a new registry global ID. SCTK fires `new_output`
for that new global — with the same name as the surface we already have.
Veiland detects this collision by name and routes it to a `pending_outputs_rebound`
queue, which tears down and recreates the surface without respawning plugins.

This Hyprland behaviour is a compositor quirk, not a Wayland protocol
requirement. Hyprlock does not have a rebind special case: it tracks outputs by
**registry numeric ID** and uses RAII destructors for teardown. The re-advertised
surviving output is handled as a normal remove + add cycle. No name collision
logic needed.

## Goal

Switch veiland to the same model: track by registry numeric ID. This eliminates
the rebind path and `update_output` special-case entirely, leaving one clean
plug-in path and one clean unplug path — both compositors, no divergence.

---

## What changes

### `LockSurface` struct (`app/mod.rs`)

Add `registry_id: u32` — the `wl_registry` global name for this output,
received via SCTK's `OutputInfo::id`. Keep `name: String` for logging and for
the plugin monitor-selector filter. **The `registry_id` becomes the key; the
`name` becomes data.**

```rust
pub(crate) struct LockSurface {
    pub(crate) registry_id: u32,   // ← new: registry numeric ID, used as key
    pub(crate) name: String,       // kept: for logging + entry_matches_output
    // ... rest unchanged
}
```

### `AppData` fields (`main.rs`)

Remove `pending_outputs_rebound`. Keep `pending_outputs_arrived` — it now
carries `(wl_output, registry_id, name)` instead of `(wl_output, name)`.

### `new_output` (`app/output.rs`)

Stop checking for name collision. Just queue every new output to
`pending_outputs_arrived` with its registry ID. The registry ID is available
via `output_state.info(&output).map(|i| i.id)`.

```rust
fn new_output(&mut self, ..., output: wl_output::WlOutput) {
    let info = self.output_state.info(&output);
    let id   = info.map(|i| i.id).unwrap_or(0);
    let name = info.and_then(|i| i.name).unwrap_or_else(|| "<unnamed>".into());
    self.pending_outputs_arrived.push((output, id, name));
}
```

### `update_output` (`app/output.rs`)

Remove the rebind detection entirely. The only remaining reason
`update_output` fires (mode/scale change on a live output) is already a no-op.
The whole method becomes:

```rust
fn update_output(&mut self, ..., output: wl_output::WlOutput) {
    // mode/scale change on a live output — no action needed here.
    // Scale updates arrive via wp_fractional_scale_v1; size updates
    // arrive via SessionLockHandler::configure.
}
```

Or it can be deleted if SCTK allows a default no-op (check the trait).

### `output_destroyed` (`app/output.rs`)

Look up the slot by **registry ID** instead of name. SCTK gives us the
`WlOutput` proxy; we get the ID from `output_state.info(&output).map(|i| i.id)`.

```rust
fn output_destroyed(&mut self, ..., output: wl_output::WlOutput) {
    let id = match self.output_state.info(&output).map(|i| i.id) {
        Some(id) => id,
        None => { eprintln!("output_destroyed: no info, skipping"); return; }
    };
    let output_idx = self.lock_surfaces.iter().position(|opt| {
        opt.as_ref().map(|ls| ls.registry_id == id).unwrap_or(false)
    });
    // ... rest of 4-phase teardown unchanged
}
```

### `create_lock_surface_for_output` (`app/mod.rs`)

Accept `registry_id: u32` as a new parameter. Store it in the new field.

### `process_pending_hotplug` (`app/mod.rs`)

Remove the `rebound` queue drain entirely. The arrivals drain is unchanged
except it now passes `registry_id` through to `create_lock_surface_for_output`.

The "already have a surface for this name" idempotency guard can be replaced
by "already have a surface for this registry ID" — or removed, since with
correct ID-based tracking it can no longer happen legitimately.

---

## What does NOT change

- `LockSurface::name` — kept, used by `entry_matches_output` and logging.
- `entry_matches_output` — unchanged; still matches config `monitors` strings
  against the output name.
- `spawn_plugins_for_output` — unchanged; still takes `output_name: &str`.
- The 4-phase teardown sequence in `output_destroyed` — unchanged.
- The deferred-drain pattern (`process_pending_hotplug` after each dispatch) —
  kept. The reason still holds: SCTK must finish processing the event batch
  before we call EGL/session_lock APIs.
- Plugin respawn behaviour on unplug/replug — same as today (respawned on
  replug, since the output slot gets a fresh lock surface).

---

## Files touched

| File | Change |
|---|---|
| `veiland-core/src/main.rs` | Remove `pending_outputs_rebound` field; add `registry_id` to `pending_outputs_arrived` tuple |
| `veiland-core/src/app/mod.rs` | `LockSurface` gains `registry_id`; `create_lock_surface_for_output` takes `registry_id`; `process_pending_hotplug` drops rebind drain |
| `veiland-core/src/app/output.rs` | `new_output` drops name-collision check; `update_output` becomes no-op or is removed; `output_destroyed` looks up by ID |

No protocol changes. No plugin SDK changes. No config changes.

---

## Steps (implement one at a time)

1. **Add `registry_id: u32` to `LockSurface`** and update
   `create_lock_surface_for_output` to accept and store it. Populate it in the
   startup loop in `main.rs` (needs `OutputInfo::id` there too).

2. **Update `pending_outputs_arrived`** tuple from `(WlOutput, String)` to
   `(WlOutput, u32, String)`. Update `new_output` to populate the ID. Update
   the arrivals drain in `process_pending_hotplug` to pass it through.

3. **Update `output_destroyed`** to look up by `registry_id` instead of name.

4. **Remove `pending_outputs_rebound`** from `AppData` and delete the rebind
   queue drain from `process_pending_hotplug`.

5. **Simplify `new_output`** — remove the `already_have` name-collision check
   and the rebound routing branch.

6. **Simplify or remove `update_output`** — the rebind detection block goes
   away; what remains is a no-op.

7. Build, run, test plug/unplug on both Sway and Hyprland.

---

## Risk

Low. The 4-phase teardown, deferred-drain pattern, and plugin lifecycle are
untouched. The only semantic change is that a Hyprland re-advertisement of a
surviving monitor is now handled as remove + add (plugins respawn) instead of
surface-only recreate (plugins kept). This is a minor UX regression for the
fast-replug edge case, but the fast-replug case currently crashes anyway — so
no net loss.
