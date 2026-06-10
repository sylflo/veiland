// SPDX-License-Identifier: GPL-3.0-or-later

//! `SeatHandler` + `KeyboardHandler` impls: keyboard acquisition and
//! the raw key-event entry points.
//!
//! Security-critical surface (CLAUDE.md "Trust boundaries"): this is
//! where keyboard focus is taken and where key events enter the core.
//! The actual password-buffer / PAM / unlock logic lives in
//! `AppData::handle_key` (see `app/mod.rs`); every key entry point
//! forwards to it.
//!
//! Key auto-repeat (holding a key) is driven by SCTK's calloop timer,
//! set up via `get_keyboard_with_repeat` in `new_capability` below; its
//! callback calls `handle_key` directly. The `KeyboardHandler::repeat_key`
//! method also forwards to `handle_key` for any compositor that emits its
//! own repeat events, so both paths behave identically.

use smithay_client_toolkit::seat::{
    Capability, SeatHandler, SeatState,
    keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers, RawModifiers},
};

use wayland_client::{
    Connection, QueueHandle,
    protocol::{wl_keyboard, wl_seat, wl_surface},
};

use crate::AppData;

impl SeatHandler for AppData {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            eprintln!("veiland-core: set keyboard capability");
            // `get_keyboard_with_repeat` (not the plain `get_keyboard`) so
            // held keys auto-repeat: SCTK registers a calloop timer driven
            // by the compositor-advertised `wl_keyboard.repeat_info` rate
            // and delay, and invokes our callback for each synthetic
            // repeat. Without this, only the initial press fires, so e.g.
            // holding Backspace deletes a single character. The callback
            // routes through the same `handle_key` path as a real press —
            // no separate repeat logic, and the threat-model boundary is
            // unchanged (still core-only, still no plugin involvement).
            let keyboard = self
                .seat_state
                .get_keyboard_with_repeat(
                    qh,
                    &seat,
                    None,
                    self.loop_handle.clone(),
                    Box::new(|app: &mut AppData, _kbd, event| {
                        app.handle_key(&event);
                    }),
                )
                .expect("Failed to create keyboard");
            self.keyboard = Some(keyboard);
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_some() {
            eprintln!("veiland-core: unset keyboard capability");
            self.keyboard.take().unwrap().release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl KeyboardHandler for AppData {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.handle_key(&event);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.handle_key(&event);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: Modifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.modifiers = modifiers;
    }
}
