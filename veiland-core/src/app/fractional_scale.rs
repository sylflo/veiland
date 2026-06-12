// SPDX-License-Identifier: GPL-3.0-or-later

//! `Dispatch` impl for `wp_fractional_scale_v1`: handles `preferred_scale`
//! events and updates the matching `LockSurface::scale_120`.

use wayland_client::{Connection, Dispatch, QueueHandle};
use wayland_protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1;

use crate::AppData;

impl Dispatch<wp_fractional_scale_v1::WpFractionalScaleV1, ()> for AppData {
    fn event(
        state: &mut AppData,
        proxy: &wp_fractional_scale_v1::WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &(),
        _: &Connection,
        _: &QueueHandle<AppData>,
    ) {
        let wp_fractional_scale_v1::Event::PreferredScale { scale } = event else {
            return;
        };

        let new_scale_120 = scale.clamp(1, 9999);

        // Find which output_idx owns this fractional_scale object.
        let Some(output_idx) = state.lock_surfaces.iter().position(|opt| {
            opt.as_ref()
                .and_then(|s| s.fractional_scale.as_ref())
                .map(|fs| fs == proxy)
                .unwrap_or(false)
        }) else {
            return;
        };

        let Some(entry) = state.lock_surfaces[output_idx].as_mut() else {
            return;
        };

        if entry.scale_120 == new_scale_120 {
            return;
        }

        eprintln!(
            "veiland-core: output {}: fractional scale updated to {}/120 ({:.3}×)",
            entry.name,
            new_scale_120,
            new_scale_120 as f32 / 120.0,
        );

        entry.scale_120 = new_scale_120;
        entry.needs_paint = true;

        state.resend_configure_scale_for_output(output_idx, new_scale_120);
    }
}
