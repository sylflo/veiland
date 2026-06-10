// SPDX-License-Identifier: GPL-3.0-or-later

//! `SessionLockHandler` impl: the `ext-session-lock-v1` lifecycle.
//!
//! Lock-acquired / lock-refused transitions and the per-output
//! `configure` that creates (or resizes) the EGL window-surface and
//! paints the bootstrap frame. Moved verbatim from main.rs; no logic
//! change.

use khronos_egl as egl;

use smithay_client_toolkit::session_lock::{
    SessionLock, SessionLockHandler, SessionLockSurface, SessionLockSurfaceConfigure,
};

use wayland_client::{Connection, Proxy, QueueHandle};

use crate::{AppData, RunState};

impl SessionLockHandler for AppData {
    fn locked(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _session_lock: SessionLock) {
        eprintln!("veiland-core: session locked");
    }

    fn finished(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _session_lock: SessionLock,
    ) {
        eprintln!("Compositor refused the lock");
        self.run = RunState::Refused;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        session_lock_surface: SessionLockSurface,
        configure: SessionLockSurfaceConfigure,
        _serial: u32,
    ) {
        let (width, height) = configure.new_size;
        eprintln!(
            "[M8-TRACE] configure ENTER: wl_surface={:?} size=({}x{})",
            session_lock_surface.wl_surface().id(),
            width,
            height,
        );

        let target = session_lock_surface.wl_surface();
        // Look up the surface in our vec. Returning None here is *not*
        // a panic-worthy case: hotplug-out can put a Configure event
        // for the now-departed surface ahead of our output_destroyed
        // handler in the queue, so by the time we see the Configure
        // the slot is already None. (Or the compositor sent Configure
        // for a surface we never tracked — also not our problem to die
        // on.) Log and skip; the matching surface, if it ever existed,
        // is being torn down through the proper path.
        let Some(output_idx) = self.lock_surfaces.iter().position(|ls| {
            ls.as_ref()
                .map(|ls| ls.lock_surface.wl_surface() == target)
                .unwrap_or(false)
        }) else {
            eprintln!(
                "veiland-core: ignoring Configure for unknown/departed lock surface \
                ({}x{}) — the matching output was probably just unplugged",
                width, height
            );
            return;
        };
        let entry = self.lock_surfaces[output_idx]
            .as_mut()
            .expect("just matched Some");
        // Record the compositor-reported size in physical pixels and
        // note whether it changed. `size_changed` is true on the first
        // `configure` (None -> Some) and on any later resolution change
        // (mode switch mid-lock). We compare against the *old* value
        // before overwriting it, then — once the borrow on `entry` has
        // ended — resend Configure to this output's plugins so they
        // render at the true size instead of the 1080p spawn fallback.
        // Guarding on change means the steady state (same size every
        // frame callback) sends nothing.
        let new_size = (width, height);
        let size_changed = entry.surface_size != Some(new_size);
        entry.surface_size = Some(new_size);
        if entry.egl_window.is_none() {
            let egl_window =
                wayland_egl::WlEglSurface::new(target.id(), width as i32, height as i32)
                    .expect("WlEglSurface::new");

            let egl_surface = unsafe {
                self.renderer.egl.create_window_surface(
                    self.renderer.egl_display,
                    self.renderer.egl_config,
                    egl_window.ptr() as egl::NativeWindowType,
                    None,
                )
            }
            .expect("create_window_surface");
            entry.egl_window = Some(egl_window);
            entry.egl_surface = Some(egl_surface);
            println!(
                " -> [{}] created EGL surface ({}x{})",
                entry.name, width, height
            );
        } else {
            entry
                .egl_window
                .as_ref()
                .unwrap()
                .resize(width as i32, height as i32, 0, 0);
            println!(
                " -> [{}] resized EGL surface ({}x{})",
                entry.name, width, height
            );
        }

        // Copy the egl::Surface out so `entry`'s mutable borrow
        // ends here; we need an immutable self borrow later for
        // draw_password_field. egl::Surface is Copy (the rest
        // of this function already deref-copies it via *egl_surface).
        let egl_surface = *entry.egl_surface.as_ref().unwrap();
        self.renderer.egl
            .make_current(
                self.renderer.egl_display,
                Some(egl_surface),
                Some(egl_surface),
                Some(self.renderer.egl_context),
            )
            .expect("eglMakeCurrent");

        // Bootstrap paint: solid black, no plugin composition. Plugins
        // typically haven't shipped a Buffer yet at this point, so
        // sampling their (empty) slots here would show a frame with
        // gaps that the next paint then fills in — visible as
        // flicker / see-through against partially-transparent plugins
        // (vignette especially). Keeping the bootstrap content-free
        // means the *first* real paint is the one with all plugins,
        // not the second.
        //
        // We still need to swap once so the compositor has a frame to
        // show and our `wl_surface.frame` request has something to
        // attach to.
        unsafe {
            gl::Viewport(0, 0, width as i32, height as i32);
            gl::ClearColor(0.0, 0.0, 0.0, 1.0);
            gl::Clear(gl::COLOR_BUFFER_BIT);
        }

        // Request a frame callback so the cadence loop starts from
        // frame one. Without this, the surface only gets repainted
        // when something else dirties it (first plugin Buffer, key
        // press) — and until then the compositor double-buffer
        // alternates between this bootstrap frame and whatever
        // garbage was in the back buffer. Same ordering rule as
        // repaint_lock_surface: request BEFORE swap_buffers, because
        // swap commits.
        let target_clone = target.clone();
        target_clone.frame(&self.qh, target_clone.clone());
        if let Some(entry) = self.lock_surfaces[output_idx].as_mut() {
            entry.frame_callback_pending = true;
            // Leave needs_paint = true so the first frame-callback
            // firing immediately repaints with plugin content.
        }

        self.renderer.egl
            .swap_buffers(self.renderer.egl_display, egl_surface)
            .expect("eglSwapBuffers");

        // If the surface size is new or changed, tell this output's
        // plugins to render at it. Done last, after every borrow on
        // `self.lock_surfaces[output_idx]` above has ended, so the
        // `&mut self` resend is free of borrow conflicts. On a cold 4K
        // lock this is what upgrades plugins from the 1080p spawn
        // fallback to native resolution; on an unchanged size it is
        // skipped entirely.
        if size_changed {
            self.resend_configure_region_for_output(output_idx, width, height);
        }
    }
}
