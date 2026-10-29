//! The frame a window draws itself where the compositor leaves that to it: a header bar
//! with the title and a close button, rounded corners, a shadow, and the edges outside
//! the window that resize it.

use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;
use swash::zeno::{Cap, Mask, PathBuilder, Stroke};

use crate::atlas::GpuGlyph;
use crate::fonts::Raster;

/// The header bar's height, in logical pixels as all sizes here are.
pub const HEADER: u32 = 47;
/// Room around a floating window for its shadow and the edges that resize it.
pub const MARGIN: u32 = 24;
/// How far outside the window its edges reach.
pub const EDGE: f64 = 12.0;
/// How far along the edges, from the window's corners, the corners that resize both ways
/// reach.
const CORNER: f64 = 12.0;
const RADIUS: f32 = 15.0;
/// The close button's circle, and the square around it a click hits.
const BUTTON: f32 = 24.0;
const BUTTON_HIT: f32 = 34.0;
/// From the window's right edge to the close button's centre.
const BUTTON_END: f32 = 23.0;
/// The close icon's square, and the cross in it.
const ICON: f32 = 16.0;
const CROSS: (f32, f32) = (4.0, 12.0);
const CROSS_WIDTH: f32 = 1.5;

/// Colours as 0xRRGGBBAA, not premultiplied.
const HEADER_COLOR: u32 = 0x2e2e_32ff;
const HEADER_BACKDROP: u32 = 0x2222_26ff;
const SHADE: u32 = 0x0000_065c;
const OUTLINE: u32 = 0x0000_063b;
const SHADOW: u32 = 0x0000_0680;
const SHADOW_BACKDROP: u32 = 0x0000_0640;
const FOREGROUND: u32 = 0xffff_ffff;
const BUTTON_COLOR: u32 = 0xffff_ff1a;
const BUTTON_HOVERED: u32 = 0xffff_ff26;
const BUTTON_PRESSED: u32 = 0xffff_ff4d;

pub const TOP_LEFT: u32 = 1;
pub const TOP_RIGHT: u32 = 2;
pub const BOTTOM_RIGHT: u32 = 4;
pub const BOTTOM_LEFT: u32 = 8;
const ALL_CORNERS: u32 = TOP_LEFT | TOP_RIGHT | BOTTOM_RIGHT | BOTTOM_LEFT;

/// A rectangle as the shaders read it, in device pixels: filled, rounded, blurred into a
/// shadow, or showing an atlas entry.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Quad {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// 0xRRGGBBAA, not premultiplied; for an atlas entry, the colour of a mask and the
    /// opacity of a coloured one.
    pub color: u32,
    pub radius: f32,
    /// Which corners [`Self::radius`] rounds.
    pub corners: u32,
    /// The atlas entry plus one; zero for a filled rectangle.
    pub glyph: u32,
    /// How far the edges fade, inside and out, for a shadow.
    pub blur: f32,
}

/// What the frame keeps in the atlas.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Drawing {
    /// The title, cut to fit this many device pixels.
    Title {
        text: String,
        width: u32,
    },
    Close,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Edge(ResizeEdge),
    Header,
    Close,
}

#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a separate state the compositor configures"
)]
pub struct Chrome {
    /// The window draws its frame: the compositor draws none.
    pub drawn: bool,
    /// Neither maximized nor tiled, so the window has a shadow, rounded corners and
    /// edges to resize it by.
    pub floating: bool,
    pub fullscreen: bool,
    pub maximized: bool,
    pub active: bool,
    pub title: String,
    pub hovered: Option<Part>,
    pub pressed: Option<Part>,
}

impl Chrome {
    /// Room around the window, on each side.
    pub fn margin(&self) -> u32 {
        if self.drawn && self.floating && !self.fullscreen {
            MARGIN
        } else {
            0
        }
    }

    pub fn header(&self) -> u32 {
        if self.drawn && !self.fullscreen {
            HEADER
        } else {
            0
        }
    }

    /// The part under `(x, y)`, from the corner of the surface of a window `size` big;
    /// none over the terminal or outside every part.
    pub fn part_at(&self, (x, y): (f64, f64), size: (u32, u32)) -> Option<Part> {
        let margin = f64::from(self.margin());
        let (x, y) = (x - margin, y - margin);
        let (width, height) = (f64::from(size.0), f64::from(size.1));
        if !((0.0..width).contains(&x) && (0.0..height).contains(&y)) {
            return if margin > 0.0 {
                edge(x, y, width, height).map(Part::Edge)
            } else {
                None
            };
        }
        if y >= f64::from(self.header()) {
            return None;
        }
        let reach = f64::from(BUTTON_HIT) / 2.0;
        let centre = (width - f64::from(BUTTON_END), f64::from(HEADER) / 2.0);
        if (x - centre.0).abs() < reach && (y - centre.1).abs() < reach {
            Some(Part::Close)
        } else {
            Some(Part::Header)
        }
    }

    /// The device pixels the title may take in a window `width` wide, clear of the
    /// close button on both sides so that it stays centred.
    pub fn title_width(width: u32, scale: f32) -> u32 {
        let buttons = 2.0 * (BUTTON_END + BUTTON_HIT / 2.0 + 6.0);
        ((width as f32 - buttons) * scale).max(0.0) as u32
    }

    /// The frame of a window `size` big at `scale`, in the order it is drawn, under the
    /// terminal's cells: `title` and `close` are atlas entries and the glyphs they index,
    /// and `line` the height of the title's line, in device pixels.
    pub fn quads(
        &self,
        size: (u32, u32),
        scale: f32,
        background: u32,
        title: Option<(u32, GpuGlyph)>,
        line: i32,
        close: Option<(u32, GpuGlyph)>,
    ) -> Vec<Quad> {
        if !self.drawn || self.fullscreen {
            return Vec::new();
        }
        let margin = self.margin() as f32 * scale;
        let (width, height) = (size.0 as f32 * scale, size.1 as f32 * scale);
        let header = HEADER as f32 * scale;
        let (radius, corners) = if self.floating {
            (RADIUS * scale, ALL_CORNERS)
        } else {
            (0.0, 0)
        };
        let rectangle = |x, y, width, height, color| Quad {
            x,
            y,
            width,
            height,
            color,
            radius,
            corners,
            ..Quad::default()
        };
        let mut quads = Vec::new();
        if self.floating {
            let spread = 2.0 * scale;
            quads.push(Quad {
                blur: 10.0 * scale,
                color: if self.active { SHADOW } else { SHADOW_BACKDROP },
                ..rectangle(
                    margin - spread,
                    margin - spread + 2.0 * scale,
                    width + 2.0 * spread,
                    height + 2.0 * spread,
                    0,
                )
            });
            quads.push(Quad {
                radius: radius + scale,
                ..rectangle(
                    margin - scale,
                    margin - scale,
                    width + 2.0 * scale,
                    height + 2.0 * scale,
                    OUTLINE,
                )
            });
        }
        quads.push(rectangle(margin, margin, width, height, background));
        let header_color = if self.active {
            HEADER_COLOR
        } else {
            HEADER_BACKDROP
        };
        quads.push(Quad {
            corners: corners & (TOP_LEFT | TOP_RIGHT),
            ..rectangle(margin, margin, width, header, header_color)
        });
        quads.push(Quad {
            radius: 0.0,
            ..rectangle(margin, margin + header - scale, width, scale, SHADE)
        });
        self.controls(
            &mut quads,
            (margin, width, header),
            scale,
            title,
            line,
            close,
        );
        quads
    }

    /// The title and the close button, in a header `header` high on a window `width`
    /// wide, `margin` from the surface's edges.
    fn controls(
        &self,
        quads: &mut Vec<Quad>,
        (margin, width, header): (f32, f32, f32),
        scale: f32,
        title: Option<(u32, GpuGlyph)>,
        line: i32,
        close: Option<(u32, GpuGlyph)>,
    ) {
        // Both are dimmed in a window without the focus.
        let dim = |color: u32| {
            if self.active {
                color
            } else {
                (color & !0xff) | ((color & 0xff) / 2)
            }
        };
        let middle = margin + header / 2.0;
        if let Some((index, glyph)) = title {
            let (glyph_width, _) = halves(glyph.size);
            let top = i32::from((glyph.offset >> 16) as u16 as i16);
            let x = (margin + (width - glyph_width as f32) / 2.0).floor();
            let y = (middle - line as f32 / 2.0).floor() + top as f32;
            quads.push(entry(x, y, index, glyph, dim(FOREGROUND)));
        }
        let centre = margin + width - BUTTON_END * scale;
        let pressed = self.pressed == Some(Part::Close) && self.hovered == Some(Part::Close);
        let button = if pressed {
            BUTTON_PRESSED
        } else if self.hovered == Some(Part::Close) {
            BUTTON_HOVERED
        } else {
            BUTTON_COLOR
        };
        let diameter = BUTTON * scale;
        quads.push(Quad {
            x: (centre - diameter / 2.0).round(),
            y: (middle - diameter / 2.0).round(),
            width: diameter,
            height: diameter,
            color: dim(button),
            radius: diameter / 2.0,
            corners: ALL_CORNERS,
            ..Quad::default()
        });
        if let Some((index, glyph)) = close {
            let (icon_width, icon_height) = halves(glyph.size);
            let x = (centre - icon_width as f32 / 2.0).round();
            let y = (middle - icon_height as f32 / 2.0).round();
            quads.push(entry(x, y, index, glyph, dim(FOREGROUND)));
        }
    }
}

/// The edge a point outside a window `width` by `height` resizes it by, if near enough.
fn edge(x: f64, y: f64, width: f64, height: f64) -> Option<ResizeEdge> {
    if x < -EDGE || y < -EDGE || x >= width + EDGE || y >= height + EDGE {
        return None;
    }
    let top = y < CORNER;
    let bottom = y >= height - CORNER;
    let left = x < CORNER;
    let right = x >= width - CORNER;
    Some(match (top, bottom, left, right) {
        (true, _, true, _) => ResizeEdge::TopLeft,
        (true, _, _, true) => ResizeEdge::TopRight,
        (_, true, true, _) => ResizeEdge::BottomLeft,
        (_, true, _, true) => ResizeEdge::BottomRight,
        (true, ..) => ResizeEdge::Top,
        (_, true, ..) => ResizeEdge::Bottom,
        (_, _, true, _) => ResizeEdge::Left,
        _ => ResizeEdge::Right,
    })
}

/// A quad showing atlas entry `index`, its pixels' top left at `(x, y)`.
fn entry(x: f32, y: f32, index: u32, glyph: GpuGlyph, color: u32) -> Quad {
    let (width, height) = halves(glyph.size);
    Quad {
        x,
        y,
        width: width as f32,
        height: height as f32,
        color,
        glyph: index,
        ..Quad::default()
    }
}

fn halves(packed: u32) -> (u32, u32) {
    (packed & 0xffff, packed >> 16)
}

/// The close button's cross at `scale`.
pub fn close_icon(scale: f32) -> Raster {
    let size = (ICON * scale).round() as u32;
    let (from, to) = (CROSS.0 * scale, CROSS.1 * scale);
    let mut path = Vec::new();
    path.move_to([from, from])
        .line_to([to, to])
        .move_to([to, from])
        .line_to([from, to]);
    let mut stroke = Stroke::new(CROSS_WIDTH * scale);
    stroke.cap(Cap::Butt);
    let mut cover = vec![0u8; (size * size) as usize];
    Mask::new(&path)
        .style(stroke)
        .size(size, size)
        .render_into(&mut cover, None);
    Raster {
        width: size,
        height: size,
        left: 0,
        top: 0,
        pixels: cover.iter().flat_map(|&a| [a, a, a, a]).collect(),
        color: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn floating() -> Chrome {
        Chrome {
            drawn: true,
            floating: true,
            active: true,
            ..Chrome::default()
        }
    }

    #[test]
    fn the_header_moves_the_window_and_the_button_closes_it() {
        let chrome = floating();
        let size = (800, 500);
        let m = f64::from(MARGIN);
        assert_eq!(
            chrome.part_at((m + 100.0, m + 10.0), size),
            Some(Part::Header)
        );
        assert_eq!(
            chrome.part_at((m + 777.0, m + 23.0), size),
            Some(Part::Close)
        );
        assert_eq!(chrome.part_at((m + 100.0, m + 60.0), size), None);
    }

    #[test]
    fn outside_a_floating_window_its_edges_and_corners_resize_it() {
        let chrome = floating();
        let size = (800, 500);
        let m = f64::from(MARGIN);
        assert_eq!(
            chrome.part_at((m - 5.0, m + 200.0), size),
            Some(Part::Edge(ResizeEdge::Left))
        );
        assert_eq!(
            chrome.part_at((m + 5.0, m - 5.0), size),
            Some(Part::Edge(ResizeEdge::TopLeft))
        );
        assert_eq!(
            chrome.part_at((m + 805.0, m + 495.0), size),
            Some(Part::Edge(ResizeEdge::BottomRight))
        );
        assert_eq!(
            chrome.part_at((m + 400.0, m + 510.0), size),
            Some(Part::Edge(ResizeEdge::Bottom))
        );
        assert_eq!(chrome.part_at((1.0, 1.0), size), None);
    }

    #[test]
    fn a_maximized_window_has_no_margin_and_a_fullscreen_one_no_header() {
        let mut chrome = Chrome {
            floating: false,
            ..floating()
        };
        assert_eq!((chrome.margin(), chrome.header()), (0, HEADER));
        assert_eq!(chrome.part_at((5.0, 5.0), (800, 500)), Some(Part::Header));
        chrome.fullscreen = true;
        assert_eq!((chrome.margin(), chrome.header()), (0, 0));
        assert_eq!(chrome.part_at((5.0, 5.0), (800, 500)), None);
        assert!(chrome.quads((800, 500), 1.0, 0, None, 0, None).is_empty());
    }

    #[test]
    fn the_close_icon_is_a_cross() {
        let icon = close_icon(1.0);
        assert_eq!((icon.width, icon.height), (16, 16));
        let alpha = |x: u32, y: u32| icon.pixels[((y * 16 + x) * 4 + 3) as usize];
        assert!(alpha(8, 8) > 200 && alpha(5, 5) > 100 && alpha(10, 5) > 100);
        assert_eq!(alpha(8, 1), 0);
    }
}
