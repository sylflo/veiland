// SPDX-License-Identifier: GPL-3.0-or-later

//! `CompositorHandler` impl: the `wl_surface.frame` callback that
//! drives the repaint cadence. The other callbacks (scale/transform
//! change, surface enter/leave) are no-ops we don't act on. Moved
//! verbatim from main.rs; no logic change.

use smithay_client_toolkit::compositor::CompositorHandler;

use wayland_client::{
    Connection, QueueHandle,
    protocol::{wl_output, wl_surface},
};

use crate::AppData;

impl CompositorHandler for AppData {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_factor: i32,
    ) {
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _new_transform: wl_output::Transform,
    ) {
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &wl_surface::WlSurface,
        _time: u32,
    ) {
        // SCTK demuxes wl_callback.done events for every surface we
        // requested .frame() on. We match the surface back to its
        // output by linear scan — at most ≤8 outputs in practice, and
        // the comparison style matches the M8 hotplug-rebind detection
        // elsewhere in this file.
        let output_idx = self.lock_surfaces.iter().position(|entry| {
            entry
                .as_ref()
                .is_some_and(|e| e.lock_surface.wl_surface() == surface)
        });
        let Some(output_idx) = output_idx else {
            // Stale callback for a destroyed lock surface (hotplug-out
            // race). Not an error — drop it.
            return;
        };

        // The callback we requested is consumed. repaint_lock_surface
        // requests a fresh one if it actually paints.
        if let Some(entry) = self.lock_surfaces[output_idx].as_mut() {
            entry.frame_callback_pending = false;
        }

        // Paint only if dirty. When nothing is animating we don't
        // request another callback — a static lockscreen stops
        // burning 60Hz no-op repaints.
        self.repaint_lock_surface(output_idx);
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &wl_surface::WlSurface,
        _output: &wl_output::WlOutput,
    ) {
    }
}
