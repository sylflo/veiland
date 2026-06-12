<!-- SPDX-License-Identifier: GPL-3.0-or-later -->

# Auth state feedback — implementation plan

Adds visual feedback to the password field for three conditions:
- **Failed auth**: brief red tint on the box (~1.5 s), then self-clears.
- **Caps lock active**: amber tint on the box while the modifier is held.
- **Checking** (PAM in progress): reserved in the enum; no visual change yet
  (PAM is synchronous today so the user never sees the intermediate state).

Tint-only for v1 — no caps lock text label.

All changes are in `veiland-core`. No protocol changes, no new dependencies.
Calloop's `Timer` arrives via the existing `smithay_client_toolkit::reexports::calloop`
re-export, so no `Cargo.toml` edit is needed.

---

## Files touched

| File | Change |
|---|---|
| `veiland-core/src/main.rs` | Add `AuthState` enum; add `auth_state` field to `AppData` |
| `veiland-core/src/config.rs` | Add `fail_color`, `capslock_color` fields to `Password` |
| `veiland-core/src/app/mod.rs` | Drive state transitions in `handle_key`; register the fail-clear timer; update `draw_password_field` call site |
| `veiland-core/src/renderer.rs` | Accept `auth_state` + `caps_lock` in `draw_password_field`; apply colour overrides before `draw_box` |

---

## Step 1 — `AuthState` enum + `AppData` field (`main.rs`)

Add alongside the existing `RunState`:

```rust
#[derive(Default, PartialEq, Clone, Copy)]
pub(crate) enum AuthState {
    #[default]
    Idle,
    Checking,
    Failed,
}
```

Add to `AppData`:

```rust
auth_state: AuthState,
```

Initialise in the struct literal in `main()`:

```rust
auth_state: AuthState::default(),
```

`Checking` has no visual handling yet — it exists so the enum is complete and
a future async-PAM branch has a place to land without a breaking change.

---

## Step 2 — Config fields (`config.rs`)

Add two fields to `Password`:

```rust
/// Box colour override when the last auth attempt failed. Applied for
/// ~1.5 s then returns to `inner_color`. Default a saturated red.
#[serde(default = "default_fail_color")]
pub fail_color: Color,

/// Box colour override while caps lock is active. Default amber.
#[serde(default = "default_capslock_color")]
pub capslock_color: Color,
```

Default functions:

```rust
fn default_fail_color() -> Color {
    Color::new(180.0 / 255.0, 40.0 / 255.0, 40.0 / 255.0, 0.75)
}
fn default_capslock_color() -> Color {
    Color::new(200.0 / 255.0, 150.0 / 255.0, 30.0 / 255.0, 0.75)
}
```

Add them to `Default for Password` and to the `validate_password` clamp block
if one exists (these colours have no range to clamp, but they need to appear
in `Default::default()`).

Colour override priority (applied at draw time, not stored):

1. `Failed` → `fail_color`
2. caps lock active → `capslock_color`
3. otherwise → `inner_color` (the existing field, unchanged)

Only `inner_color` is overridden — `outer_color` and `dot_color` are left
alone in all states.

---

## Step 3 — State transitions in `handle_key` (`app/mod.rs`)

### On auth failure

In the `Err(_)` arm of `auth.authenticate(...)`, after `buffer_changed = true`:

```rust
self.auth_state = AuthState::Failed;

use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use std::time::Duration;

const FAIL_FLASH_DURATION: Duration = Duration::from_millis(1500);

let _ = self.loop_handle.insert_source(
    Timer::from_duration(FAIL_FLASH_DURATION),
    |_, _, state: &mut AppData| {
        state.auth_state = AuthState::Idle;
        for entry in state.lock_surfaces.iter_mut().flatten() {
            entry.needs_paint = true;
        }
        TimeoutAction::Drop
    },
);
```

`insert_source` returns `Result`; the `let _ =` discards the `RegistrationToken`
because we don't need to cancel it (it self-drops via `TimeoutAction::Drop`).
If registration fails (shouldn't in practice), the flash just never self-clears
— not a locker-safety issue, only cosmetic. Log it with `eprintln!` before
discarding if you prefer.

### On any keystroke that edits the buffer (push or pop)

At the top of the `BackSpace` arm and the `_ =>` text arm, before the existing
logic:

```rust
if self.auth_state == AuthState::Failed {
    self.auth_state = AuthState::Idle;
}
```

This clears the flash the moment the user starts retyping, which is more
responsive than waiting for the timer. The timer fires harmlessly when it
expires (setting `Idle` again on an already-`Idle` state is a no-op).

---

## Step 4 — Renderer signature and colour override (`renderer.rs`)

### New signature for `draw_password_field`

```rust
pub fn draw_password_field(
    &mut self,
    password: &config::Password,
    char_count: usize,
    auth_state: crate::AuthState,
    caps_lock: bool,
    width: i32,
    height: i32,
)
```

### Colour override logic (add before the `draw_box` call)

```rust
let effective_inner = match auth_state {
    crate::AuthState::Failed => pw.fail_color,
    _ if caps_lock => pw.capslock_color,
    _ => pw.inner_color,
};
```

Then build an ephemeral `Password` with `inner_color` replaced, or — simpler
— pass `effective_inner` directly to `draw_box` by adding a parameter. The
cleanest approach is a local override struct:

```rust
let pw_override = config::Password {
    inner_color: effective_inner,
    ..pw.clone()
};
self.draw_box(&pw_override, centre_x_px, centre_y_px, w, h);
```

`config::Password` derives `Clone`, so this is cheap.

---

## Step 5 — Update the call site (`app/mod.rs`)

The single call to `draw_password_field` in `repaint_lock_surface`:

```rust
self.renderer.draw_password_field(
    &self.config.password,
    self.auth.char_count(),
    self.auth_state,
    self.modifiers.caps_lock,
    w,
    h,
);
```

`self.modifiers` is already updated by `KeyboardHandler::update_modifiers` on
every modifier event — caps lock state is available without any new tracking.

---

## What does NOT change

- The password buffer itself (`auth::Session`) — no state leaks out.
- Plugin protocol — no new messages or fields.
- `veiland-text` / placeholder rendering path.
- Any plugin code.
- The unlock decision path.

---

## Suggested commit sequence

1. `AuthState` enum + `AppData` field (compiles, no visible change).
2. Config fields `fail_color` / `capslock_color` (compiles, unused).
3. State transitions in `handle_key` (timer + early-clear on retype).
4. Renderer signature + colour override + call-site update (all visible).

Each step compiles independently. Step 4 is the only one that touches the
security-adjacent `handle_key` path beyond adding one enum assignment — review
it as carefully as the surrounding auth code.
