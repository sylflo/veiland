// SPDX-License-Identifier: GPL-3.0-or-later

//! GPU glyph atlas. See `docs/m10-plan.md` step 4.
//!
//! One `R8` GL texture (1024×1024 by default) holds every rasterized
//! glyph the plugin has drawn this session. A hash map keyed on
//! `(font_id, glyph_id, size_bin, subpixel_bin)` maps to the glyph's
//! `(u, v, w, h)` rect inside that texture. Drawing text is then
//! "compute one quad per glyph, sample the shared atlas, one draw call."
//!
//! Packing strategy: shelf packing. The atlas is divided into horizontal
//! stripes ("shelves") sized to the tallest glyph each holds. A new
//! glyph either fits onto an existing shelf or starts a new one below
//! the last. Cheap, predictable, "good enough" for the few-fonts /
//! few-sizes regime a lockscreen has.
//!
//! Eviction: flush-on-full. When the atlas can't fit a new glyph, drop
//! everything and start fresh. The next few frames pay the
//! re-rasterization cost; thereafter we're back to cache hits. Real LRU
//! is M11+ if profiling shows thrashing.
//!
//! Subpixel positioning: snapped to integer pixels in M10. cosmic-text
//! supports sub-pixel placement; we discretize the X bin to 0 so the
//! cache key collapses. Real subpixel is M12+ polish.
//!
//! Concurrency: none. The atlas is single-threaded; cosmic-text and our
//! plugin render loop both run on the same thread.

// gl crate functions are all `unsafe fn`. The crate-level deny stays in
// lib.rs to keep the rest of veiland-text unsafe-free; the atlas opts in
// here, deliberately scoped to the one file that needs GL.
#![allow(unsafe_code)]

use std::collections::HashMap;

/// Side length of the atlas texture, in physical pixels. 1024 × 1024 ×
/// 1 byte = 1 MB. See `docs/m10-plan.md` Q7 for the sizing rationale.
const ATLAS_SIZE: u32 = 1024;

/// Identifier for a glyph in the atlas. The tuple discretizes everything
/// that affects rasterization output: which font face, which glyph
/// within it, what pixel size, and (for future use) what subpixel
/// offset. In M10 the subpixel bin is always 0 — see module docs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct GlyphKey {
    pub font_id: u64,
    pub glyph_id: u16,
    pub size_px: u16,
    pub subpixel_bin: u8,
}

/// Where in the atlas a cached glyph lives. UV coordinates in [0, 1] —
/// the consumer (step 5's `Label::render`) feeds these straight into a
/// vertex buffer. No GL state involved; pure data.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct AtlasEntry {
    pub u_min: f32,
    pub v_min: f32,
    pub u_max: f32,
    pub v_max: f32,
}

/// One row of the atlas. `y_top` is the top edge of the shelf, `height`
/// is fixed by the first glyph placed on it (subsequent glyphs must be
/// shorter), `next_free_x` walks right as glyphs are added.
#[derive(Debug, Clone, Copy)]
struct Shelf {
    y_top: u32,
    height: u32,
    next_free_x: u32,
}

/// The atlas. Owned by `FontContext`; constructed lazily on the first
/// `Label::render` call (step 5) when a live GL context exists.
pub(crate) struct Atlas {
    /// GL texture name from `glGenTextures`. Single-channel `R8`.
    texture: u32,
    /// Side length in pixels (square). Pinned at construction, used to
    /// convert glyph pixel rects into UV coords.
    size: u32,
    /// Active shelves, in order of vertical placement (top to bottom).
    shelves: Vec<Shelf>,
    /// Y coordinate where the next new shelf would start. Equals
    /// `shelves.last().y_top + shelves.last().height`, or 0 when empty.
    next_shelf_y: u32,
    /// Glyph cache. Keyed on the discretized rasterization parameters;
    /// the entry tells us where in the atlas the glyph lives.
    entries: HashMap<GlyphKey, AtlasEntry>,
}

impl Atlas {
    /// Allocate a fresh atlas. Requires a live GL context — `glGenTextures`
    /// and `glTexImage2D` are GL calls.
    ///
    /// One-time `glTexImage2D` defines the texture's storage; every
    /// subsequent glyph upload uses `glTexSubImage2D` to write into a
    /// subrect of this same allocation. See module docs.
    pub(crate) fn new() -> Self {
        let mut texture: u32 = 0;
        // SAFETY: gl is FFI; caller (FontContext, called from Label::render)
        // guarantees a current GL context. Output `texture` is initialized
        // by glGenTextures before we read it.
        unsafe {
            gl::GenTextures(1, &mut texture);
            gl::BindTexture(gl::TEXTURE_2D, texture);
            // R8: single-channel coverage data, see module docs for why
            // this is not RGBA8. Width/height = ATLAS_SIZE; null data —
            // we're only *defining* storage here. Contents are written
            // glyph-by-glyph via glTexSubImage2D in `insert_bitmap`.
            gl::TexImage2D(
                gl::TEXTURE_2D,
                0,
                gl::R8 as i32,
                ATLAS_SIZE as i32,
                ATLAS_SIZE as i32,
                0,
                gl::RED,
                gl::UNSIGNED_BYTE,
                std::ptr::null(),
            );
            // Linear filter: glyph edges look smoother than nearest at
            // off-grid sample positions. Clamp-to-edge prevents bleed
            // between adjacent atlas slots when UV math has rounding
            // wobble at the boundary.
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MIN_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_MAG_FILTER, gl::LINEAR as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_S, gl::CLAMP_TO_EDGE as i32);
            gl::TexParameteri(gl::TEXTURE_2D, gl::TEXTURE_WRAP_T, gl::CLAMP_TO_EDGE as i32);
            // R8 rows aren't 4-byte aligned; tell GL not to expect padding.
            gl::PixelStorei(gl::UNPACK_ALIGNMENT, 1);
        }
        Self {
            texture,
            size: ATLAS_SIZE,
            shelves: Vec::new(),
            next_shelf_y: 0,
            entries: HashMap::new(),
        }
    }

    /// GL texture name. Step 5 binds this before issuing the draw call.
    pub(crate) fn texture(&self) -> u32 {
        self.texture
    }

    /// Look up a previously-uploaded glyph. Returns `None` if the glyph
    /// has never been seen at these (font, glyph_id, size, subpixel) bins.
    pub(crate) fn lookup(&self, key: GlyphKey) -> Option<AtlasEntry> {
        self.entries.get(&key).copied()
    }

    /// Insert a freshly rasterized glyph. Returns the atlas entry the
    /// caller should remember (and that future `lookup` calls will hit).
    ///
    /// `bitmap` is `w * h` bytes of coverage data, top-to-bottom rows,
    /// as cosmic-text/swash produce them. Empty glyphs (zero width or
    /// height) get a zero-area entry and no upload — saves the GL call.
    ///
    /// If the new glyph won't fit, the atlas flushes everything and
    /// tries once more. Flush-on-full is the M10 eviction policy; see
    /// module docs. The second attempt will only fail if the glyph
    /// itself is larger than the atlas, which we treat as the caller's
    /// bug and signal by panicking — a 1024-pixel-tall glyph would be a
    /// pathological config no real plugin produces.
    pub(crate) fn insert_bitmap(
        &mut self,
        key: GlyphKey,
        w: u32,
        h: u32,
        bitmap: &[u8],
    ) -> AtlasEntry {
        if w == 0 || h == 0 {
            let entry = AtlasEntry {
                u_min: 0.0,
                v_min: 0.0,
                u_max: 0.0,
                v_max: 0.0,
            };
            self.entries.insert(key, entry);
            return entry;
        }

        let rect = match self.try_pack(w, h) {
            Some(r) => r,
            None => {
                self.flush();
                self.try_pack(w, h).expect(
                    "glyph larger than the entire atlas — likely a runaway font_size; \
                     M10 ships with a 1024² atlas (see atlas.rs ATLAS_SIZE)",
                )
            }
        };

        // SAFETY: gl is FFI; we own `self.texture` and rect is bounded by
        // `self.size`. The bitmap slice must contain at least w*h bytes;
        // the caller (step 5) guarantees this when forwarding swash output.
        debug_assert!(
            bitmap.len() >= (w as usize) * (h as usize),
            "bitmap too short: got {} bytes, need {}x{}={}",
            bitmap.len(),
            w,
            h,
            (w as usize) * (h as usize),
        );
        unsafe {
            gl::BindTexture(gl::TEXTURE_2D, self.texture);
            // glTexSubImage2D — subrect update, see module docs. No
            // reallocation, no rebind cost beyond the one above.
            gl::TexSubImage2D(
                gl::TEXTURE_2D,
                0,
                rect.x as i32,
                rect.y as i32,
                rect.w as i32,
                rect.h as i32,
                gl::RED,
                gl::UNSIGNED_BYTE,
                bitmap.as_ptr() as *const _,
            );
        }

        let entry = AtlasEntry {
            u_min: rect.x as f32 / self.size as f32,
            v_min: rect.y as f32 / self.size as f32,
            u_max: (rect.x + rect.w) as f32 / self.size as f32,
            v_max: (rect.y + rect.h) as f32 / self.size as f32,
        };
        self.entries.insert(key, entry);
        entry
    }

    /// Find a position for a `w × h` glyph using shelf packing. Returns
    /// `None` if the glyph won't fit on any existing shelf and there's
    /// no room for a new one. Pure CPU work — no GL state touched.
    fn try_pack(&mut self, w: u32, h: u32) -> Option<PackedRect> {
        // Try to fit on an existing shelf: must be at least as tall as
        // the glyph, and have at least `w` pixels free at the right edge.
        for shelf in &mut self.shelves {
            if shelf.height >= h && shelf.next_free_x + w <= self.size {
                let rect = PackedRect {
                    x: shelf.next_free_x,
                    y: shelf.y_top,
                    w,
                    h,
                };
                shelf.next_free_x += w;
                return Some(rect);
            }
        }
        // Start a new shelf if there's vertical room left.
        if self.next_shelf_y + h <= self.size {
            let shelf = Shelf {
                y_top: self.next_shelf_y,
                height: h,
                next_free_x: w,
            };
            let rect = PackedRect {
                x: 0,
                y: shelf.y_top,
                w,
                h,
            };
            self.shelves.push(shelf);
            self.next_shelf_y += h;
            return Some(rect);
        }
        None
    }

    /// Drop every cached glyph and reset the shelf layout. The GL
    /// texture's contents become stale (we don't bother zeroing them —
    /// the next `glTexSubImage2D` will overwrite the regions that
    /// matter). Subsequent renders re-rasterize from cosmic-text.
    fn flush(&mut self) {
        self.shelves.clear();
        self.next_shelf_y = 0;
        self.entries.clear();
    }
}

impl Drop for Atlas {
    fn drop(&mut self) {
        // 0 is GL's "no such texture" sentinel: glGenTextures never
        // returns 0, so a 0 here means this Atlas was built outside of
        // `new()` (specifically, by the GL-free test fixture) and there
        // is nothing to delete. Skipping the FFI call also avoids
        // panicking in unit tests where the `gl` crate's function
        // pointers were never loaded.
        if self.texture == 0 {
            return;
        }
        // SAFETY: gl is FFI; if the GL context has gone away before us
        // (plugin shutdown order), glDeleteTextures is a no-op on an
        // invalid name rather than UB. Best-effort cleanup.
        unsafe {
            gl::DeleteTextures(1, &self.texture);
        }
    }
}

/// Output of `try_pack`. Private — only `insert_bitmap` cares.
#[derive(Debug, Clone, Copy)]
struct PackedRect {
    x: u32,
    y: u32,
    w: u32,
    h: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Construct an Atlas-equivalent that skips the GL calls so we can
    /// exercise shelf-packing math without a GL context. Mirrors
    /// `Atlas::new()` field-for-field except for the texture id, which
    /// stays 0. The packing methods don't read `texture`, only
    /// `insert_bitmap` does (for the BindTexture / TexSubImage2D pair),
    /// so the math-only tests below avoid it.
    fn fake_atlas() -> Atlas {
        Atlas {
            texture: 0,
            size: ATLAS_SIZE,
            shelves: Vec::new(),
            next_shelf_y: 0,
            entries: HashMap::new(),
        }
    }

    #[test]
    fn pack_first_glyph_opens_a_shelf() {
        let mut a = fake_atlas();
        let r = a.try_pack(40, 50).expect("first glyph must fit");
        assert_eq!((r.x, r.y, r.w, r.h), (0, 0, 40, 50));
        assert_eq!(a.shelves.len(), 1);
        assert_eq!(a.shelves[0].y_top, 0);
        assert_eq!(a.shelves[0].height, 50);
        assert_eq!(a.shelves[0].next_free_x, 40);
        assert_eq!(a.next_shelf_y, 50);
    }

    #[test]
    fn pack_second_glyph_fits_on_existing_shelf_if_shorter() {
        let mut a = fake_atlas();
        a.try_pack(40, 50).unwrap();
        let r = a.try_pack(30, 40).expect("shorter glyph must fit on shelf");
        assert_eq!(r.x, 40);
        assert_eq!(r.y, 0);
        assert_eq!(a.shelves.len(), 1);
        assert_eq!(a.shelves[0].next_free_x, 70);
    }

    #[test]
    fn pack_taller_glyph_opens_a_new_shelf() {
        let mut a = fake_atlas();
        a.try_pack(40, 50).unwrap();
        let r = a.try_pack(20, 80).expect("taller glyph must open new shelf");
        assert_eq!(r.y, 50);
        assert_eq!(a.shelves.len(), 2);
        assert_eq!(a.next_shelf_y, 130);
    }

    #[test]
    fn pack_returns_none_when_atlas_is_full() {
        let mut a = fake_atlas();
        // Open one shelf that occupies the full height. Subsequent
        // glyphs either fit on it (if narrow enough) or fail.
        a.try_pack(1, ATLAS_SIZE).unwrap(); // one tall sliver, height = ATLAS_SIZE
        // shelf is ATLAS_SIZE tall, next_free_x = 1, so width-1023 still fits.
        a.try_pack(ATLAS_SIZE - 1, 10).unwrap();
        // Now shelf is full at x; and no room for another shelf below
        // (next_shelf_y = ATLAS_SIZE). A new glyph that won't fit
        // on the existing shelf must fail.
        assert!(a.try_pack(2, 1).is_none());
    }

    #[test]
    fn flush_resets_packing_state() {
        let mut a = fake_atlas();
        a.try_pack(40, 50).unwrap();
        a.try_pack(20, 80).unwrap();
        a.flush();
        assert!(a.shelves.is_empty());
        assert_eq!(a.next_shelf_y, 0);
        assert!(a.entries.is_empty());
    }
}
