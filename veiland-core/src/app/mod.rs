// SPDX-License-Identifier: GPL-3.0-or-later

//! `AppData` — the single calloop/SCTK dispatch target — and its
//! supporting types and inherent methods.
//!
//! SCTK's `delegate_*` macros and the calloop event loop both require
//! one concrete state type, so `AppData` stays whole and owns
//! everything. This module holds the cross-cutting inherent methods
//! (lock-surface creation, plugin spawning, the hotplug drain, the
//! periodic Configure tick, repaint/compositing, and the plugin-socket
//! driver). The Wayland trait impls live in sibling modules
//! (`lock`, `compositor`, `output`, `input`); the `AppData` struct
//! definition and `main()` stay in the crate root.

mod compositor;
mod input;
mod lock;
mod output;

use std::time::Duration;

use smithay_client_toolkit::{
    reexports::calloop::{Interest, Mode, PostAction, generic::Generic},
    session_lock::SessionLockSurface,
};

use wayland_client::protocol::wl_output;

use nix::unistd::{User, getuid};

use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use khronos_egl as egl;

use veiland_protocol::{ClientMessage, Configure};

use crate::plugin::{
    self, PluginSlot, current_time_for_configure, entry_matches_output, try_spawn_one,
};
use crate::region;
use crate::{AppData, RunState};

/// One output's lock surface plus the EGL window/surface bound to it.
pub(crate) struct LockSurface {
    pub(crate) name: String,
    /// The `wl_output` proxy this lock surface was created against.
    /// Kept so `update_output` can detect a rebind: if SCTK rebinds
    /// the global on a topology change, the new `WlOutput` will have
    /// a different `id()` and we know to destroy + recreate this
    /// surface against the fresh proxy. Comparing by id (not by
    /// equality of the WlOutput value) is the protocol-correct way
    /// to detect identity change.
    pub(crate) wl_output: wl_output::WlOutput,
    pub(crate) lock_surface: SessionLockSurface,
    pub(crate) egl_window: Option<wayland_egl::WlEglSurface>,
    pub(crate) egl_surface: Option<egl::Surface>,
    /// True when something has changed since the last paint: a plugin
    /// imported a new texture, a keystroke moved the indicator. The
    /// frame-callback handler checks this; if false it skips the paint
    /// and does not request another callback, so a static lockscreen
    /// stops burning 60Hz no-op repaints.
    pub(crate) needs_paint: bool,
    /// True after `wl_surface.frame()` was called and before the
    /// matching `done` event came back. Prevents requesting a second
    /// callback per commit.
    pub(crate) frame_callback_pending: bool,
    /// Surface size in physical pixels, as last reported by the
    /// compositor's `configure` (`SessionLockHandler::configure`'s
    /// `new_size`). `None` until the first `configure` fires — at
    /// spawn time the size is genuinely not known yet, since the
    /// compositor sends it asynchronously after the surface is
    /// created. This is the size plugins should render at; later
    /// commits feed it into the plugin `Configure` so a 4K monitor
    /// gets a 4K buffer instead of an upscaled 1080p one.
    pub(crate) surface_size: Option<(u32, u32)>,
}

impl AppData {
    /// Create a `LockSurface` for one output and place it in
    /// `self.lock_surfaces`. Prefers reusing the first `None` slot
    /// (left behind by an earlier `output_destroyed`) so the
    /// `lock_surfaces` ↔ `plugins` index correspondence stays
    /// compact across hotplug cycles. If no `None` slot exists,
    /// pushes a fresh one. Returns the index the surface was
    /// placed at, or `None` if the session_lock isn't held
    /// (defensive check, log + skip).
    pub(crate) fn create_lock_surface_for_output(
        &mut self,
        output: &wl_output::WlOutput,
        name: String,
    ) -> Option<usize> {
        let session_lock = match self.session_lock.as_ref() {
            Some(l) => l,
            None => {
                eprintln!(
                    "veiland-core: refusing to create lock surface for {:?}: \
                    no session lock held",
                    name
                );
                return None;
            }
        };
        let surface = self.compositor_state.create_surface(&self.qh);
        eprintln!(
            "veiland-core: output {} connected, creating lock surface",
            name
        );
        let lock_surface = LockSurface {
            name,
            wl_output: output.clone(),
            lock_surface: session_lock.create_lock_surface(surface, output, &self.qh),
            egl_window: None,
            egl_surface: None,
            needs_paint: true,
            frame_callback_pending: false,
            surface_size: None,
        };
        // Reuse the first None slot if one exists (hotplug-out leaves
        // sentinels; reusing keeps lock_surfaces.len() bounded under
        // long sessions with frequent topology changes).
        if let Some(idx) = self.lock_surfaces.iter().position(|s| s.is_none()) {
            self.lock_surfaces[idx] = Some(lock_surface);
            Some(idx)
        } else {
            self.lock_surfaces.push(Some(lock_surface));
            Some(self.lock_surfaces.len() - 1)
        }
    }

    /// Spawn the per-output slice of plugins for `output_name` and
    /// place it at `output_idx` in `self.plugins`, growing the outer
    /// vec with empty slices as needed so the index matches the
    /// LockSurface index returned by `create_lock_surface_for_output`.
    /// Filters each entry by its `monitors` selector. Reusing slot
    /// `output_idx` (overwriting whatever was there) handles the
    /// hotplug-in case where `create_lock_surface_for_output`
    /// returned a recycled `None` slot.
    pub(crate) fn spawn_plugins_for_output(&mut self, output_idx: usize, output_name: &str) {
        // Look up the output's scale factor via SCTK's OutputInfo and clamp to
        // the protocol's 1..=3 range. Hardware reporting an out-of-range value
        // (e.g. 4 on 8K-at-200%) is rare but real; clamping to 3 keeps text
        // *almost* the right size on unfamiliar hardware, which is the least
        // surprising failure mode. Raising the cap is a separate decision.
        let raw_scale = self.lock_surfaces[output_idx]
            .as_ref()
            .and_then(|s| self.output_state.info(&s.wl_output))
            .map(|info| info.scale_factor)
            .unwrap_or(1);
        let scale: u32 = match u32::try_from(raw_scale) {
            Ok(s) if (1..=3).contains(&s) => s,
            Ok(s) => {
                eprintln!(
                    "veiland-core: output {} reports scale {}, outside 1..=3; clamping to 3",
                    output_name, s
                );
                3
            }
            Err(_) => {
                eprintln!(
                    "veiland-core: output {} reports negative scale {}; clamping to 1",
                    output_name, raw_scale
                );
                1
            }
        };

        // The surface size in physical pixels, if the compositor has
        // reported it yet. On a fresh lock this is usually `None` here:
        // spawn runs before the compositor's first `configure`, so the
        // size only lands afterwards (commit 3 resends Configure when it
        // does). `try_spawn_one` falls back to 1080p for the brief
        // initial window. On hotplug-in of an output that already has a
        // size cached it will be `Some`, so those plugins start at the
        // right size immediately.
        let surface_size = self.lock_surfaces[output_idx]
            .as_ref()
            .and_then(|s| s.surface_size);

        let mut per_output: Vec<Option<PluginSlot>> = Vec::with_capacity(self.config.plugins.len());
        for entry in &self.config.plugins {
            if !entry_matches_output(entry, output_name) {
                per_output.push(None);
                continue;
            }
            match try_spawn_one(
                entry,
                output_name,
                scale,
                surface_size,
                self.host_capabilities,
                &self.renderer.egl,
                self.renderer.egl_display,
            ) {
                Ok(slot) => {
                    eprintln!(
                        "veiland-core: spawned plugin {:?} on output {} (binary {:?}, z_index {}) pid={}",
                        slot.name, slot.output_name, slot.binary, slot.z_index, slot.pid
                    );
                    per_output.push(Some(slot));
                }
                Err(e) => {
                    eprintln!(
                        "veiland-core: plugin {:?} failed to start on output {}: {} — its layer will be empty",
                        entry.name, output_name, e
                    );
                    per_output.push(None);
                }
            }
        }
        // Sort by z_index, stable: ties keep config-file order. Failed (None)
        // slots sort to the end via i32::MAX; they never render anything.
        per_output.sort_by_key(|slot| slot.as_ref().map(|s| s.z_index).unwrap_or(i32::MAX));
        // Place at output_idx, growing with empty slices if needed.
        while self.plugins.len() <= output_idx {
            self.plugins.push(Vec::new());
        }
        self.plugins[output_idx] = per_output;
    }

    /// Register a plugin's socket as a calloop event source. Captures
    /// `(o, p)` by value; the closure stays valid as long as
    /// `self.plugins[o][p]` remains in place (slot may be `None` later;
    /// `drive_plugin` handles that by returning `PostAction::Remove`).
    pub(crate) fn register_plugin_source(&self, o: usize, p: usize) {
        let Some(slot) = self
            .plugins
            .get(o)
            .and_then(|po| po.get(p))
            .and_then(|s| s.as_ref())
        else {
            return;
        };
        let plugin_fd = slot
            .state
            .connection
            .as_fd()
            .try_clone_to_owned()
            .expect("dup plugin socket for calloop");
        self.loop_handle
            .insert_source(
                Generic::new(plugin_fd, Interest::READ, Mode::Level),
                move |_event, _meta, state: &mut AppData| state.drive_plugin(o, p),
            )
            .expect("register plugin fd with calloop");
    }

    /// Drain the pending-hotplug queues. Called after every
    /// `event_loop.dispatch()` returns — by that point SCTK has fully
    /// processed all events from the batch (registry bind/unbind,
    /// xdg_output.name, geometry, etc.) and our internal state is
    /// safe to mutate.
    ///
    /// Two queues:
    ///
    /// - `pending_outputs_arrived`: outputs whose `wl_output` global
    ///   newly appeared. Create a lock surface (the compositor will
    ///   send us a Configure for it on a later dispatch), then spawn
    ///   the matching plugin instances and register their sockets.
    ///
    /// - `pending_outputs_rebound`: outputs whose `wl_output` proxy
    ///   was rebound (Hyprland's fast-replug pattern). The plugins
    ///   are still alive and connected; only the lock surface needs
    ///   replacing. Destroy the old surface (dropping it lets SCTK's
    ///   Drop send `ext_session_lock_surface_v1.destroy()`), then
    ///   create a fresh one against the new proxy.
    ///
    /// Both paths are idempotent against running again with the same
    /// queues — we filter "already have a lock surface for this
    /// name" out of the arrived queue defensively in case SCTK fires
    /// `new_output` for an output we already created at startup.
    pub(crate) fn process_pending_hotplug(&mut self) {
        let arrived = std::mem::take(&mut self.pending_outputs_arrived);
        let rebound = std::mem::take(&mut self.pending_outputs_rebound);

        // --- Arrivals ---
        // For each newly-arrived output, create a lock surface and
        // spawn plugins. The compositor will send a Configure for
        // the new surface on a subsequent dispatch.
        for (output, name) in arrived {
            // Defensive: if we already have a lock surface for this
            // name, this is a spurious notification. Skip rather
            // than double-create.
            let already_have = self
                .lock_surfaces
                .iter()
                .any(|s| s.as_ref().map(|ls| ls.name == name).unwrap_or(false));
            if already_have {
                eprintln!(
                    "[M8-TRACE] arrival skipped: {:?} already has a lock surface",
                    name
                );
                continue;
            }
            eprintln!("[M8-TRACE] processing arrival: {:?}", name);
            let Some(idx) = self.create_lock_surface_for_output(&output, name.clone()) else {
                continue;
            };
            self.spawn_plugins_for_output(idx, &name);
            // Register calloop sources for every plugin slot at this
            // index. Even if the slot was previously occupied (slot
            // recycled from a torn-down output), the prior calloop
            // sources self-removed when their plugin sockets hit EOF;
            // the new processes need fresh sources.
            for p in 0..self.plugins[idx].len() {
                self.register_plugin_source(idx, p);
            }
        }

        // --- Rebinds ---
        // For each rebound output, replace the lock surface with a
        // fresh one against the new wl_output proxy. The old surface
        // is dropped (sending destroy) before the new one is created.
        for (output, name) in rebound {
            eprintln!("[M8-TRACE] processing rebind: {:?}", name);
            // Find the matching slot. If it's gone (e.g. our previous
            // drain step already replaced it), nothing to do.
            let Some(idx) = self
                .lock_surfaces
                .iter()
                .position(|s| s.as_ref().map(|ls| ls.name == name).unwrap_or(false))
            else {
                eprintln!(
                    "[M8-TRACE] rebind skipped: {:?} has no current lock surface",
                    name
                );
                continue;
            };
            // Tear down EGL bits first (same order as output_destroyed
            // Phase 3 — keep EGL from sending commits to a dying surface).
            if let Some(surface_ref) = self.lock_surfaces[idx].as_mut() {
                if let Some(egl_surface) = surface_ref.egl_surface.take() {
                    if let Err(e) = self.renderer.egl.destroy_surface(self.renderer.egl_display, egl_surface) {
                        eprintln!(
                            "veiland-core: eglDestroySurface for {:?} (rebind) failed: \
                            {:?} (continuing)",
                            surface_ref.name, e
                        );
                    }
                }
                surface_ref.egl_window = None;
            }
            // Drop the old LockSurface so SCTK's Drop sends destroy.
            self.lock_surfaces[idx] = None;
            // Create the fresh one against the new wl_output proxy.
            // It will land in the just-emptied slot (`create_lock_
            // surface_for_output` prefers None slots).
            self.create_lock_surface_for_output(&output, name);
        }
    }

    /// Fire the 30 s periodic Configure tick. Called from the main
    /// loop after every `event_loop.dispatch()` returns; bails out
    /// early if less than 30 s have elapsed since the last tick.
    ///
    /// Why this exists: the host's only mandatory Configure is at
    /// spawn. A clock plugin (`veiland-clock`, M11 step 2) needs the
    /// time field to advance for its display to track the wall clock.
    /// Re-sending Configure with refreshed `time_unix_seconds` keeps
    /// plugins pure functions of host events instead of each one
    /// reaching for `clock_gettime` independently.
    ///
    /// 30 s is the cadence picked in `docs/m11-plan.md` Q1: within
    /// half a minute of true wall-clock time, cheap enough to ignore.
    /// Other Configure fields (region, scale, output_name) are
    /// re-sent unchanged from `slot.last_configure`.
    ///
    /// If a plugin's socket has died, `send_configure` will return
    /// an error; we log and continue. The next inbound-event read on
    /// that plugin's calloop source will surface the broken-pipe
    /// error through the existing drive_plugin error path, which
    /// removes the slot.
    pub(crate) fn process_periodic_tick(&mut self) {
        const TICK_INTERVAL: Duration = Duration::from_secs(30);
        if self.last_time_tick.elapsed() < TICK_INTERVAL {
            return;
        }
        self.last_time_tick = std::time::Instant::now();

        let (time_unix_seconds, time_tz_offset_seconds) = current_time_for_configure();
        for per_output in self.plugins.iter_mut() {
            for slot_opt in per_output.iter_mut() {
                let Some(slot) = slot_opt.as_mut() else {
                    continue;
                };
                let Some(prev) = slot.last_configure.clone() else {
                    continue;
                };
                let next = Configure {
                    time_unix_seconds,
                    time_tz_offset_seconds,
                    ..prev
                };
                if let Err(e) = slot.state.connection.send_configure(next.clone()) {
                    eprintln!(
                        "veiland-core: periodic tick: send_configure to plugin {:?} failed: {} \
                         (will be cleaned up on next read)",
                        slot.name, e
                    );
                    continue;
                }
                slot.last_configure = Some(next);
            }
        }
    }

    /// Re-send `Configure` to every live plugin on a single output with
    /// an updated render region. Called from `SessionLockHandler::
    /// configure` when the compositor reports a surface size that
    /// differs from what plugins were last told (the common cause: the
    /// first `configure` after spawn, where plugins were started with
    /// the 1080p fallback and now learn the true size — so a 4K monitor
    /// gets a 4K buffer instead of an upscaled 1080p one).
    ///
    /// Mirrors `process_periodic_tick`'s clone-override-send-store
    /// shape, but scoped to one output (a resize is per-output, unlike
    /// the time tick which advances everywhere) and overriding region
    /// instead of time. `region_x/y` stay 0 — full-surface placement is
    /// unchanged; only the size moves. The next periodic tick refreshes
    /// the time field as usual.
    ///
    /// Plugin-input rule applies: a dead socket logs and is skipped, it
    /// never panics. `drive_plugin`'s EOF path cleans the slot up later.
    pub(crate) fn resend_configure_region_for_output(
        &mut self,
        output_idx: usize,
        region_w: u32,
        region_h: u32,
    ) {
        let Some(per_output) = self.plugins.get_mut(output_idx) else {
            return;
        };
        for slot_opt in per_output.iter_mut() {
            let Some(slot) = slot_opt.as_mut() else {
                continue;
            };
            let Some(prev) = slot.last_configure.clone() else {
                continue;
            };
            let next = Configure {
                region_x: 0,
                region_y: 0,
                region_w,
                region_h,
                ..prev
            };
            if let Err(e) = slot.state.connection.send_configure(next.clone()) {
                eprintln!(
                    "veiland-core: resize resend: send_configure to plugin {:?} failed: {} \
                     (will be cleaned up on next read)",
                    slot.name, e
                );
                continue;
            }
            slot.last_configure = Some(next);
        }
    }

    /// Repaint every lock surface that already has an EGL window.
    /// Called when a new plugin Buffer arrives — without this, the
    /// first paint (in `configure`) happens before the plugin's
    /// first Buffer and the screen stays at the clear-color.
    /// Real frame-callback wiring is M5.
    pub(crate) fn repaint_lock_surfaces(&mut self) {
        // Bail if we're past the Running state. After lock.unlock()
        // the compositor destroys our lock surface server-side; a
        // swap_buffers() into it then blocks forever waiting for
        // the compositor to release the previous frame. That kept
        // the main loop stuck and teardown never ran (M6 step 6
        // bug; surfaced with three plugins, where there's always a
        // Buffer event queued behind unlock — two plugins worked by
        // timing luck).
        if !matches!(self.run, RunState::Running) {
            return;
        }
        for output_idx in 0..self.lock_surfaces.len() {
            self.repaint_lock_surface(output_idx);
        }
    }

    /// Paint one output's lock surface if it is dirty (`needs_paint`).
    /// Requests the next `wl_surface.frame` callback before swap so the
    /// compositor's repaint cadence keeps driving us. No-op if the
    /// surface is not ready (no `egl_window` / `egl_surface`) or
    /// `needs_paint` is false.
    ///
    /// Callers: the all-surfaces `repaint_lock_surfaces` loop, the
    /// per-surface `CompositorHandler::frame` callback, and the kick-
    /// a-paint path in the Buffer arrival handler.
    pub(crate) fn repaint_lock_surface(&mut self, output_idx: usize) {
        if !matches!(self.run, RunState::Running) {
            return;
        }
        let Some(entry) = self.lock_surfaces.get(output_idx).and_then(|e| e.as_ref()) else {
            return;
        };
        if !entry.needs_paint {
            return;
        }
        let Some(egl_surface) = entry.egl_surface else {
            return;
        };
        let Some(egl_window) = entry.egl_window.as_ref() else {
            return;
        };
        let (w, h) = egl_window.get_size();
        // Clone the WlSurface so we can call `.frame()` later without
        // re-borrowing `self.lock_surfaces`. Wayland proxies are Arc-
        // backed; the clone is cheap.
        let wl_surface = entry.lock_surface.wl_surface().clone();

        self.renderer.egl
            .make_current(
                self.renderer.egl_display,
                Some(egl_surface),
                Some(egl_surface),
                Some(self.renderer.egl_context),
            )
            .expect("eglMakeCurrent (repaint)");

        unsafe {
            gl::Viewport(0, 0, w, h);
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
            // Premultiplied-alpha blending. Plugins emit premultiplied
            // pixels (RGB already scaled by alpha) so glyph coverage and
            // shader alpha compose exactly once across the dmabuf
            // boundary; straight alpha here double-applied coverage and
            // haloed text edges. The password indicator (drawn after the
            // loop) emits premultiplied too. We don't toggle blend off
            // after the loop because nothing else needs it off.
            gl::Enable(gl::BLEND);
            gl::BlendFunc(gl::ONE, gl::ONE_MINUS_SRC_ALPHA);
        }

        for slot_opt in &self.plugins[output_idx] {
            if let Some(slot) = slot_opt {
                let rect = region::region_to_clip_rect(slot.region.as_ref(), w, h);
                slot.state.composite(
                    self.renderer.compositor_program,
                    self.renderer.compositor_vbo,
                    self.renderer.compositor_sampler_loc,
                    self.renderer.compositor_rect_loc,
                    rect,
                );
            }
        }

        // The password field (box + dots) paints on top of any plugins —
        // see the matching note in SessionLockHandler::configure.
        self.renderer.draw_password_field(
            &self.config.password,
            self.auth.char_count(),
            w,
            h,
        );

        // Request the next frame callback BEFORE swap. swap_buffers
        // calls wl_surface.commit; if we requested .frame() after the
        // commit it would attach to the next-after-this commit (which
        // usually never happens) and the callback would never fire.
        let should_request_callback = self
            .lock_surfaces
            .get(output_idx)
            .and_then(|e| e.as_ref())
            .is_some_and(|e| !e.frame_callback_pending);
        if should_request_callback {
            wl_surface.frame(&self.qh, wl_surface.clone());
            if let Some(entry) = self.lock_surfaces[output_idx].as_mut() {
                entry.frame_callback_pending = true;
            }
        }

        self.renderer.egl
            .swap_buffers(self.renderer.egl_display, egl_surface)
            .expect("eglSwapBuffers (repaint)");

        // Egress fence + BufferReleased for every plugin in this
        // output's row. Moved out of the Buffer-arrival path (M5
        // location); the host has just sampled the slot textures
        // into this lock surface, so it is now safe to tell plugins
        // they can reuse their dmabufs.
        self.release_sampled_buffers(output_idx);

        if let Some(entry) = self.lock_surfaces[output_idx].as_mut() {
            entry.needs_paint = false;
        }
    }

    /// Egress-fence and release every plugin's currently-bound buffer
    /// for one output. Called after `swap_buffers` on that output's
    /// lock surface — at that point the host's GL has finished
    /// submitting the sampling work and the fence guarantees the GPU
    /// agrees before we tell the plugin to reuse its dmabuf.
    pub(crate) fn release_sampled_buffers(&mut self, output_idx: usize) {
        if self
            .lock_surfaces
            .get(output_idx)
            .and_then(|e| e.as_ref())
            .is_none()
        {
            return;
        }

        let fence = match plugin::create_host_fence(&self.renderer.egl, self.renderer.egl_display) {
            Ok(f) => f,
            Err(e) => {
                eprintln!("egress fence create failed: {}", e);
                return;
            }
        };
        let wait_result = plugin::wait_fence(&self.renderer.egl, self.renderer.egl_display, &fence);
        plugin::release_fence(&self.renderer.egl, self.renderer.egl_display, fence);
        if let Err(e) = wait_result {
            eprintln!("egress fence wait failed: {}", e);
            return;
        }

        // One BufferReleased per Buffer received, not per paint.
        // Clearing `current_buffer_id` after release means a slot whose
        // plugin didn't ship a new Buffer since the last release just
        // skips here — the host's paint still re-samples the cached
        // EGLImage texture, which is fine because the plugin hasn't
        // touched the underlying dmabuf either.
        //
        // Without this clear, static plugins (vignette, wallpaper)
        // got a BufferReleased on every paint at compositor refresh
        // rate. Harmless protocol-wise but wasteful, and depending on
        // the plugin's loop shape it could cause stale-frame
        // confusion (see the vignette flicker bug fixed alongside
        // this).
        let plugin_count = self.plugins[output_idx].len();
        for p in 0..plugin_count {
            let Some(slot) = self.plugins[output_idx][p].as_mut() else {
                continue;
            };
            let Some(id) = slot.state.current_buffer_id.take() else {
                continue;
            };
            if let Err(e) = slot.state.connection.send_buffer_released(id) {
                let name = slot.name.clone();
                eprintln!(
                    "veiland-core: plugin {:?} send_buffer_released failed: {}",
                    name, e
                );
                self.plugins[output_idx][p] = None;
            }
        }
    }

    /// Handle one key event from the keyboard: the keystroke →
    /// password-buffer → PAM → unlock path. Called from both
    /// `press_key` and `repeat_key`. Security-critical: see CLAUDE.md
    /// "Trust boundaries" — this is the only place the password buffer
    /// is fed and the only place the unlock decision is made.
    pub(crate) fn handle_key(&mut self, event: &KeyEvent) {
        // Reject modified keys. Plain Shift is fine (for capitals); Ctrl/Alt/Super are not.
        if self.modifiers.ctrl || self.modifiers.alt || self.modifiers.logo {
            return;
        }

        // Tracks whether this key changed the indicator-visible state
        // (buffer length). Set on push/pop and on PAM-fail (the
        // buffer is cleared inside authenticate()). On unlock we
        // don't bother — the lock surface is about to go away and
        // repaint_lock_surfaces will bail on RunState::UnlockedCleanly
        // anyway.
        let mut buffer_changed = false;

        match event.keysym {
            Keysym::Return | Keysym::KP_Enter => {
                if self.auth.is_empty() {
                    return;
                }
                let user = match User::from_uid(getuid()) {
                    Ok(Some(u)) => u.name,
                    _ => {
                        eprintln!("auth: cannot resolve current user, refusing auth");
                        return;
                    }
                };
                match self.auth.authenticate("veiland", &user) {
                    Ok(()) => {
                        if let Some(lock) = self.session_lock.take() {
                            lock.unlock();
                            self.conn.roundtrip().expect("flush unlock");
                        }
                        self.run = RunState::UnlockedCleanly;
                    }
                    Err(_) => {
                        // Buffer already cleared by authenticate(). User retypes.
                        // Repaint so the dots vanish — that's the
                        // (silent) failure feedback for M9.
                        buffer_changed = true;
                    }
                }
            }
            Keysym::BackSpace => {
                self.auth.pop_char();
                buffer_changed = true;
            }
            #[cfg(feature = "debug-unlock")]
            Keysym::Escape => {
                self.auth.clear();
                if let Some(lock) = self.session_lock.take() {
                    lock.unlock();
                    self.conn.roundtrip().expect("flush unlock");
                }
                self.run = RunState::UnlockedCleanly;
            }
            _ => {
                if let Some(s) = event.utf8.as_deref()
                    && !s.chars().any(|c| c.is_control())
                {
                    self.auth.push_utf8(s);
                    buffer_changed = true;
                }
            }
        }

        if buffer_changed {
            // Synchronous repaint from inside the keyboard handler.
            // Single-threaded calloop: this can't race with a Buffer-
            // driven repaint because both run on the same loop. The
            // RunState guard inside repaint_lock_surfaces handles the
            // post-unlock case defensively.
            //
            // Mark every surface dirty first — repaint_lock_surface is
            // gated on `needs_paint`, and a password keystroke is the
            // host's own dirty event (no Buffer arrival precedes it).
            for entry in self.lock_surfaces.iter_mut().flatten() {
                entry.needs_paint = true;
            }
            self.repaint_lock_surfaces();
        }
    }

    fn slot_mut(&mut self, o: usize, p: usize) -> Option<&mut PluginSlot> {
        self.plugins.get_mut(o)?.get_mut(p)?.as_mut()
    }

    fn plugin_name_for_log(&self, o: usize, p: usize) -> String {
        self.plugins
            .get(o)
            .and_then(|per_output| per_output.get(p))
            .and_then(|s| s.as_ref())
            .map(|s| s.name.clone())
            .unwrap_or_else(|| "<unknown>".to_string())
    }

    pub(crate) fn drive_plugin(&mut self, o: usize, p: usize) -> std::io::Result<PostAction> {
        // 1. Recv. Borrow scoped to this block.
        let recv_result = {
            let Some(slot) = self.slot_mut(o, p) else {
                // Slot was nulled by an earlier event on this fd
                // before calloop drained the queue. Remove the source.
                return Ok(PostAction::Remove);
            };
            slot.state.connection.recv_message()
        };

        let (msg, fds) = match recv_result {
            Ok(t) => t,
            Err(e) => {
                let name = self.plugin_name_for_log(o, p);
                eprintln!(
                    "veiland-core: plugin {:?} disconnected or violated protocol: {}",
                    name, e
                );
                self.plugins[o][p] = None;
                return Ok(PostAction::Remove);
            }
        };

        let is_buffer = matches!(msg, ClientMessage::Buffer(_));

        // 2. Dispatch. The block produces an owned outcome
        //    (Result<(), (name, err)>) so the slot borrow ends before
        //    we touch `self.plugins[i] = None` on the failure path.
        //    `&self.renderer.egl` and `self.renderer.egl_display` are captured *before*
        //    the slot borrow because handle_message needs them while
        //    we hold the slot; the borrow checker won't let us reach
        //    into self after slot_mut has taken &mut self.
        let dispatch_result: Result<(), (String, plugin::HostError)> = {
            let egl = &self.renderer.egl;
            let display = self.renderer.egl_display;
            let Some(slot) = self
                .plugins
                .get_mut(o)
                .and_then(|per_output| per_output.get_mut(p))
                .and_then(|s| s.as_mut())
            else {
                return Ok(PostAction::Remove);
            };
            let name = slot.name.clone();
            slot.state
                .handle_message(msg, fds, egl, display)
                .map_err(|e| (name, e))
        };

        if let Err((name, e)) = dispatch_result {
            eprintln!(
                "veiland-core: plugin {:?} protocol error: {} — treating as dead",
                name, e
            );
            self.plugins[o][p] = None;
            return Ok(PostAction::Remove);
        }

        // 3. Buffer post-processing.
        //
        //    The texture has already been imported (handle_message did
        //    that above). Painting and BufferReleased now happen out of
        //    band — `repaint_lock_surface` paints when the compositor
        //    grants us a frame callback, then releases the sampled
        //    buffers on its way out. Here we just:
        //      a) mark the surface dirty so the next callback paints,
        //      b) acknowledge FrameDone to the plugin,
        //      c) kick a paint immediately if no callback is in flight
        //         (the very first frame after spawn has no prior
        //         commit to drive a callback; same for the case where
        //         the compositor stopped sending callbacks because
        //         nothing was dirty for a while).
        if is_buffer {
            if let Some(entry) = self.lock_surfaces.get_mut(o).and_then(|e| e.as_mut()) {
                entry.needs_paint = true;
            }

            if let Some(slot) = self.slot_mut(o, p) {
                if let Err(e) = slot.state.connection.send_frame_done() {
                    let name = slot.name.clone();
                    eprintln!(
                        "veiland-core: plugin {:?} send_frame_done failed: {}",
                        name, e
                    );
                    self.plugins[o][p] = None;
                    return Ok(PostAction::Remove);
                }
            }

            let kick_paint = self
                .lock_surfaces
                .get(o)
                .and_then(|e| e.as_ref())
                .is_some_and(|e| !e.frame_callback_pending);
            if kick_paint {
                self.repaint_lock_surface(o);
            }
        }
        Ok(PostAction::Continue)
    }
}
