// SPDX-License-Identifier: GPL-3.0-or-later

/// Convert pixel coordinates to OpenGL clip space.
///
/// Maps `(0, 0)` → `(-1, -1)` (top-left in screen space, bottom-left in
/// clip space — the Y-flip is handled by the lock-surface projection, so
/// plugins use screen-Y-down and this function preserves that convention).
pub fn px_to_clip(x: f32, y: f32, w: f32, h: f32) -> (f32, f32) {
    let cx = (x / w) * 2.0 - 1.0;
    let cy = (y / h) * 2.0 - 1.0;
    (cx, cy)
}

/// Tiny deterministic PRNG (xorshift32, Marsaglia).
///
/// Useful for seeding per-particle or per-element offsets without pulling in
/// the `rand` crate. Not suitable for anything cryptographic.
pub struct Rng(u32);

impl Rng {
    /// Create a new `Rng` with the given seed. Seed must be non-zero;
    /// the golden-ratio constant `0x9E3779B9` is a good default.
    pub fn new(seed: u32) -> Self {
        Self(seed.max(1))
    }

    /// Advance the state and return a value in `[0, 1)`.
    pub fn next_f32(&mut self) -> f32 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        self.0 = x.max(1);
        (x >> 8) as f32 / (1u32 << 24) as f32
    }
}
