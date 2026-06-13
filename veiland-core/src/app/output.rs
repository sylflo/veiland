// SPDX-License-Identifier: GPL-3.0-or-later

//! `OutputHandler` impl: hotplug routing.
//!
//! `new_output` queues new outputs onto `pending_outputs_arrived`;
//! the actual surface creation happens later in `process_pending_hotplug`
//! (deferred-drain pattern — SCTK must finish the event batch first).
//! `update_output` is a no-op (scale/size updates arrive via other paths).
//! `output_destroyed` runs the synchronous 4-phase teardown for a departed
//! output. Outputs are tracked by registry numeric ID, not by name.

use smithay_client_toolkit::output::{OutputHandler, OutputState};

use wayland_client::{Connection, QueueHandle, protocol::wl_output};

use crate::AppData;
use crate::plugin::teardown_one_plugin;

impl OutputHandler for AppData {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // Defer hotplug-in to `process_pending_hotplug` (called after
        // each `event_loop.dispatch()`). Doing EGL/session_lock work
        // synchronously inside the handler is unsafe: SCTK is mid-way
        // through processing the current event batch and may not have
        // finished binding the new wl_output globally yet. The
        // deferred drain runs after SCTK's internal state has
        // settled.
        //
        // We track by registry numeric ID (OutputInfo::id), not by
        // output name. This means a compositor that re-advertises a
        // surviving monitor under a new global ID (Hyprland quirk on
        // unplug) is handled as a normal remove + add with no special
        // case: output_destroyed removes the old slot by the old ID,
        // and this new_output queues an arrival with the new ID.
        let info = self.output_state.info(&output);
        let id = info.as_ref().map(|i| i.id).unwrap_or(0);
        let name = info
            .and_then(|i| i.name)
            .unwrap_or_else(|| "<unnamed>".to_string());
        eprintln!(
            "veiland-core: new_output fired: {:?} (id {}, queued)",
            name, id
        );
        self.pending_outputs_arrived.push((output, id, name));
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // Fires when a live output's mode, scale, or geometry changes.
        // Scale updates arrive via wp_fractional_scale_v1; size updates
        // arrive via SessionLockHandler::configure. Nothing to do here.
        let _ = output;
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // The compositor has already torn down the server-side
        // SessionLockSurface for this output; touching its EGL surface
        // from here on would block in eglSwapBuffers waiting on a
        // commit no one will ever ack. We:
        //   1. Resolve the departed output's registry ID (the stable
        //      numeric key we store on LockSurface) and name (for logs).
        //   2. Tear down every plugin instance for that output via the
        //      existing Shutdown → grace → SIGTERM → SIGKILL sequence.
        //      Plugin calloop sources self-remove via PostAction::Remove
        //      when drive_plugin next sees EOF on the dropped socket.
        //   3. Replace both the lock_surfaces and plugins slots with
        //      None sentinels — preserves the (o, p) indices captured
        //      in surviving calloop closures.
        // Defensive throughout: this is compositor-driven input, never
        // crash on it.
        let info = self.output_state.info(&output);
        let registry_id = match info.as_ref().map(|i| i.id) {
            Some(id) => id,
            None => {
                eprintln!(
                    "veiland-core: output_destroyed fired for an output \
                    with no cached info; skipping teardown"
                );
                return;
            }
        };
        let name = info
            .and_then(|i| i.name)
            .unwrap_or_else(|| format!("<id {}>", registry_id));
        eprintln!(
            "veiland-core: output_destroyed ENTER: {:?} (id {})",
            name, registry_id
        );

        let output_idx = self.lock_surfaces.iter().position(|opt| {
            opt.as_ref()
                .map(|ls| ls.registry_id == registry_id)
                .unwrap_or(false)
        });
        let Some(output_idx) = output_idx else {
            eprintln!(
                "veiland-core: output_destroyed for {:?} (id {}) but no matching \
                lock surface; nothing to tear down",
                name, registry_id
            );
            return;
        };

        eprintln!(
            "veiland-core: output {} disconnected, tearing down {} plugin instance(s)",
            name,
            self.plugins[output_idx]
                .iter()
                .filter(|s| s.is_some())
                .count()
        );

        // Phase 1: send Shutdown to every live plugin on this output.
        // Errors are non-fatal — the next phase will SIGTERM/SIGKILL
        // anything that's not already dying.
        for slot in self.plugins[output_idx].iter_mut().flatten() {
            if let Err(e) = slot.state.connection.send_shutdown() {
                eprintln!(
                    "veiland-core: hotplug teardown: plugin {:?} \
                    send_shutdown failed: {} (continuing)",
                    slot.name, e
                );
            }
        }

        // Phase 2: take each slot and run the per-plugin teardown
        // (grace period, escalate to SIGTERM/SIGKILL, reap zombie).
        for slot_opt in self.plugins[output_idx].iter_mut() {
            if let Some(slot) = slot_opt.take() {
                teardown_one_plugin(slot);
            }
        }

        // Phase 3: tear down EGL bits *before* dropping the LockSurface.
        // wayland_egl::WlEglSurface holds an internal reference to the
        // wl_surface; explicit destroy first keeps EGL from sending
        // commits to a dying surface.
        if let Some(surface_ref) = self.lock_surfaces[output_idx].as_mut() {
            if let Some(egl_surface) = surface_ref.egl_surface.take()
                && let Err(e) = self
                    .renderer
                    .egl
                    .destroy_surface(self.renderer.egl_display, egl_surface)
            {
                eprintln!(
                    "veiland-core: eglDestroySurface for {:?} failed: {:?} (continuing)",
                    surface_ref.name, e
                );
            }
            // WlEglSurface drops via the take() leaving None.
            surface_ref.egl_window = None;
        }
        // Phase 4: drop the SessionLockSurface, letting SCTK's
        // SessionLockSurfaceInner::Drop send ext_session_lock_surface_v1
        // .destroy() to the compositor. SCTK runs *our* output_destroyed
        // handler BEFORE releasing the wl_output (SCTK 0.20
        // src/output.rs lines 909-918), so destroying the lock surface
        // here happens while the wl_output is still alive — matching
        // swaylock / hyprlock's destruction order:
        //
        //     ext_session_lock_surface_v1.destroy()  ← our Drop here
        //     wl_surface.destroy()                   ← chained Drops
        //     wl_output.release()                    ← SCTK after we return
        //
        // The earlier `mem::forget` was a M7-debug-cycle empirical
        // workaround that traded a Drop-time crash for a later
        // swap-time crash on the surviving monitor (Hyprland) and
        // stranded keyboard focus on the destroyed surface (Sway —
        // the compositor doesn't re-route until the surface object
        // is actually destroyed). Both researched references just
        // drop the surface — see hyprlock's
        // src/core/hyprlock.cpp setGlobalRemove and swaylock-plugin's
        // main.c destroy_surface.
        self.lock_surfaces[output_idx] = None;
        eprintln!(
            "veiland-core: output {:?} (id {}) teardown complete; slot is now None",
            name, registry_id
        );
    }
}
