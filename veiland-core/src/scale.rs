// SPDX-License-Identifier: GPL-3.0-or-later

//! Logical -> buffer pixel conversion for scaled outputs. `scale_120` uses
//! wp_fractional_scale_v1's encoding (120 = 1x, 180 = 1.5x).

/// Largest buffer side handed to EGL or a plugin; Configure rejects more.
pub const MAX_BUFFER_DIM: u32 = 8192;

/// Scale a logical length, rounding half away from zero like the
/// fractional-scale protocol. `as` saturates, so huge inputs can't wrap.
pub fn scale_u32(v: u32, scale_120: u32) -> u32 {
    (v as f64 * scale_120 as f64 / 120.0).round() as u32
}

pub fn scale_i32(v: i32, scale_120: u32) -> i32 {
    (v as f64 * scale_120 as f64 / 120.0).round() as i32
}

/// Buffer size for a surface of `logical` size, each side in [1, MAX_BUFFER_DIM].
pub fn physical_size(logical: (u32, u32), scale_120: u32) -> (u32, u32) {
    let side = |v| scale_u32(v, scale_120).clamp(1, MAX_BUFFER_DIM);
    (side(logical.0), side(logical.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn physical_size_matches_the_panel() {
        // Logical sizes a 2880x1800 panel reports at each scale.
        assert_eq!(physical_size((2304, 1440), 150), (2880, 1800));
        assert_eq!(physical_size((1920, 1200), 180), (2880, 1800));
        assert_eq!(physical_size((960, 600), 360), (2880, 1800));
        assert_eq!(physical_size((1920, 1080), 120), (1920, 1080));
    }

    #[test]
    fn physical_size_rounds_half_away_from_zero() {
        assert_eq!(physical_size((1707, 1), 180), (2561, 2));
    }

    #[test]
    fn physical_size_clamps_to_protocol_max() {
        assert_eq!(physical_size((5120, 2880), 240), (8192, 5760));
        assert_eq!(physical_size((0, 0), 120), (1, 1));
    }

    #[test]
    fn scale_i32_keeps_sign() {
        assert_eq!(scale_i32(100, 180), 150);
        assert_eq!(scale_i32(-100, 180), -150);
    }
}
