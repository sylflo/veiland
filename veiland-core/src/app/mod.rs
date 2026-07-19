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
mod fractional_scale;
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
use zeroize::Zeroize;

use crate::plugin::{
    self, PluginSlot, current_time_for_configure, entry_matches_output, try_spawn_one,
};
use crate::region;
use crate::{AppData, RunState};

/// One output's lock surface plus the EGL window/surface bound to it.
pub(crate) struct LockSurface {
    /// The `wl_registry` global name (numeric ID) for this output.
    /// Used as the primary lookup key — stable within a compositor
    /// session and unambiguous even when Hyprland re-advertises a
    /// surviving monitor under a new global (that gets a new ID and
    /// is handled as a normal remove + add).
    pub(crate) registry_id: u32,
    /// Human-readable output name (xdg_output.name, e.g. "DP-1").
    /// Kept for logging and for matching against the user's
    /// `monitors = [...]` config entries via `entry_matches_output`.
    /// Not used as a lookup key.
    pub(crate) name: String,
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
    /// Output scale as 120ths. Initialised from `wl_output.scale * 120`;
    /// updated asynchronously by `wp_fractional_scale_v1.preferred_scale`
    /// events when the compositor supports the protocol.
    pub(crate) scale_120: u32,
    /// The `wp_fractional_scale_v1` object for this surface. `None` when
    /// `wp_fractional_scale_manager_v1` is not available.
    pub(crate) fractional_scale: Option<wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::WpFractionalScaleV1>,
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
        registry_id: u32,
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
        eprintln!(
            "veiland-core: output {} connected, creating lock surface",
            name
        );
        // Seed scale_120 from the integer wl_output.scale (always available).
        // If wp_fractional_scale_v1 is supported, a preferred_scale event will
        // arrive soon and override this with the true fractional value.
        let raw_scale = self
            .output_state
            .info(output)
            .map(|i| i.scale_factor)
            .unwrap_or(1);
        let seed_scale_120 = (raw_scale.max(1) as u32).min(83) * 120;

        let wl_surface = self.compositor_state.create_surface(&self.qh);
        let lock_surface_obj =
            session_lock.create_lock_surface(wl_surface.clone(), output, &self.qh);

        let fractional_scale = self
            .fractional_scale_manager
            .as_ref()
            .map(|mgr| mgr.get_fractional_scale(&wl_surface, &self.qh, ()));

        let lock_surface = LockSurface {
            registry_id,
            name,
            lock_surface: lock_surface_obj,
            egl_window: None,
            egl_surface: None,
            needs_paint: true,
            frame_callback_pending: false,
            surface_size: None,
            scale_120: seed_scale_120,
            fractional_scale,
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
        // Read scale_120 directly from the LockSurface — it was seeded from
        // wl_output.scale at surface creation and may already have been updated
        // by a wp_fractional_scale_v1.preferred_scale event.
        let scale_120: u32 = self.lock_surfaces[output_idx]
            .as_ref()
            .map(|s| s.scale_120)
            .unwrap_or(120);

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
                scale_120,
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
    /// `(o, p)` by value plus a freshly-minted tenancy serial; the
    /// serial is also stored on the slot. A source that outlives its
    /// slot (a `None` slot, or hotplug reusing the indices for a new
    /// plugin) identifies itself as stale on its next fire and
    /// `drive_plugin` removes it.
    pub(crate) fn register_plugin_source(&mut self, o: usize, p: usize) {
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
        let serial = self.next_plugin_source_serial;
        self.next_plugin_source_serial += 1;
        self.loop_handle
            .insert_source(
                Generic::new(plugin_fd, Interest::READ, Mode::Level),
                move |_event, _meta, state: &mut AppData| state.drive_plugin(o, p, serial),
            )
            .expect("register plugin fd with calloop");
        if let Some(slot) = self
            .plugins
            .get_mut(o)
            .and_then(|po| po.get_mut(p))
            .and_then(|s| s.as_mut())
        {
            slot.source_serial = Some(serial);
        }
    }

    /// Drain the pending-arrivals queue. Called after every
    /// `event_loop.dispatch()` returns — by that point SCTK has fully
    /// processed all events from the batch (registry bind/unbind,
    /// xdg_output.name, geometry, etc.) and our internal state is
    /// safe to mutate.
    ///
    /// For each newly-arrived output: create a lock surface (the
    /// compositor will send a Configure for it on a later dispatch),
    /// spawn the matching plugin instances, and register their sockets.
    ///
    /// Outputs are tracked by registry numeric ID. A compositor that
    /// re-advertises a surviving monitor under a new global ID
    /// (Hyprland unplug quirk) produces a normal remove + add cycle:
    /// `output_destroyed` removes the old slot by its ID, and the new
    /// arrival here creates a fresh slot with the new ID. No special
    /// rebind path needed.
    pub(crate) fn process_pending_hotplug(&mut self) {
        let arrived = std::mem::take(&mut self.pending_outputs_arrived);

        for (output, registry_id, name) in arrived {
            // Defensive: skip if we already have a slot for this
            // registry ID (shouldn't happen with correct ID tracking,
            // but guards against SCTK firing new_output twice).
            let already_have = self.lock_surfaces.iter().any(|s| {
                s.as_ref()
                    .map(|ls| ls.registry_id == registry_id)
                    .unwrap_or(false)
            });
            if already_have {
                eprintln!(
                    "veiland-core: arrival skipped: id {} ({:?}) already has a lock surface",
                    registry_id, name
                );
                continue;
            }
            eprintln!(
                "veiland-core: processing arrival: {:?} (id {})",
                name, registry_id
            );
            let Some(idx) = self.create_lock_surface_for_output(&output, registry_id, name.clone())
            else {
                continue;
            };
            self.spawn_plugins_for_output(idx, &name);
            // Register calloop sources for every plugin slot at this
            // index. Prior calloop sources self-removed when their
            // plugin sockets hit EOF; the new processes need fresh ones.
            for p in 0..self.plugins[idx].len() {
                self.register_plugin_source(idx, p);
            }
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
    /// 30 s is the chosen cadence: within
    /// half a minute of true wall-clock time, cheap enough to ignore.
    /// Other Configure fields (region, scale, output_name) are
    /// re-sent unchanged from `slot.last_configure`.
    ///
    /// A send failure kills the slot on the spot, like every other
    /// send site. Waiting for a read-side cleanup instead would be
    /// wrong for one of the two failure modes: a dead socket fails
    /// fast (EPIPE) and does hit EOF on the read side, but a plugin
    /// that stopped draining its socket fails via `SO_SNDTIMEO` after
    /// 500 ms and never becomes readable — its slot would survive and
    /// stall the calloop thread another 500 ms on every future tick.
    /// The stale calloop source self-removes on its next fire
    /// (`drive_plugin`'s slot-gone path).
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
                         — treating as dead",
                        slot.name, e
                    );
                    plugin::kill_slot(slot_opt);
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
    /// the time tick which advances everywhere) and overriding the
    /// region dims instead of time. The next periodic tick refreshes
    /// the time field as usual.
    ///
    /// `surface_w/h` are the *new surface* size. What each plugin is told
    /// is decided per-slot by `region::configure_dims`: a full-surface
    /// plugin (`region = None`) gets the new surface dims at origin (0, 0)
    /// — full-surface placement unchanged, only the size moves; a region
    /// plugin gets its region's own absolute (x, y, w, h), which does not
    /// depend on the surface and so is re-sent unchanged. Without the
    /// per-slot decision a mode change would silently revert a region
    /// plugin to full-surface (the pre-fix bug on the resend path).
    ///
    /// Plugin-input rule applies: a send failure logs and kills that
    /// slot, it never panics. See `process_periodic_tick` for why the
    /// slot must die here rather than wait for a read-side cleanup.
    pub(crate) fn resend_configure_region_for_output(
        &mut self,
        output_idx: usize,
        surface_w: u32,
        surface_h: u32,
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
            // Re-resolve the region against the new surface size: an
            // anchored region must re-anchor to the resized surface (a
            // mode change moves the corner), a pixel region resolves to
            // itself. Cache the result for the composite path.
            slot.resolved_region = slot
                .region_spec
                .as_ref()
                .map(|spec| spec.resolve(surface_w, surface_h));
            let (region_x, region_y, region_w, region_h) =
                region::configure_dims(slot.resolved_region.as_ref(), surface_w, surface_h);
            let next = Configure {
                region_x,
                region_y,
                region_w,
                region_h,
                ..prev
            };
            if let Err(e) = slot.state.connection.send_configure(next.clone()) {
                eprintln!(
                    "veiland-core: resize resend: send_configure to plugin {:?} failed: {} \
                     — treating as dead",
                    slot.name, e
                );
                plugin::kill_slot(slot_opt);
                continue;
            }
            slot.last_configure = Some(next);
        }
    }

    /// Re-send `Configure` to every live plugin on one output with an
    /// updated `scale_120`. Called when a `wp_fractional_scale_v1.
    /// preferred_scale` event arrives for that output's surface.
    pub(crate) fn resend_configure_scale_for_output(&mut self, output_idx: usize, scale_120: u32) {
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
            let next = veiland_protocol::Configure { scale_120, ..prev };
            if let Err(e) = slot.state.connection.send_configure(next.clone()) {
                eprintln!(
                    "veiland-core: scale resend: send_configure to plugin {:?} failed: {} \
                     — treating as dead",
                    slot.name, e
                );
                plugin::kill_slot(slot_opt);
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

        self.renderer
            .egl
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
        crate::gl_debug::check_gl("repaint: viewport/clear/blend setup");

        for slot in self.plugins[output_idx].iter().flatten() {
            let rect = region::region_to_clip_rect(slot.resolved_region.as_ref(), w, h);
            // Pick the compositor variant matching how this plugin's dmabuf
            // bound at import: the plain sampler2D program for TEXTURE_2D, the
            // samplerExternalOES program for external-only dmabufs (NVIDIA's
            // LINEAR/CPU path). Both share compositor_vbo (same unit quad +
            // a_pos). A slot with no texture never draws (composite bails on
            // texture.is_none()), so defaulting to TEXTURE_2D is harmless. If
            // the texture is external but the ext program failed to build,
            // compositor_for yields program 0 and composite skips the draw
            // (region stays black) instead of crashing.
            let target = slot
                .state
                .texture
                .as_ref()
                .map_or(gl::TEXTURE_2D, |t| t.target);
            let (program, sampler_loc, rect_loc) = self.renderer.compositor_for(target);
            slot.state.composite(
                program,
                self.renderer.compositor_vbo,
                sampler_loc,
                rect_loc,
                rect,
            );
        }

        // The password field (box + dots) paints on top of any plugins —
        // see the matching note in SessionLockHandler::configure.
        self.renderer.draw_password_field(
            &self.config.password,
            self.auth.char_count(),
            self.auth_state,
            self.modifiers.caps_lock,
            w,
            h,
        );
        crate::gl_debug::check_gl("repaint: composite loop + password field");

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

        match self
            .renderer
            .egl
            .swap_buffers(self.renderer.egl_display, egl_surface)
        {
            Ok(()) => {}
            // Same transient case as the bootstrap swap in lock.rs: a
            // surface invalidated mid-repaint by a hotplug storm. Skip
            // and retry next paint. Returning here is load-bearing: it
            // skips release_sampled_buffers (we presented nothing) and
            // leaves needs_paint = true (cleared only on the Ok path
            // below), so the next paint retries.
            Err(egl::Error::BadSurface) => {
                eprintln!(
                    "veiland-core: repaint swap_buffers for {:?} returned \
                    BadSurface (stale after hotplug); skipping, will retry",
                    self.lock_surfaces[output_idx]
                        .as_ref()
                        .map(|s| s.name.as_str())
                        .unwrap_or("<gone>")
                );
                return;
            }
            Err(e) => panic!("eglSwapBuffers (repaint): {e:?}"),
        }

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

        // Egress sync: wait until the host GPU has finished sampling the
        // plugin dmabufs before telling plugins to reuse them. Prefer the
        // bounded fence wait; if the display cannot even create a core
        // fence sync, fall back to glFinish — unbounded, but the
        // alternative is never sending BufferReleased, which freezes
        // every plugin after its first frame (protocol.md §7.3 requires
        // the release on both sync paths).
        match plugin::create_host_fence(&self.renderer.egl, self.renderer.egl_display) {
            Ok(fence) => {
                let wait_result =
                    plugin::wait_fence(&self.renderer.egl, self.renderer.egl_display, &fence);
                plugin::release_fence(&self.renderer.egl, self.renderer.egl_display, fence);
                if let Err(e) = wait_result {
                    // Bounded wait timed out: the host GPU is wedged. Skip
                    // the releases this paint rather than fall through to an
                    // unbounded glFinish on a GPU we just watched hang for a
                    // second. current_buffer_id stays set, so the next
                    // successful paint releases the buffers.
                    eprintln!("egress fence wait failed: {}", e);
                    return;
                }
            }
            Err(e) => {
                static GLFINISH_FALLBACK_WARN: std::sync::Once = std::sync::Once::new();
                GLFINISH_FALLBACK_WARN.call_once(|| {
                    eprintln!(
                        "veiland-core: egress fence create failed ({}); falling back to \
                        glFinish before buffer releases",
                        e
                    );
                });
                unsafe { gl::Finish() };
            }
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
                plugin::kill_slot(&mut self.plugins[output_idx][p]);
            }
        }
    }

    /// Act on the auth worker's verdict for one attempt. Runs on the main
    /// thread (calloop channel handler), so it may touch the Wayland lock
    /// object and repaint. `ok` is true when PAM authenticated.
    ///
    /// Security-critical: this is where the unlock decision is committed.
    /// The worker only computes a verdict; taking the session lock and
    /// unlocking happens here, on the trusted main thread.
    pub(crate) fn handle_auth_verdict(&mut self, ok: bool) {
        // A verdict can arrive after we've already left the running state
        // (compositor forced unlock, teardown). Ignore stale verdicts.
        if !matches!(self.run, RunState::Running) {
            return;
        }

        // Verdict resolved: unlock the keyboard handler either way.
        self.is_checking = false;

        if ok {
            if let Some(lock) = self.session_lock.take() {
                lock.unlock();
                self.conn.roundtrip().expect("flush unlock");
            }
            self.run = RunState::UnlockedCleanly;
            return;
        }

        // Failure: buffer was already cleared by take_password(). User
        // retypes. Show the fail indicator, then reset to Idle after
        // 1500ms (same timer as before).
        self.auth_state = crate::AuthState::Failed;

        use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
        let _ = self.loop_handle.insert_source(
            Timer::from_duration(std::time::Duration::from_millis(1500)),
            |_, _, state: &mut crate::AppData| {
                state.auth_state = crate::AuthState::Idle;
                for entry in state.lock_surfaces.iter_mut().flatten() {
                    entry.needs_paint = true;
                }
                state.repaint_lock_surfaces();
                TimeoutAction::Drop
            },
        );

        for entry in self.lock_surfaces.iter_mut().flatten() {
            entry.needs_paint = true;
        }
        self.repaint_lock_surfaces();
    }

    /// Handle one key event from the keyboard: the keystroke →
    /// password-buffer → PAM → unlock path. Called from both
    /// `press_key` and `repeat_key`. Security-critical: see CLAUDE.md
    /// "Trust boundaries" — this is the only place the password buffer
    /// is fed and the only place the unlock decision is made.
    pub(crate) fn handle_key(&mut self, event: &KeyEvent) {
        // While a verdict is pending, lock input entirely: one PAM attempt
        // at a time, and the password buffer can't be mutated mid-flight.
        // Cleared in handle_auth_verdict when the worker replies.
        if self.is_checking {
            return;
        }

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
                // Copy the password out of the mlock'd buffer (which is
                // cleared here, synchronously) and hand it to the worker.
                // The blocking PAM call runs off the event loop; the
                // verdict comes back to handle_auth_verdict. None means an
                // interior NUL slipped in — treat as nothing to verify.
                let Some(password) = self.auth.take_password() else {
                    return;
                };
                if let Err(send_err) = self.auth_tx.send(crate::AuthRequest { user, password }) {
                    // Worker thread is gone (it panicked — see the Closed
                    // arm on the verdict channel in main.rs). The SendError
                    // hands the request back; scrub the password copy
                    // before it drops (same pattern as PasswordConv::drop),
                    // then surface the failure like a wrong password so the
                    // user gets feedback instead of silence.
                    eprintln!("auth: worker thread unavailable, dropping attempt");
                    let mut bytes = send_err.0.password.into_bytes_with_nul();
                    bytes.zeroize();
                    self.handle_auth_verdict(false);
                    return;
                }
                // Lock input and show the checking state until the verdict
                // returns. Buffer is already cleared, so the indicator
                // reflects char_count() == 0.
                self.is_checking = true;
                self.auth_state = crate::AuthState::Checking;
                buffer_changed = true;
            }
            Keysym::BackSpace => {
                if self.auth_state == crate::AuthState::Failed {
                    self.auth_state = crate::AuthState::Idle;
                }
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
                    if self.auth_state == crate::AuthState::Failed {
                        self.auth_state = crate::AuthState::Idle;
                    }
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

    pub(crate) fn drive_plugin(
        &mut self,
        o: usize,
        p: usize,
        source_serial: u64,
    ) -> std::io::Result<PostAction> {
        // 1. Recv. Borrow scoped to this block.
        let recv_result = {
            let Some(slot) = self.slot_mut(o, p) else {
                // Slot was nulled by an earlier event on this fd
                // before calloop drained the queue. Remove the source.
                return Ok(PostAction::Remove);
            };
            // Stale-source guard: this source was registered for one
            // tenancy of (o, p). If hotplug reused the indices for a
            // new plugin while the old EOF-readable dup was still
            // registered (level-triggered: it fires on every dispatch
            // forever), polling here would hit the new tenant's socket
            // — WouldBlock, PostAction::Continue, and a pinned core.
            // 1fe5f63 fixed the hang variant of this race; the serial
            // check retires the spin variant. The new tenant has its
            // own source.
            if slot.source_serial != Some(source_serial) {
                return Ok(PostAction::Remove);
            }
            slot.state.connection.recv_message()
        };

        let (msg, fds) = match recv_result {
            Ok(t) => t,
            // WouldBlock: a non-blocking runtime read found nothing queued
            // — a spurious/stale readiness fire (e.g. a not-yet-removed
            // source firing against a reused hotplug slot). Not a plugin
            // failure: leave the slot intact and keep the source, we just
            // had nothing to do this wakeup.
            Err(plugin::HostError::WouldBlock) => return Ok(PostAction::Continue),
            Err(e) => {
                let name = self.plugin_name_for_log(o, p);
                eprintln!(
                    "veiland-core: plugin {:?} disconnected or violated protocol: {}",
                    name, e
                );
                plugin::kill_slot(&mut self.plugins[o][p]);
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
            plugin::kill_slot(&mut self.plugins[o][p]);
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

            if let Some(slot) = self.slot_mut(o, p)
                && let Err(e) = slot.state.connection.send_frame_done()
            {
                let name = slot.name.clone();
                eprintln!(
                    "veiland-core: plugin {:?} send_frame_done failed: {}",
                    name, e
                );
                plugin::kill_slot(&mut self.plugins[o][p]);
                return Ok(PostAction::Remove);
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
