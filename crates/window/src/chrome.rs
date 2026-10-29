//! The frame a window draws itself where the compositor leaves that to it: a header bar
//! with the title and a close button, rounded corners, a shadow, and the edges outside
//! the window that resize it.

use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;
use smithay_client_toolkit::seat::keyboard::Keysym;
use swash::zeno::{Cap, Mask, PathBuilder, Stroke};

use crate::atlas::GpuGlyph;
use crate::fonts::Raster;

mod dialog;
mod menu;

pub use dialog::{Appearance, Dialog, wrap};
pub use menu::{Item, Menu};

/// The header bar's height, in logical pixels as all sizes here are.
pub const HEADER: u32 = 47;
/// The strip of tabs under the header, shown while there are two or more.
pub const TAB_BAR: u32 = 38;
/// The bar to find text in, under them while it is open.
pub const SEARCH_BAR: u32 = 46;
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
/// From the window's right edge to the main menu button's centre.
const MAIN_MENU_END: f32 = 63.0;
/// The new tab button's square, from the window's left edge, and its corners' radius.
const FLAT_BUTTON: (f32, f32) = (6.0, 34.0);
const FLAT_RADIUS: f32 = 9.0;
/// Between the strip's edges and its tabs, and around each tab in its share.
const TAB_INSET: f32 = 3.0;
const TAB_RADIUS: f32 = 9.0;
/// From a tab's right edge to its close button's centre.
const TAB_CLOSE_END: f32 = 19.0;
/// Between the find bar's edges, its field and its buttons; the field's padding.
const SEARCH_INSET: f32 = 6.0;
const FIELD_PADDING: f32 = 9.0;
/// The icons' square, and the strokes drawn in it.
const ICON: f32 = 16.0;
const STROKE: f32 = 1.5;
/// A toast's height, the room either side of its text and below it, and its colour.
const TOAST: f32 = 42.0;
const TOAST_PADDING: f32 = 18.0;
const TOAST_MARGIN: f32 = 12.0;
const TOAST_BACKGROUND: u32 = 0x2828_2cf0;
/// The bubble showing a link's address: how far it is from the window's corner, and the
/// room around its text.
const BUBBLE_INSET: f32 = 6.0;
const BUBBLE_PADDING: (f32, f32) = (8.0, 4.0);

/// Colours as 0xRRGGBBAA, not premultiplied.
const OUTLINE: u32 = 0x0000_063b;
const ACCENT: u32 = 0x3584_e4ff;
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
    /// Text that was not found.
    error: u32,
    /// What dims the window under a dialog, the dialog, and its button that warns.
    scrim: u32,
    dialog: u32,
    destructive: u32,
    /// The ring around what the keys act on.
    focus: u32,
}

const DARK: Colors = Colors {
    header: 0x2e2e_32ff,
    backdrop: 0x2222_26ff,
    shade: 0x0000_065c,
    foreground: 0xffff_ffff,
    button: [0xffff_ff1a, 0xffff_ff26, 0xffff_ff4d],
    flat: [0xffff_ff12, 0xffff_ff29],
    error: 0xff7b_63ff,
    scrim: 0x0000_0666,
    dialog: 0x3636_3aff,
    destructive: 0xc01c_28ff,
    focus: 0x3584_e480,
};

const LIGHT: Colors = Colors {
    header: 0xffff_ffff,
    backdrop: 0xfafa_fbff,
    shade: 0x0000_061f,
    foreground: 0x0000_06cc,
    button: [0x0000_0614, 0x0000_061f, 0x0000_063d],
    flat: [0x0000_060e, 0x0000_0621],
    error: 0xc01c_28ff,
    scrim: 0x0000_0640,
    dialog: 0xffff_ffff,
    destructive: 0xe01b_24ff,
    focus: 0x3584_e480,
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
    /// A rectangle left out, x, y, width and height, its corners those of
    /// [`Self::corners`] rounded by [`Self::cut_radius`]; none while its width is zero.
    pub cut: [f32; 4],
    pub cut_radius: f32,
}

/// What the frame keeps in the atlas.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Drawing {
    /// A line of text, bold or not, cut to fit this many device pixels.
    Label {
        text: String,
        width: u32,
        bold: bool,
    },
    /// A line of larger bold text, cut to fit.
    Heading {
        text: String,
        width: u32,
    },
    Icon(Icon),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    Close,
    NewTab,
    Up,
    Down,
    Menu,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabLabel {
    pub title: String,
    /// Something happened in the tab while it was out of sight.
    pub attention: bool,
}

/// Text an input method is composing, over the cursor's cell.
#[derive(Debug, Default)]
pub struct Preedit {
    pub text: String,
    /// The cell's top left and height, in device pixels from the surface's corner.
    pub at: (f32, f32),
    pub height: f32,
    /// The terminal's text colour.
    pub color: u32,
}

/// A line of text the user types, and where typing goes in it.
#[derive(Debug, Default)]
pub struct Field {
    pub text: String,
    /// Where typing goes in the text, in bytes.
    pub cursor: usize,
    /// How far the cursor is from the start of the text, in device pixels.
    pub caret: f32,
}

/// What a key did to a field.
#[derive(Debug, PartialEq, Eq)]
pub enum Edit {
    Changed,
    Moved,
    /// The key is not one a field takes.
    Passed,
}

impl Field {
    /// A field holding `text`, typing at its end.
    pub fn holding(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            cursor: text.len(),
            caret: 0.0,
        }
    }

    /// Edits the text as `key` does, `typed` being the text it types if any.
    pub fn key(&mut self, key: Keysym, typed: Option<&str>) -> Edit {
        let (text, cursor) = (&mut self.text, &mut self.cursor);
        let before = text[..*cursor]
            .chars()
            .next_back()
            .map_or(0, char::len_utf8);
        let after = text[*cursor..].chars().next().map_or(0, char::len_utf8);
        match key {
            Keysym::BackSpace if before > 0 => {
                *cursor -= before;
                text.remove(*cursor);
                Edit::Changed
            }
            Keysym::Delete | Keysym::KP_Delete if after > 0 => {
                text.remove(*cursor);
                Edit::Changed
            }
            Keysym::BackSpace | Keysym::Delete | Keysym::KP_Delete => Edit::Moved,
            Keysym::Left | Keysym::KP_Left => {
                *cursor -= before;
                Edit::Moved
            }
            Keysym::Right | Keysym::KP_Right => {
                *cursor += after;
                Edit::Moved
            }
            Keysym::Home | Keysym::KP_Home => {
                *cursor = 0;
                Edit::Moved
            }
            Keysym::End | Keysym::KP_End => {
                *cursor = text.len();
                Edit::Moved
            }
            _ => match typed {
                Some(typed) => {
                    self.type_text(typed);
                    Edit::Changed
                }
                None => Edit::Passed,
            },
        }
    }

    pub fn type_text(&mut self, typed: &str) {
        self.text.insert_str(self.cursor, typed);
        self.cursor += typed.len();
    }
}

/// The bar to find text in the history with.
#[derive(Debug, Default)]
pub struct Search {
    pub field: Field,
    /// Keys go to the field, not the terminal.
    pub focused: bool,
    /// The text was found, or there is none to find.
    pub found: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Part {
    Edge(ResizeEdge),
    Header,
    Close,
    NewTab,
    /// The button that opens the main menu.
    MainMenu,
    /// The tab at this index, and its close button.
    Tab(usize),
    CloseTab(usize),
    SearchField,
    FindUp,
    FindDown,
    /// A dialog's button, and the rest of the window under the dialog.
    Response(usize),
    Dialog,
    /// A menu's item, the menu between its items, and the window outside it.
    MenuItem(usize),
    Menu,
    OffMenu,
}

impl Part {
    /// The part looks different while the pointer is over it.
    fn shows_hover(self) -> bool {
        !matches!(
            self,
            Self::Edge(_)
                | Self::Header
                | Self::SearchField
                | Self::Dialog
                | Self::Menu
                | Self::OffMenu
        )
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
    /// The tabs as the strip shows them, and which one is shown.
    pub tabs: Vec<TabLabel>,
    pub selected: usize,
    /// The find bar, while it is open.
    pub search: Option<Search>,
    /// Text being composed, shown over the terminal.
    pub preedit: Option<Preedit>,
    /// The address of the link under the pointer, shown over the terminal.
    pub link: Option<String>,
    /// A dialog over the window, which takes all input while it is open.
    pub dialog: Option<Dialog>,
    /// A menu over the window, which takes all input while it is open.
    pub menu: Option<Menu>,
    /// A short message over the bottom of the window.
    pub toast: Option<String>,
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

    pub fn search_bar(&self) -> u32 {
        if self.search.is_some() { SEARCH_BAR } else { 0 }
    }

    /// The height of the bars above the terminal.
    pub fn top(&self) -> u32 {
        self.header() + self.tab_bar() + self.search_bar()
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
        if let Some(menu) = &self.menu {
            return Some(menu.part_at((x, y), size));
        }
        if let Some(dialog) = &self.dialog {
            return Some(dialog.part_at((x, y), size));
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
                } else if within((width - f64::from(MAIN_MENU_END), middle), side / 2.0) {
                    Part::MainMenu
                } else {
                    Part::Header
                },
            );
        }
        let tabs = header + f64::from(self.tab_bar());
        if y >= tabs {
            if y >= tabs + f64::from(self.search_bar()) {
                return None;
            }
            let side = f64::from(FLAT_BUTTON.1);
            let buttons = width - f64::from(SEARCH_INSET) - 2.0 * side;
            return Some(if x < buttons - f64::from(SEARCH_INSET) {
                Part::SearchField
            } else if x < buttons + side {
                Part::FindUp
            } else {
                Part::FindDown
            });
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

    /// The tab whose share of the strip of a window `width` wide `x` is in, the last
    /// past its end.
    pub fn tab_at(&self, x: f64, width: u32) -> Option<usize> {
        let x = x as f32;
        self.shares(width as f32)
            .position(|(left, share)| x < left + share)
            .or_else(|| self.tabs.len().checked_sub(1))
    }

    /// Each tab's share of the strip of a window `width` wide: its left edge and width.
    fn shares(&self, width: f32) -> impl Iterator<Item = (f32, f32)> {
        let count = self.tabs.len().max(1);
        let share = (width - 2.0 * TAB_INSET) / count as f32;
        (0..self.tabs.len()).map(move |index| (TAB_INSET + index as f32 * share, share))
    }

    /// The frame of a window `size` big at `scale`, in the order it is drawn, and how
    /// many of its quads go under the terminal's cells; the rest go over them. `draw`
    /// gives the atlas entry of a drawing and the glyph it indexes, and `line` is the
    /// height of a line of text, in device pixels.
    pub fn quads(
        &self,
        size: (u32, u32),
        scale: f32,
        background: u32,
        line: i32,
        mut draw: impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) -> (Vec<Quad>, usize) {
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
        if let Some(search) = &self.search {
            painter.search(&mut quads, search, &mut draw);
        }
        let under = quads.len();
        if let Some(preedit) = &self.preedit {
            painter.preedit(&mut quads, preedit, background, &mut draw);
        }
        if let Some(link) = &self.link {
            painter.link(&mut quads, link, size.1 as f32 * scale, &mut draw);
        }
        if let Some(toast) = &self.toast {
            painter.toast(&mut quads, toast, size.1 as f32 * scale, &mut draw);
        }
        if let Some(dialog) = &self.dialog {
            painter.dialog(&mut quads, dialog, size.1 as f32 * scale, &mut draw);
        }
        if let Some(menu) = &self.menu {
            painter.menu(&mut quads, menu, size.1 as f32 * scale, &mut draw);
        }
        (quads, under)
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
        // What is drawn around the window stays out of it, where it would show through
        // a translucent background.
        let around = |quad: Quad| Quad {
            cut: [margin, margin, width, height],
            cut_radius: radius,
            ..quad
        };
        if self.floating {
            let spread = 2.0 * scale;
            quads.push(around(Quad {
                blur: 10.0 * scale,
                color: if self.active { SHADOW } else { SHADOW_BACKDROP },
                ..rectangle(
                    margin - spread,
                    margin - spread + 2.0 * scale,
                    width + 2.0 * spread,
                    height + 2.0 * spread,
                    0,
                )
            }));
            quads.push(around(Quad {
                radius: radius + scale,
                ..rectangle(
                    margin - scale,
                    margin - scale,
                    width + 2.0 * scale,
                    height + 2.0 * scale,
                    OUTLINE,
                )
            }));
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
        if let Some(icon) = draw(&Drawing::Icon(Icon::NewTab)) {
            quads.push(centred(
                left + side / 2.0,
                middle,
                icon,
                chrome.dim(colors.foreground),
            ));
        }
        let menu_left = (margin + width - (MAIN_MENU_END + FLAT_BUTTON.1 / 2.0) * scale).round();
        let open = chrome.menu.as_ref().is_some_and(|menu| menu.main);
        if open || chrome.over(Part::MainMenu, false) {
            let pressed = open || chrome.over(Part::MainMenu, true);
            quads.push(rounded(
                menu_left,
                top,
                side,
                side,
                FLAT_RADIUS * scale,
                chrome.dim(colors.flat[usize::from(pressed)]),
            ));
        }
        if let Some(icon) = draw(&Drawing::Icon(Icon::Menu)) {
            quads.push(centred(
                menu_left + side / 2.0,
                middle,
                icon,
                chrome.dim(colors.foreground),
            ));
        }
        let room = width - 2.0 * (MAIN_MENU_END + FLAT_BUTTON.1 / 2.0 + 6.0) * scale;
        if !chrome.title.is_empty() && room > 0.0 {
            let title = Drawing::Label {
                text: chrome.title.clone(),
                width: room as u32,
                bold: true,
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
        if let Some(icon) = draw(&Drawing::Icon(Icon::Close)) {
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
            let label = chrome.tabs.get(index);
            if let Some(label) = label.filter(|_| room > 0.0) {
                let tab = Drawing::Label {
                    text: label.title.clone(),
                    width: room as u32,
                    bold: false,
                };
                if let Some(label) = draw(&tab) {
                    quads.push(self.label(left + share / 2.0, middle, label, colors.foreground));
                }
            }
            if label.is_some_and(|label| label.attention) {
                let (width, height) = ((share / 3.0).min(64.0 * scale), 3.0 * scale);
                quads.push(Quad {
                    blur: 2.0 * scale,
                    ..rounded(
                        (left + (share - width) / 2.0).round(),
                        (top + TAB_BAR as f32 * scale - inset - height).round(),
                        width,
                        height,
                        height / 2.0,
                        chrome.dim(ACCENT),
                    )
                });
            }
            if index == chrome.selected || hovered {
                let centre = left + share - TAB_CLOSE_END * scale;
                if chrome.over(Part::CloseTab(index), false) {
                    let shade = colors.flat[usize::from(chrome.over(Part::CloseTab(index), true))];
                    quads.push(circle(centre, middle, BUTTON * scale, chrome.dim(shade)));
                }
                if let Some(icon) = draw(&Drawing::Icon(Icon::Close)) {
                    quads.push(centred(centre, middle, icon, chrome.dim(colors.foreground)));
                }
            }
        }
    }

    /// The field to type what to find in, and the buttons that find it upwards and
    /// downwards.
    fn search(
        &self,
        quads: &mut Vec<Quad>,
        search: &Search,
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) {
        let (chrome, scale, margin, width) = (self.chrome, self.scale, self.margin, self.width);
        let colors = chrome.colors();
        let top = margin + (chrome.header() + chrome.tab_bar()) as f32 * scale;
        let middle = top + SEARCH_BAR as f32 * scale / 2.0;
        let (inset, side) = (SEARCH_INSET * scale, FLAT_BUTTON.1 * scale);
        let field_top = (middle - side / 2.0).round();
        let buttons = (margin + width - inset - 2.0 * side).round();
        let field = (margin + inset).round();
        let color = if search.found {
            colors.foreground
        } else {
            colors.error
        };
        self.field(
            quads,
            &search.field,
            [field, field_top, buttons - inset - field, side],
            ("Find in the history", color),
            search.focused,
            draw,
        );
        let radius = FLAT_RADIUS * scale;
        for (index, (part, icon, corners)) in [
            (Part::FindUp, Icon::Up, TOP_LEFT | BOTTOM_LEFT),
            (Part::FindDown, Icon::Down, TOP_RIGHT | BOTTOM_RIGHT),
        ]
        .into_iter()
        .enumerate()
        {
            let left = buttons + index as f32 * side;
            let shade = if chrome.over(part, true) {
                colors.button[2]
            } else if chrome.over(part, false) {
                colors.button[1]
            } else {
                colors.button[0]
            };
            quads.push(Quad {
                corners,
                ..rounded(left, field_top, side, side, radius, chrome.dim(shade))
            });
            if let Some(icon) = draw(&Drawing::Icon(icon)) {
                let shown = chrome.dim(colors.foreground);
                quads.push(centred(left + side / 2.0, middle, icon, shown));
            }
        }
    }

    /// Text being composed, on the terminal's background and underlined, so that it
    /// hides the cells it covers.
    fn preedit(
        &self,
        quads: &mut Vec<Quad>,
        preedit: &Preedit,
        background: u32,
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) {
        let label = Drawing::Label {
            text: preedit.text.clone(),
            width: (self.margin + self.width - preedit.at.0).max(0.0) as u32,
            bold: false,
        };
        let Some((index, glyph)) = draw(&label) else {
            return;
        };
        let (width, _) = halves(glyph.size);
        let ((x, y), height) = (preedit.at, preedit.height);
        let width = width as f32;
        quads.push(Quad {
            x,
            y,
            width,
            height,
            color: background,
            ..Quad::default()
        });
        let (_, top) = offset(glyph);
        let text_y = (y + (height - self.line as f32) / 2.0).floor() + top as f32;
        quads.push(entry(x, text_y, index, glyph, preedit.color));
        let thickness = self.scale.round().max(1.0);
        quads.push(Quad {
            x,
            y: y + height - thickness,
            width,
            height: thickness,
            color: preedit.color,
            ..Quad::default()
        });
    }

    /// `text` in a toast at the bottom of a window `height` high.
    fn toast(
        &self,
        quads: &mut Vec<Quad>,
        text: &str,
        height: f32,
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) {
        let (scale, margin) = (self.scale, self.margin);
        let padding = TOAST_PADDING * scale;
        let room = (self.width - 2.0 * (TOAST_MARGIN * scale + padding)).max(0.0);
        let label = Drawing::Label {
            text: text.to_owned(),
            width: room as u32,
            bold: false,
        };
        let Some(glyph) = draw(&label) else {
            return;
        };
        let (width, _) = halves(glyph.1.size);
        let (toast_width, toast_height) = (width as f32 + 2.0 * padding, TOAST * scale);
        let x = (margin + (self.width - toast_width) / 2.0).round();
        let y = (margin + height - TOAST_MARGIN * scale - toast_height).round();
        quads.push(Quad {
            blur: 6.0 * scale,
            ..rounded(
                x,
                y + scale,
                toast_width,
                toast_height,
                toast_height / 2.0,
                SHADOW,
            )
        });
        quads.push(rounded(
            x,
            y,
            toast_width,
            toast_height,
            toast_height / 2.0,
            TOAST_BACKGROUND,
        ));
        quads.push(self.label(
            x + toast_width / 2.0,
            y + toast_height / 2.0,
            glyph,
            0xffff_ffff,
        ));
    }

    /// A link's address in a bubble at the bottom left of a window `height` high.
    fn link(
        &self,
        quads: &mut Vec<Quad>,
        link: &str,
        height: f32,
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) {
        let (chrome, scale, margin) = (self.chrome, self.scale, self.margin);
        let colors = chrome.colors();
        let (inset, padding) = (BUBBLE_INSET * scale, BUBBLE_PADDING.0 * scale);
        let room = (self.width / 2.0 - 2.0 * padding).max(0.0);
        let label = Drawing::Label {
            text: link.to_owned(),
            width: room as u32,
            bold: false,
        };
        let Some((index, glyph)) = draw(&label) else {
            return;
        };
        let (width, _) = halves(glyph.size);
        let bubble = (
            (margin + inset).round(),
            self.line as f32 + 2.0 * BUBBLE_PADDING.1 * scale,
        );
        let top = (margin + height - inset - bubble.1).round();
        quads.push(rounded(
            bubble.0 - scale,
            top - scale,
            width as f32 + 2.0 * padding + 2.0 * scale,
            bubble.1 + 2.0 * scale,
            FLAT_RADIUS * scale + scale,
            OUTLINE,
        ));
        quads.push(rounded(
            bubble.0,
            top,
            width as f32 + 2.0 * padding,
            bubble.1,
            FLAT_RADIUS * scale,
            colors.header,
        ));
        let (left, y) = offset(glyph);
        quads.push(entry(
            bubble.0 + padding + left as f32,
            top + BUBBLE_PADDING.1 * scale + y as f32,
            index,
            glyph,
            colors.foreground,
        ));
    }

    /// `field` in `[x, y, width, height]`, showing `placeholder` while it is empty and
    /// its text in `color`, with a caret while it has the keys.
    fn field(
        &self,
        quads: &mut Vec<Quad>,
        field: &Field,
        [x, y, width, height]: [f32; 4],
        (placeholder, color): (&str, u32),
        focused: bool,
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) {
        let (chrome, scale) = (self.chrome, self.scale);
        let colors = chrome.colors();
        quads.push(rounded(
            x,
            y,
            width,
            height,
            FLAT_RADIUS * scale,
            colors.button[0],
        ));
        let padding = FIELD_PADDING * scale;
        let room = (width - 2.0 * padding).max(0.0);
        let (text, color) = if field.text.is_empty() {
            (placeholder, (colors.foreground & !0xff) | 0x80)
        } else {
            (field.text.as_str(), color)
        };
        let label = Drawing::Label {
            text: text.to_owned(),
            width: room as u32,
            bold: false,
        };
        let (start, middle) = (x + padding, y + height / 2.0);
        if let Some((index, glyph)) = draw(&label) {
            let (left, top) = offset(glyph);
            let y = (middle - self.line as f32 / 2.0).floor() + top as f32;
            quads.push(entry(
                start + left as f32,
                y,
                index,
                glyph,
                chrome.dim(color),
            ));
        }
        if focused && chrome.active {
            let line = self.line as f32;
            quads.push(Quad {
                x: (start + field.caret.min(room)).round(),
                y: (middle - line / 2.0).floor(),
                width: scale.round().max(1.0),
                height: line,
                color: colors.foreground,
                ..Quad::default()
            });
        }
    }

    /// A line of text centred on `(x, middle)`, placed by its line so that it does not
    /// shift with the letters in it.
    fn label(&self, x: f32, middle: f32, (index, glyph): (u32, GpuGlyph), color: u32) -> Quad {
        let (width, _) = halves(glyph.size);
        let (_, top) = offset(glyph);
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

/// Where a glyph's pixels go from its pen and line, left and down.
fn offset(glyph: GpuGlyph) -> (i32, i32) {
    let half = |bits: u32| i32::from(bits as u16 as i16);
    (half(glyph.offset), half(glyph.offset >> 16))
}

/// `icon`'s pixels at `scale`.
pub fn icon(icon: Icon, scale: f32) -> Raster {
    let lines: &[&[[f32; 2]]] = match icon {
        Icon::Close => &[&[[4.0, 4.0], [12.0, 12.0]], &[[12.0, 4.0], [4.0, 12.0]]],
        Icon::NewTab => &[&[[3.0, 8.0], [13.0, 8.0]], &[[8.0, 3.0], [8.0, 13.0]]],
        Icon::Up => &[&[[3.0, 10.5], [8.0, 5.5], [13.0, 10.5]]],
        Icon::Down => &[&[[3.0, 5.5], [8.0, 10.5], [13.0, 5.5]]],
        Icon::Menu => &[
            &[[2.5, 4.0], [13.5, 4.0]],
            &[[2.5, 8.0], [13.5, 8.0]],
            &[[2.5, 12.0], [13.5, 12.0]],
        ],
    };
    let size = (ICON * scale).round() as u32;
    let mut path = Vec::new();
    for line in lines {
        let mut points = line.iter().map(|&[x, y]| [x * scale, y * scale]);
        if let Some(first) = points.next() {
            path.move_to(first);
        }
        for point in points {
            path.line_to(point);
        }
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

    fn label(title: &str) -> TabLabel {
        TabLabel {
            title: title.to_owned(),
            attention: false,
        }
    }

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
        assert!(chrome.quads((800, 500), 1.0, 0, 0, |_| None).0.is_empty());
    }

    #[test]
    fn with_two_tabs_a_strip_shows_them_and_their_close_buttons() {
        let mut chrome = Chrome {
            floating: false,
            tabs: vec![label("one")],
            ..floating()
        };
        let size = (800, 500);
        assert_eq!(chrome.top(), HEADER);
        chrome.tabs.push(label("two"));
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
        let icon = icon(Icon::Close, 1.0);
        assert_eq!((icon.width, icon.height), (16, 16));
        let alpha = |x: u32, y: u32| icon.pixels[((y * 16 + x) * 4 + 3) as usize];
        assert!(alpha(8, 8) > 200 && alpha(5, 5) > 100 && alpha(10, 5) > 100);
        assert_eq!(alpha(8, 1), 0);
    }
}
