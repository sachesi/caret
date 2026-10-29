//! The glyph atlas: where each rasterised glyph sits in the one texture the shaders
//! sample, the table of entries they look glyphs up in, and the pixels still to be copied
//! to the GPU.

use std::collections::HashMap;

use caret_protocol::Glyph;

use crate::fonts::{Fonts, Raster};

/// Texels on each side of the atlas texture.
pub const SIZE: u32 = 2048;

/// One glyph as the shaders read it.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpuGlyph {
    /// Texel position in the atlas, x in the low half.
    pub position: u32,
    /// Width in the low half, height in the high.
    pub size: u32,
    /// Where the pixels go from the cell's top left, signed; left in the low half.
    pub offset: u32,
    /// [`COLOR`] for an emoji drawn as it is, else a mask in the text colour.
    pub flags: u32,
}

pub const COLOR: u32 = 1;

/// Pixels to copy into the atlas texture before the next frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Upload {
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    /// Premultiplied RGBA.
    pub pixels: Vec<u8>,
}

/// Fills the texture in shelves: rows as tall as the tallest glyph put in them.
#[derive(Debug, Default)]
struct Shelves {
    /// The top of the open shelf, its height, and how far along it is filled.
    top: u32,
    height: u32,
    filled: u32,
}

impl Shelves {
    /// A place for a `width` by `height` rectangle, a texel apart from its neighbours so
    /// sampling never bleeds between glyphs.
    fn place(&mut self, width: u32, height: u32) -> Option<(u32, u32)> {
        let (padded_width, padded_height) = (width + 1, height + 1);
        if padded_width > SIZE || padded_height > SIZE {
            return None;
        }
        if self.filled + padded_width > SIZE {
            self.top += self.height;
            self.height = 0;
            self.filled = 0;
        }
        if self.top + padded_height > SIZE {
            return None;
        }
        let place = (self.filled, self.top);
        self.filled += padded_width;
        self.height = self.height.max(padded_height);
        Some(place)
    }
}

#[derive(Default)]
pub struct Atlas {
    shelves: Shelves,
    /// Each glyph's index in `entries` plus one; zero for one that draws nothing.
    indices: HashMap<Glyph, u32>,
    pub entries: Vec<GpuGlyph>,
    pub uploads: Vec<Upload>,
    full: bool,
}

impl Atlas {
    /// The number the cells refer to `glyph` by, rasterising it the first time: zero
    /// for one that draws nothing.
    pub fn index(&mut self, glyph: &Glyph, fonts: &mut Fonts) -> u32 {
        if let Some(&index) = self.indices.get(glyph) {
            return index;
        }
        let index = fonts
            .raster(glyph)
            .and_then(|raster| self.insert(raster))
            .unwrap_or(0);
        self.indices.insert(glyph.clone(), index);
        index
    }

    fn insert(&mut self, raster: Raster) -> Option<u32> {
        let Some((x, y)) = self.shelves.place(raster.width, raster.height) else {
            if !self.full {
                tracing::warn!("the glyph atlas is full; glyphs from now on are left out");
                self.full = true;
            }
            return None;
        };
        let halves = |low: u32, high: u32| (low & 0xffff) | (high << 16);
        self.entries.push(GpuGlyph {
            position: halves(x, y),
            size: halves(raster.width, raster.height),
            offset: halves(raster.left as u32, raster.top as u32),
            flags: if raster.color { COLOR } else { 0 },
        });
        self.uploads.push(Upload {
            x,
            y,
            width: raster.width,
            height: raster.height,
            pixels: raster.pixels,
        });
        u32::try_from(self.entries.len()).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shelves_fill_left_to_right_then_open_below() {
        let mut shelves = Shelves::default();
        assert_eq!(shelves.place(10, 20), Some((0, 0)));
        assert_eq!(shelves.place(10, 30), Some((11, 0)));
        assert_eq!(shelves.place(SIZE - 20, 5), Some((0, 31)));
        assert_eq!(shelves.place(SIZE + 1, 1), None);
    }

    #[test]
    fn a_full_atlas_takes_nothing_more() {
        let mut shelves = Shelves::default();
        assert_eq!(shelves.place(SIZE - 1, SIZE - 1), Some((0, 0)));
        assert_eq!(shelves.place(1, 1), None);
    }
}
