//! The frame a window draws itself where the compositor leaves that to it: a header bar
//! with the title and a close button, rounded corners, a shadow, and the edges outside
//! the window that resize it.

use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;
use swash::zeno::{Cap, Mask, PathBuilder, Stroke};

use crate::atlas::GpuGlyph;
use crate::fonts::Raster;

/// The header bar's height, in logical pixels as all sizes here are.
pub const HEADER: u32 = 47;
/// The strip of tabs under the header, shown while there are two or more.
pub const TAB_BAR: u32 = 38;
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
/// The new tab button's square, from the window's left edge, and its corners' radius.
const FLAT_BUTTON: (f32, f32) = (6.0, 34.0);
const FLAT_RADIUS: f32 = 9.0;
/// Between the strip's edges and its tabs, and around each tab in its share.
const TAB_INSET: f32 = 3.0;
const TAB_RADIUS: f32 = 9.0;
/// From a tab's right edge to its close button's centre.
const TAB_CLOSE_END: f32 = 19.0;
/// The icons' square, and the strokes drawn in it.
const ICON: f32 = 16.0;
const CROSS: (f32, f32) = (4.0, 12.0);
const PLUS: (f32, f32) = (3.0, 13.0);
const STROKE: f32 = 1.5;

/// Colours as 0xRRGGBBAA, not premultiplied.
const OUTLINE: u32 = 0x0000_063b;
const SHADOW: u32 = 0x0000_0680;
const SHADOW_BACKDROP: u32 = 0x0000_0640;

/// The header bar's colours in one style.
struct Colors {
    header: u32,
    /// The header in a window without the focus.
    backdrop: u32,
    /// The line under the header.
    shade: u32,
    foreground: u32,
    /// The close button's circle, then hovered, then pressed; the first is also the
    /// selected tab's.
    button: [u32; 3],
    /// A flat button or tab hovered, then pressed.
    flat: [u32; 2],
}

const DARK: Colors = Colors {
    header: 0x2e2e_32ff,
    backdrop: 0x2222_26ff,
    shade: 0x0000_065c,
    foreground: 0xffff_ffff,
    button: [0xffff_ff1a, 0xffff_ff26, 0xffff_ff4d],
    flat: [0xffff_ff12, 0xffff_ff29],
};

const LIGHT: Colors = Colors {
    header: 0xffff_ffff,
    backdrop: 0xfafa_fbff,
    shade: 0x0000_061f,
    foreground: 0x0000_06cc,
    button: [0x0000_0614, 0x0000_061f, 0x0000_063d],
    flat: [0x0000_060e, 0x0000_0621],
};

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
    /// The title, in bold, cut to fit this many device pixels.
    Title {
        text: String,
        width: u32,
    },
    /// A tab's title, the same in the regular weight.
    Tab {
        text: String,
        width: u32,
    },
    Close,
    NewTab,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Edge(ResizeEdge),
    Header,
    Close,
    NewTab,
    /// The tab at this index, and its close button.
    Tab(usize),
    CloseTab(usize),
}

impl Part {
    /// The part looks different while the pointer is over it.
    fn shows_hover(self) -> bool {
        !matches!(self, Self::Edge(_) | Self::Header)
    }
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
    /// The header bar is shown; hidden, the window keeps its other parts.
    pub header_bar: bool,
    pub dark: bool,
    pub title: String,
    /// The tabs' titles, and which one is shown.
    pub tabs: Vec<String>,
    pub selected: usize,
    pub hovered: Option<Part>,
    pub pressed: Option<Part>,
}

impl Chrome {
    /// The window draws its frame: a background with rounded corners, an outline and a
    /// shadow when floating, and the header bar.
    pub fn framed(&self) -> bool {
        self.drawn && !self.fullscreen
    }

    /// Room around the window, on each side.
    pub fn margin(&self) -> u32 {
        if self.drawn && self.floating && !self.fullscreen {
            MARGIN
        } else {
            0
        }
    }

    pub fn header(&self) -> u32 {
        if self.drawn && self.header_bar && !self.fullscreen {
            HEADER
        } else {
            0
        }
    }

    pub fn tab_bar(&self) -> u32 {
        if self.tabs.len() > 1 { TAB_BAR } else { 0 }
    }

    /// The height of the bars above the terminal.
    pub fn top(&self) -> u32 {
        self.header() + self.tab_bar()
    }

    /// Whether a change of the part under the pointer from `old` to `new` changes how
    /// the frame looks.
    pub fn hover_shows(old: Option<Part>, new: Option<Part>) -> bool {
        old.filter(|part| part.shows_hover()) != new.filter(|part| part.shows_hover())
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
        let header = f64::from(self.header());
        if y < header {
            let within =
                |(cx, cy): (f64, f64), reach: f64| (x - cx).abs() < reach && (y - cy).abs() < reach;
            let middle = f64::from(HEADER) / 2.0;
            let (left, side) = (f64::from(FLAT_BUTTON.0), f64::from(FLAT_BUTTON.1));
            return Some(
                if within(
                    (width - f64::from(BUTTON_END), middle),
                    f64::from(BUTTON_HIT) / 2.0,
                ) {
                    Part::Close
                } else if within((left + side / 2.0, middle), side / 2.0) {
                    Part::NewTab
                } else {
                    Part::Header
                },
            );
        }
        if y >= header + f64::from(self.tab_bar()) {
            return None;
        }
        let (index, (left, share)) = self
            .shares(width as f32)
            .enumerate()
            .find(|(_, (left, share))| (f64::from(*left)..f64::from(left + share)).contains(&x))?;
        let close = f64::from(left + share - TAB_CLOSE_END);
        let middle = header + f64::from(TAB_BAR) / 2.0;
        let reach = f64::from(BUTTON) / 2.0;
        Some(if (x - close).abs() < reach && (y - middle).abs() < reach {
            Part::CloseTab(index)
        } else {
            Part::Tab(index)
        })
    }

    /// Each tab's share of the strip of a window `width` wide: its left edge and width.
    fn shares(&self, width: f32) -> impl Iterator<Item = (f32, f32)> {
        let count = self.tabs.len().max(1);
        let share = (width - 2.0 * TAB_INSET) / count as f32;
        (0..self.tabs.len()).map(move |index| (TAB_INSET + index as f32 * share, share))
    }

    /// The frame of a window `size` big at `scale`, in the order it is drawn, under the
    /// terminal's cells. `draw` gives the atlas entry of a drawing and the glyph it
    /// indexes, and `line` is the height of a line of text, in device pixels.
    pub fn quads(
        &self,
        size: (u32, u32),
        scale: f32,
        background: u32,
        line: i32,
        mut draw: impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) -> Vec<Quad> {
        let mut quads = Vec::new();
        let framed = self.framed();
        if framed {
            self.frame(&mut quads, size, scale, background);
        }
        let painter = Painter {
            chrome: self,
            scale,
            margin: self.margin() as f32 * scale,
            width: size.0 as f32 * scale,
            line,
        };
        let top = self.top() as f32 * scale;
        if top > 0.0 {
            let colors = self.colors();
            let color = if self.active {
                colors.header
            } else {
                colors.backdrop
            };
            let corners = if self.floating && framed {
                TOP_LEFT | TOP_RIGHT
            } else {
                0
            };
            let (margin, width) = (painter.margin, painter.width);
            quads.push(Quad {
                x: margin,
                y: margin,
                width,
                height: top,
                color,
                radius: RADIUS * scale,
                corners,
                ..Quad::default()
            });
            quads.push(Quad {
                x: margin,
                y: margin + top - scale,
                width,
                height: scale,
                color: colors.shade,
                ..Quad::default()
            });
        }
        if self.header() > 0 {
            painter.header(&mut quads, &mut draw);
        }
        if self.tab_bar() > 0 {
            painter.tabs(&mut quads, &mut draw);
        }
        quads
    }

    /// The shadow, the outline and the window's background.
    fn frame(&self, quads: &mut Vec<Quad>, size: (u32, u32), scale: f32, background: u32) {
        let margin = self.margin() as f32 * scale;
        let (width, height) = (size.0 as f32 * scale, size.1 as f32 * scale);
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
    }

    fn colors(&self) -> &'static Colors {
        if self.dark { &DARK } else { &LIGHT }
    }

    /// `color`, dimmed in a window without the focus.
    fn dim(&self, color: u32) -> u32 {
        if self.active {
            color
        } else {
            (color & !0xff) | ((color & 0xff) / 2)
        }
    }

    /// Whether the pointer is over `part` and, with `pressed`, also held down on it.
    fn over(&self, part: Part, pressed: bool) -> bool {
        self.hovered == Some(part) && (!pressed || self.pressed == Some(part))
    }
}

/// Draws the bars' contents at one size.
struct Painter<'a> {
    chrome: &'a Chrome,
    scale: f32,
    /// In device pixels, as the rest.
    margin: f32,
    width: f32,
    line: i32,
}

impl Painter<'_> {
    /// The new tab button, the title and the close button.
    fn header(
        &self,
        quads: &mut Vec<Quad>,
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) {
        let (chrome, scale, margin, width) = (self.chrome, self.scale, self.margin, self.width);
        let colors = chrome.colors();
        let middle = margin + HEADER as f32 * scale / 2.0;
        let side = FLAT_BUTTON.1 * scale;
        let left = (margin + FLAT_BUTTON.0 * scale).round();
        let top = (middle - side / 2.0).round();
        if chrome.over(Part::NewTab, false) {
            let shade = colors.flat[usize::from(chrome.over(Part::NewTab, true))];
            quads.push(rounded(
                left,
                top,
                side,
                side,
                FLAT_RADIUS * scale,
                chrome.dim(shade),
            ));
        }
        if let Some(icon) = draw(&Drawing::NewTab) {
            quads.push(centred(
                left + side / 2.0,
                middle,
                icon,
                chrome.dim(colors.foreground),
            ));
        }
        let room = width - 2.0 * (BUTTON_END + BUTTON_HIT / 2.0 + 6.0) * scale;
        if !chrome.title.is_empty() && room > 0.0 {
            let title = Drawing::Title {
                text: chrome.title.clone(),
                width: room as u32,
            };
            if let Some(label) = draw(&title) {
                quads.push(self.label(margin + width / 2.0, middle, label, colors.foreground));
            }
        }
        let centre = margin + width - BUTTON_END * scale;
        let shade = if chrome.over(Part::Close, true) {
            colors.button[2]
        } else if chrome.over(Part::Close, false) {
            colors.button[1]
        } else {
            colors.button[0]
        };
        let diameter = BUTTON * scale;
        quads.push(circle(centre, middle, diameter, chrome.dim(shade)));
        if let Some(icon) = draw(&Drawing::Close) {
            quads.push(centred(centre, middle, icon, chrome.dim(colors.foreground)));
        }
    }

    /// The tabs, the shown one marked, each with its title and, shown or hovered, its
    /// close button.
    fn tabs(
        &self,
        quads: &mut Vec<Quad>,
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) {
        let (chrome, scale, margin) = (self.chrome, self.scale, self.margin);
        let colors = chrome.colors();
        let top = margin + chrome.header() as f32 * scale;
        let middle = top + TAB_BAR as f32 * scale / 2.0;
        for (index, (left, share)) in chrome.shares(self.width / scale).enumerate() {
            let (left, share) = (margin + left * scale, share * scale);
            let inset = TAB_INSET * scale;
            let hovered =
                chrome.over(Part::Tab(index), false) || chrome.over(Part::CloseTab(index), false);
            let shade = if index == chrome.selected {
                Some(colors.button[0])
            } else if hovered {
                Some(colors.flat[0])
            } else {
                None
            };
            if let Some(shade) = shade {
                quads.push(rounded(
                    (left + inset).round(),
                    (top + inset).round(),
                    (share - 2.0 * inset).round(),
                    (TAB_BAR as f32 * scale - 2.0 * inset).round(),
                    TAB_RADIUS * scale,
                    chrome.dim(shade),
                ));
            }
            let room = share - 2.0 * (inset + TAB_CLOSE_END * scale + BUTTON * scale / 2.0);
            if let Some(text) = chrome.tabs.get(index).filter(|_| room > 0.0) {
                let tab = Drawing::Tab {
                    text: text.clone(),
                    width: room as u32,
                };
                if let Some(label) = draw(&tab) {
                    quads.push(self.label(left + share / 2.0, middle, label, colors.foreground));
                }
            }
            if index == chrome.selected || hovered {
                let centre = left + share - TAB_CLOSE_END * scale;
                if chrome.over(Part::CloseTab(index), false) {
                    let shade = colors.flat[usize::from(chrome.over(Part::CloseTab(index), true))];
                    quads.push(circle(centre, middle, BUTTON * scale, chrome.dim(shade)));
                }
                if let Some(icon) = draw(&Drawing::Close) {
                    quads.push(centred(centre, middle, icon, chrome.dim(colors.foreground)));
                }
            }
        }
    }

    /// A line of text centred on `(x, middle)`, placed by its line so that it does not
    /// shift with the letters in it.
    fn label(&self, x: f32, middle: f32, (index, glyph): (u32, GpuGlyph), color: u32) -> Quad {
        let (width, _) = halves(glyph.size);
        let top = i32::from((glyph.offset >> 16) as u16 as i16);
        let left = (x - width as f32 / 2.0).floor();
        let y = (middle - self.line as f32 / 2.0).floor() + top as f32;
        entry(left, y, index, glyph, self.chrome.dim(color))
    }
}

/// A rectangle with all its corners rounded.
fn rounded(x: f32, y: f32, width: f32, height: f32, radius: f32, color: u32) -> Quad {
    Quad {
        x,
        y,
        width,
        height,
        color,
        radius,
        corners: ALL_CORNERS,
        ..Quad::default()
    }
}

fn circle(x: f32, y: f32, diameter: f32, color: u32) -> Quad {
    let (left, top) = ((x - diameter / 2.0).round(), (y - diameter / 2.0).round());
    rounded(left, top, diameter, diameter, diameter / 2.0, color)
}

/// An icon centred on `(x, y)`.
fn centred(x: f32, y: f32, (index, glyph): (u32, GpuGlyph), color: u32) -> Quad {
    let (width, height) = halves(glyph.size);
    let left = (x - width as f32 / 2.0).round();
    let top = (y - height as f32 / 2.0).round();
    entry(left, top, index, glyph, color)
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

/// The close buttons' cross at `scale`.
pub fn close_icon(scale: f32) -> Raster {
    let (from, to) = CROSS;
    icon(scale, &[[from, from, to, to], [to, from, from, to]])
}

/// The new tab button's plus sign at `scale`.
pub fn new_tab_icon(scale: f32) -> Raster {
    let (from, to) = PLUS;
    let middle = ICON / 2.0;
    icon(
        scale,
        &[[from, middle, to, middle], [middle, from, middle, to]],
    )
}

/// An icon of `lines`, each from one point to another in a square [`ICON`] wide.
fn icon(scale: f32, lines: &[[f32; 4]]) -> Raster {
    let size = (ICON * scale).round() as u32;
    let mut path = Vec::new();
    for &[x0, y0, x1, y1] in lines {
        path.move_to([x0 * scale, y0 * scale])
            .line_to([x1 * scale, y1 * scale]);
    }
    let mut stroke = Stroke::new(STROKE * scale);
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
            header_bar: true,
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
        assert!(chrome.quads((800, 500), 1.0, 0, 0, |_| None).is_empty());
    }

    #[test]
    fn with_two_tabs_a_strip_shows_them_and_their_close_buttons() {
        let mut chrome = Chrome {
            floating: false,
            tabs: vec!["one".into()],
            ..floating()
        };
        let size = (800, 500);
        assert_eq!(chrome.top(), HEADER);
        chrome.tabs.push("two".into());
        assert_eq!(chrome.top(), HEADER + TAB_BAR);
        let middle = f64::from(HEADER + TAB_BAR / 2);
        assert_eq!(chrome.part_at((100.0, middle), size), Some(Part::Tab(0)));
        assert_eq!(chrome.part_at((500.0, middle), size), Some(Part::Tab(1)));
        assert_eq!(
            chrome.part_at((800.0 - 3.0 - 19.0, middle), size),
            Some(Part::CloseTab(1))
        );
        assert_eq!(chrome.part_at((20.0, 23.0), size), Some(Part::NewTab));
        assert_eq!(chrome.part_at((100.0, middle + 30.0), size), None);
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
