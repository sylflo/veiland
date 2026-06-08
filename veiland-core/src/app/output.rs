// SPDX-License-Identifier: GPL-3.0-or-later

//! `OutputHandler` impl: hotplug routing.
//!
//! `new_output` and `update_output` only *queue* topology changes onto
//! `AppData`; the actual surface create/recreate happens later in
//! `process_pending_hotplug` (the deferred-drain pattern, see M8).
//! `output_destroyed` runs the synchronous teardown for a departed
//! output. Moved verbatim from main.rs; no logic change.

use smithay_client_toolkit::output::{OutputHandler, OutputState};

use wayland_client::{Connection, Proxy, QueueHandle, protocol::wl_output};

use crate::{AppData, teardown_one_plugin};

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
        // settled. See the M8 retrospective in docs/m8-plan.md
        // for the trace evidence and design rationale.
        //
        // Hyprland twist: when a monitor is unplugged, Hyprland
        // sometimes re-advertises the *surviving* monitor's wl_output
        // under a new global. SCTK fires `new_output` for that new
        // global, with a name that matches our existing entry. The
        // server-side state is now tied to the new global; using
        // the old wl_output proxy on the next commit produces
        // "invalid object N". Detection: if `name` already has a
        // lock surface, route this to the rebound queue instead of
        // the arrival queue. The drain's rebound path destroys
        // the affected lock surface and creates a fresh one against
        // the new proxy. (Sway doesn't re-advertise, so this branch
        // never fires there and behavior is unchanged.)
        let name = self
            .output_state
            .info(&output)
            .and_then(|i| i.name)
            .unwrap_or_else(|| "<unnamed>".to_string());
        let already_have = self
            .lock_surfaces
            .iter()
            .any(|s| s.as_ref().map(|ls| ls.name == name).unwrap_or(false));
        if already_have {
            eprintln!(
                "[M8-TRACE] new_output fired: {:?} (REBIND of existing name, queued)",
                name
            );
            self.pending_outputs_rebound.push((output, name));
        } else {
            eprintln!("[M8-TRACE] new_output fired: {:?} (queued)", name);
            self.pending_outputs_arrived.push((output, name));
        }
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        output: wl_output::WlOutput,
    ) {
        // `update_output` fires for two distinct reasons we have to
        // distinguish:
        //
        // (a) Mode/scale change on a still-alive output. The wl_output
        //     proxy identity is the same; nothing to do here (a future
        //     M-step may want to re-send Configure to plugins with the
        //     new size).
        //
        // (b) SCTK rebound the wl_output global after a topology event
        //     (Hyprland fast-replug pattern: global_remove + global on
        //     the same local id within one batch). The proxy identity
        //     is DIFFERENT from what we have stored on `LockSurface`,
        //     and we need to destroy + recreate our lock surface
        //     against the fresh proxy. Otherwise the next commit on
        //     the still-alive surface trips "invalid object" because
        //     dmabuf-feedback / scanout state references the rebound
        //     identity.
        //
        // Discriminator: `output.id() != stored.wl_output.id()`.
        // ObjectId equality is Arc-based on the proxy's alive flag,
        // not on the wire-level id — so even when SCTK re-uses local
        // id 12 for the new binding, the new proxy's ObjectId is
        // distinct from the released one.
        let name = self
            .output_state
            .info(&output)
            .and_then(|i| i.name)
            .unwrap_or_else(|| "<unnamed>".to_string());
        let rebound = self
            .lock_surfaces
            .iter()
            .filter_map(|s| s.as_ref())
            .find(|ls| ls.name == name)
            .map(|ls| ls.wl_output.id() != output.id())
            .unwrap_or(false);
        if rebound {
            eprintln!(
                "[M8-TRACE] update_output fired: {:?} (REBOUND, queued for recreate)",
                name
            );
            self.pending_outputs_rebound.push((output, name));
        } else {
            eprintln!(
                "[M8-TRACE] update_output fired: {:?} (mode/scale change, no-op)",
                name
            );
        }
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
        //   1. Resolve the departed output's name (so we can find our
        //      matching slot — names are the only stable identity we
        //      keep on LockSurface).
        //   2. Tear down every plugin instance for that output via the
        //      existing Shutdown → grace → SIGTERM → SIGKILL sequence.
        //      Plugin calloop sources self-remove via PostAction::Remove
        //      when drive_plugin next sees EOF on the dropped socket.
        //   3. Replace both the lock_surfaces and plugins slots with
        //      None sentinels — preserves the (o, p) indices captured
        //      in surviving calloop closures.
        // Defensive throughout: this is compositor-driven input, never
        // crash on it.
        let name = match self.output_state.info(&output).and_then(|i| i.name) {
            Some(n) => n,
            None => {
                eprintln!(
                    "veiland-core: output_destroyed fired for an output \
                    with no cached name; skipping teardown (would not \
                    know which lock surface to tear down)"
                );
                eprintln!("[M8-TRACE] output_destroyed RETURNING (no name)");
                return;
            }
        };
        eprintln!("[M8-TRACE] output_destroyed ENTER: {:?}", name);

        let output_idx = self
            .lock_surfaces
            .iter()
            .position(|opt| opt.as_ref().map(|ls| ls.name == name).unwrap_or(false));
        let Some(output_idx) = output_idx else {
            eprintln!(
                "veiland-core: output_destroyed for {:?} but no matching \
                lock surface; nothing to tear down",
                name
            );
            eprintln!(
                "[M8-TRACE] output_destroyed RETURNING (no matching surface): {:?}",
                name
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
        for slot_opt in self.plugins[output_idx].iter_mut() {
            if let Some(slot) = slot_opt {
                if let Err(e) = slot.state.connection.send_shutdown() {
                    eprintln!(
                        "veiland-core: hotplug teardown: plugin {:?} \
                        send_shutdown failed: {} (continuing)",
                        slot.name, e
                    );
                }
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
            if let Some(egl_surface) = surface_ref.egl_surface.take() {
                if let Err(e) = self.renderer.egl.destroy_surface(self.renderer.egl_display, egl_surface) {
                    eprintln!(
                        "veiland-core: eglDestroySurface for {:?} failed: {:?} (continuing)",
                        surface_ref.name, e
                    );
                }
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
            "veiland-core: output {} teardown complete; slot is now None",
            name
        );
        eprintln!("[M8-TRACE] output_destroyed RETURNING (normal): {:?}", name);
    }
}
