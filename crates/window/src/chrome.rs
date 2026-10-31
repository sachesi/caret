//! The frame a window draws itself where the compositor leaves that to it: a header bar
//! with the title and a close button, rounded corners, a shadow, and the edges outside
//! the window that resize it.

use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;
use smithay_client_toolkit::seat::keyboard::Keysym;
use swash::zeno::{Cap, Mask, PathBuilder, Stroke};

use caret_protocol::encode::Modifiers;
use gettextrs::gettext;

use crate::atlas::GpuGlyph;
use crate::fonts::{Fonts, Raster};

mod dialog;
mod menu;
mod shortcut_list;

pub use dialog::{Appearance, Dialog, wrap};
pub use menu::{Item, Menu, Preview};
pub use shortcut_list::ShortcutList;

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
/// From one window button's centre to the next.
const BUTTON_STEP: f32 = 37.0;
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
/// A session tab's dot of its colour, and how far its middle is from the tab's start;
/// the room between its title and its count of tabs.
const TAB_DOT: f32 = 8.0;
const TAB_DOT_START: f32 = 14.0;
const TAB_COUNT_GAP: f32 = 8.0;
/// The mark of a tab whose command has run a while.
const BUSY: f32 = 10.0;
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
pub const ACCENT: u32 = 0x3584_e4ff;
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
    Minimize,
    Maximize,
    Restore,
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
    /// A command has run in it for a while.
    pub busy: bool,
    /// It stands for a session, while the window shows several.
    pub session: Option<SessionLabel>,
}

/// What a session's tab shows beside its title.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLabel {
    /// How many tabs it has.
    pub tabs: usize,
    pub color: Option<u32>,
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
    /// The selection's other end, in bytes; the cursor is the end that moves.
    pub anchor: Option<usize>,
    /// How far the cursor is from the start of the text, in device pixels.
    pub caret: f32,
    /// How far the selection's other end is from the start of the text, in device pixels.
    pub anchor_x: f32,
}

/// What a key did to a field.
#[derive(Debug, PartialEq, Eq)]
pub enum Edit {
    Changed,
    Moved,
    /// The text to put on the clipboard.
    Copy(String),
    /// The text taken out, to put on the clipboard.
    Cut(String),
    /// The clipboard is to be typed.
    Paste,
    /// The key is not one a field takes.
    Passed,
}

impl Field {
    /// A field holding `text`, typing at its end.
    pub fn holding(text: &str) -> Self {
        Self {
            text: text.to_owned(),
            cursor: text.len(),
            ..Self::default()
        }
    }

    /// Edits the text as `key` does with `modifiers` held, `typed` being the text it
    /// types if any.
    pub fn key(&mut self, key: Keysym, typed: Option<&str>, modifiers: Modifiers) -> Edit {
        let Modifiers { ctrl, shift, .. } = modifiers;
        if ctrl {
            match key {
                Keysym::a | Keysym::A => {
                    self.cursor = self.text.len();
                    self.anchor = (!self.text.is_empty()).then_some(0);
                    return Edit::Moved;
                }
                Keysym::c | Keysym::C => {
                    return self
                        .selected()
                        .map_or(Edit::Passed, |text| Edit::Copy(text.to_owned()));
                }
                Keysym::x | Keysym::X => {
                    let Some(text) = self.selected().map(str::to_owned) else {
                        return Edit::Passed;
                    };
                    self.remove_selection();
                    return Edit::Cut(text);
                }
                Keysym::v | Keysym::V => return Edit::Paste,
                _ => {}
            }
        }
        let collapsing = self.selection().filter(|_| !shift && !ctrl);
        let target = match key {
            Keysym::Left | Keysym::KP_Left => Some(match collapsing {
                Some(selection) => selection.start,
                None if ctrl => self.word_before(),
                None => self.char_before(),
            }),
            Keysym::Right | Keysym::KP_Right => Some(match collapsing {
                Some(selection) => selection.end,
                None if ctrl => self.word_after(),
                None => self.char_after(),
            }),
            Keysym::Home | Keysym::KP_Home => Some(0),
            Keysym::End | Keysym::KP_End => Some(self.text.len()),
            _ => None,
        };
        if let Some(target) = target {
            let anchor = shift.then(|| self.anchor.unwrap_or(self.cursor));
            self.anchor = anchor.filter(|&anchor| anchor != target);
            self.cursor = target;
            return Edit::Moved;
        }
        let end = match key {
            Keysym::BackSpace if ctrl => self.word_before(),
            Keysym::BackSpace => self.char_before(),
            Keysym::Delete | Keysym::KP_Delete if ctrl => self.word_after(),
            Keysym::Delete | Keysym::KP_Delete => self.char_after(),
            _ => {
                return match typed {
                    Some(typed) => {
                        self.type_text(typed);
                        Edit::Changed
                    }
                    None => Edit::Passed,
                };
            }
        };
        if self.remove_selection() {
            return Edit::Changed;
        }
        if end == self.cursor {
            return Edit::Moved;
        }
        let start = end.min(self.cursor);
        self.text.replace_range(start..end.max(self.cursor), "");
        self.cursor = start;
        Edit::Changed
    }

    pub fn type_text(&mut self, typed: &str) {
        self.remove_selection();
        self.text.insert_str(self.cursor, typed);
        self.cursor += typed.len();
    }

    /// Finds where the cursor and the selection's other end are drawn.
    pub fn measure(&mut self, fonts: &Fonts) {
        self.caret = fonts.label_advance(&self.text[..self.cursor], false);
        let anchor = self.anchor.unwrap_or(self.cursor);
        self.anchor_x = fonts.label_advance(&self.text[..anchor], false);
    }

    /// The selected bytes of the text, if any are.
    pub fn selection(&self) -> Option<std::ops::Range<usize>> {
        let anchor = self.anchor?;
        Some(anchor.min(self.cursor)..anchor.max(self.cursor))
    }

    fn selected(&self) -> Option<&str> {
        self.selection().map(|selection| &self.text[selection])
    }

    /// Takes the selected text out, saying whether there was any.
    fn remove_selection(&mut self) -> bool {
        let Some(selection) = self.selection() else {
            return false;
        };
        self.cursor = selection.start;
        self.text.replace_range(selection, "");
        self.anchor = None;
        true
    }

    fn char_before(&self) -> usize {
        self.text[..self.cursor]
            .chars()
            .next_back()
            .map_or(self.cursor, |letter| self.cursor - letter.len_utf8())
    }

    fn char_after(&self) -> usize {
        self.text[self.cursor..]
            .chars()
            .next()
            .map_or(self.cursor, |letter| self.cursor + letter.len_utf8())
    }

    /// Where the word before the cursor starts.
    fn word_before(&self) -> usize {
        let before = self.text[..self.cursor].trim_end_matches(|c: char| !c.is_alphanumeric());
        before.trim_end_matches(char::is_alphanumeric).len()
    }

    /// Where the word after the cursor ends.
    fn word_after(&self) -> usize {
        let after = self.text[self.cursor..].trim_start_matches(|c: char| !c.is_alphanumeric());
        self.text.len() - after.trim_start_matches(char::is_alphanumeric).len()
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
    Minimize,
    Maximize,
    NewTab,
    /// The button that opens the main menu.
    MainMenu,
    /// The tab at this index, and its close button; a session's tab has a button
    /// that lists its tabs in place of the close button.
    Tab(usize),
    CloseTab(usize),
    TabMenu(usize),
    SearchField,
    FindUp,
    FindDown,
    /// A dialog's button, a colour it offers, and the rest of the window under it.
    Response(usize),
    Swatch(usize),
    Dialog,
    /// A menu's item, the button that closes what it stands for, the menu between its
    /// items, and the window outside it.
    MenuItem(usize),
    MenuClose(usize),
    Menu,
    OffMenu,
    /// The list of shortcuts, and the window around it.
    ShortcutList,
    OffShortcutList,
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
                | Self::ShortcutList
                | Self::OffShortcutList
        )
    }
}

/// The buttons of the window on each side of the header bar, each side's in the order
/// they are drawn from left to right.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WindowButtons {
    pub start: Vec<Part>,
    pub end: Vec<Part>,
}

impl Default for WindowButtons {
    fn default() -> Self {
        Self {
            start: Vec::new(),
            end: vec![Part::Close],
        }
    }
}

impl WindowButtons {
    /// The buttons a layout such as `"close,minimize:maximize"` asks for, in the desktop's
    /// form: the two sides apart by a colon, with names of things the window does not
    /// have left out.
    pub fn from_layout(layout: &str) -> Self {
        let side = |names: &str| {
            names
                .split(',')
                .filter_map(|name| match name.trim() {
                    "close" => Some(Part::Close),
                    "minimize" => Some(Part::Minimize),
                    "maximize" => Some(Part::Maximize),
                    _ => None,
                })
                .collect()
        };
        let (start, end) = layout.split_once(':').unwrap_or((layout, ""));
        Self {
            start: side(start),
            end: side(end),
        }
    }

    /// How far each button's centre is from the side it is on, with the side.
    fn placed(&self) -> impl Iterator<Item = (Part, bool, f32)> {
        let from_end = self.end.iter().rev().enumerate();
        let from_start = self.start.iter().enumerate();
        let at = |index: usize| BUTTON_END + index as f32 * BUTTON_STEP;
        from_start
            .map(move |(index, &part)| (part, false, at(index)))
            .chain(from_end.map(move |(index, &part)| (part, true, at(index))))
    }

    /// How much further in than with one button on the end the main menu goes.
    fn main_menu_shift(&self) -> f32 {
        (self.end.len() as f32 - 1.0) * BUTTON_STEP
    }

    /// How much further in than with no button at the start the + button goes.
    fn new_tab_shift(&self) -> f32 {
        self.start.len() as f32 * BUTTON_STEP
    }
}

#[derive(Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a separate state the compositor configures"
)]
pub struct Chrome {
    pub buttons: WindowButtons,
    /// The window draws its frame: the compositor draws none.
    pub drawn: bool,
    /// Neither maximized nor tiled, so the window has a shadow, rounded corners and
    /// edges to resize it by.
    pub floating: bool,
    pub fullscreen: bool,
    pub maximized: bool,
    pub active: bool,
    /// Being resized interactively.
    pub resizing: bool,
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
    /// The list of shortcuts over the window, which takes all input while it is open.
    pub shortcut_list: Option<ShortcutList>,
    /// A short message over the bottom of the window.
    pub toast: Option<String>,
    /// The keys of a mode the window is in, under the toast.
    pub hints: Option<String>,
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
        if let Some(list) = &self.shortcut_list {
            return Some(list.part_at((x, y), size));
        }
        let header = f64::from(self.header());
        if y < header {
            let within =
                |(cx, cy): (f64, f64), reach: f64| (x - cx).abs() < reach && (y - cy).abs() < reach;
            let middle = f64::from(HEADER) / 2.0;
            let button = self.buttons.placed().find(|&(_, end, from)| {
                let from = f64::from(from);
                let centre = if end { width - from } else { from };
                within((centre, middle), f64::from(BUTTON_HIT) / 2.0)
            });
            if let Some((part, ..)) = button {
                return Some(part);
            }
            let left = f64::from(FLAT_BUTTON.0 + self.buttons.new_tab_shift());
            let side = f64::from(FLAT_BUTTON.1);
            let menu = f64::from(MAIN_MENU_END + self.buttons.main_menu_shift());
            return Some(if within((left + side / 2.0, middle), side / 2.0) {
                Part::NewTab
            } else if within((width - menu, middle), side / 2.0) {
                Part::MainMenu
            } else {
                Part::Header
            });
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
            if self
                .tabs
                .get(index)
                .is_some_and(|tab| tab.session.is_some())
            {
                Part::TabMenu(index)
            } else {
                Part::CloseTab(index)
            }
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

    /// The tab of the strip of a window `width` wide at `(x, y)`, in logical pixels from
    /// the window's corner, whatever is over the strip.
    pub fn strip_at(&self, (x, y): (f32, f32), width: u32) -> Option<usize> {
        let header = self.header() as f32;
        if !(header..header + self.tab_bar() as f32).contains(&y) {
            return None;
        }
        self.shares(width as f32)
            .position(|(left, share)| (left..left + share).contains(&x))
    }

    /// Where a menu opened from the tab at `index` of the strip of a window `width` wide
    /// goes: under the tab, at its left edge.
    pub fn under_tab(&self, index: usize, width: u32) -> Option<(f32, f32)> {
        let (left, _) = self.shares(width as f32).nth(index)?;
        Some((left, (self.header() + TAB_BAR) as f32))
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
        let mut bottom = size.1 as f32 * scale;
        if let Some(hints) = &self.hints {
            painter.toast(&mut quads, hints, bottom, &mut draw);
            bottom -= (TOAST + TOAST_MARGIN) * scale;
        }
        if let Some(toast) = &self.toast {
            painter.toast(&mut quads, toast, bottom, &mut draw);
        }
        if let Some(list) = &self.shortcut_list {
            painter.shortcut_list(&mut quads, list, size.1 as f32 * scale, &mut draw);
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
        let left = (margin + (FLAT_BUTTON.0 + chrome.buttons.new_tab_shift()) * scale).round();
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
        let menu_end = MAIN_MENU_END + chrome.buttons.main_menu_shift();
        let menu_left = (margin + width - (menu_end + FLAT_BUTTON.1 / 2.0) * scale).round();
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
        let widest = menu_end.max(FLAT_BUTTON.1 + chrome.buttons.new_tab_shift());
        let room = width - 2.0 * (widest + FLAT_BUTTON.1 / 2.0 + 6.0) * scale;
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
        let diameter = BUTTON * scale;
        for (part, end, from) in chrome.buttons.placed() {
            let centre = if end {
                margin + width - from * scale
            } else {
                margin + from * scale
            };
            let shade = if chrome.over(part, true) {
                colors.button[2]
            } else if chrome.over(part, false) {
                colors.button[1]
            } else {
                colors.button[0]
            };
            quads.push(circle(centre, middle, diameter, chrome.dim(shade)));
            let icon = match part {
                Part::Minimize => Icon::Minimize,
                Part::Maximize if chrome.maximized => Icon::Restore,
                Part::Maximize => Icon::Maximize,
                _ => Icon::Close,
            };
            if let Some(icon) = draw(&Drawing::Icon(icon)) {
                quads.push(centred(centre, middle, icon, chrome.dim(colors.foreground)));
            }
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
            let hovered = chrome.over(Part::Tab(index), false)
                || chrome.over(Part::CloseTab(index), false)
                || chrome.over(Part::TabMenu(index), false);
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
            let session = label.and_then(|label| label.session.as_ref());
            if let Some(label) = label {
                self.tab_title(quads, label, [left, share, middle, room], draw);
            }
            if let Some(color) = session.and_then(|session| session.color) {
                let dot = TAB_DOT * scale;
                quads.push(circle(
                    left + inset + TAB_DOT_START * scale,
                    middle,
                    dot,
                    chrome.dim(color),
                ));
            }
            if label.is_some_and(|label| label.busy) {
                // Past a session's dot, where there is one.
                let start = if session.is_some_and(|session| session.color.is_some()) {
                    TAB_DOT_START + TAB_DOT + TAB_COUNT_GAP
                } else {
                    TAB_DOT_START
                };
                let centre = (left + inset + start * scale, middle);
                quads.push(ring(centre, BUSY * scale, chrome.dim(colors.foreground)));
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
            // A session's button is always there: it is how its tabs are reached.
            if index == chrome.selected || hovered || session.is_some() {
                let centre = left + share - TAB_CLOSE_END * scale;
                let (button, icon) = if session.is_some() {
                    (Part::TabMenu(index), Icon::Down)
                } else {
                    (Part::CloseTab(index), Icon::Close)
                };
                if chrome.over(button, false) {
                    let shade = colors.flat[usize::from(chrome.over(button, true))];
                    quads.push(circle(centre, middle, BUTTON * scale, chrome.dim(shade)));
                }
                if let Some(icon) = draw(&Drawing::Icon(icon)) {
                    quads.push(centred(centre, middle, icon, chrome.dim(colors.foreground)));
                }
            }
        }
    }

    /// A tab's title, centred in its share, and a session's count of tabs after it.
    fn tab_title(
        &self,
        quads: &mut Vec<Quad>,
        label: &TabLabel,
        [left, share, middle, room]: [f32; 4],
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) {
        let (chrome, scale) = (self.chrome, self.scale);
        let colors = chrome.colors();
        let session = label.session.as_ref();
        // A session's count of tabs goes at the right of its title, dimmed.
        let count = session
            .map(|session| Drawing::Label {
                text: session.tabs.to_string(),
                width: room.max(0.0) as u32,
                bold: false,
            })
            .and_then(|count| draw(&count));
        let count_width = count.map_or(0.0, |(_, glyph)| {
            halves(glyph.size).0 as f32 + TAB_COUNT_GAP * scale
        });
        if room - count_width > 0.0 {
            let tab = Drawing::Label {
                text: label.title.clone(),
                width: (room - count_width) as u32,
                bold: false,
            };
            if let Some(title) = draw(&tab) {
                let centre = left + (share - count_width) / 2.0;
                let title_width = halves(title.1.size).0 as f32;
                quads.push(self.label(centre, middle, title, colors.foreground));
                if let Some(count) = count {
                    let x = centre + title_width / 2.0 + TAB_COUNT_GAP * scale;
                    let dim = (colors.foreground & !0xff) | 0x8c;
                    quads.push(self.label(
                        x + halves(count.1.size).0 as f32 / 2.0,
                        middle,
                        count,
                        chrome.dim(dim),
                    ));
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
            (&gettext("Find in the history"), color),
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
        if field.selection().is_some() {
            let [from, to] = [field.anchor_x, field.caret].map(|end| end.min(room));
            quads.push(Quad {
                x: (start + from.min(to)).round(),
                y: (middle - self.line as f32 / 2.0).floor(),
                width: (to - from).abs().round(),
                height: self.line as f32,
                color: chrome.dim((colors.focus & !0xff) | 0x60),
                ..Quad::default()
            });
        }
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

/// A ring as wide as `diameter`, its line a fifth of that.
fn ring((x, y): (f32, f32), diameter: f32, color: u32) -> Quad {
    let line = (diameter / 5.0).round().max(1.0);
    let quad = circle(x, y, diameter, (color & !0xff) | ((color & 0xff) * 3 / 5));
    let inner = quad.width - 2.0 * line;
    Quad {
        cut: [quad.x + line, quad.y + line, inner, inner],
        cut_radius: inner / 2.0,
        ..quad
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

/// `icon`'s pixels at `scale` as `theme` draws it, where it is a window button's and the
/// theme has it.
pub fn themed_icon(icon: Icon, scale: f32, theme: &str) -> Option<Raster> {
    let name = match icon {
        Icon::Close => "window-close-symbolic",
        Icon::Minimize => "window-minimize-symbolic",
        Icon::Maximize => "window-maximize-symbolic",
        Icon::Restore => "window-restore-symbolic",
        Icon::NewTab | Icon::Up | Icon::Down | Icon::Menu => return None,
    };
    crate::icon_theme::symbolic(theme, name, (ICON * scale).round() as u32)
}

/// `icon`'s pixels at `scale`.
pub fn icon(icon: Icon, scale: f32) -> Raster {
    let lines: &[&[[f32; 2]]] = match icon {
        Icon::Close => &[&[[4.0, 4.0], [12.0, 12.0]], &[[12.0, 4.0], [4.0, 12.0]]],
        Icon::Minimize => &[&[[4.0, 12.0], [12.0, 12.0]]],
        Icon::Maximize => &[&[
            [4.0, 4.0],
            [12.0, 4.0],
            [12.0, 12.0],
            [4.0, 12.0],
            [4.0, 4.0],
        ]],
        Icon::Restore => &[
            &[
                [3.5, 6.0],
                [10.0, 6.0],
                [10.0, 12.5],
                [3.5, 12.5],
                [3.5, 6.0],
            ],
            &[[6.0, 3.5], [12.5, 3.5], [12.5, 10.0]],
        ],
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

    const PLAIN: Modifiers = Modifiers {
        shift: false,
        alt: false,
        ctrl: false,
    };
    const CTRL: Modifiers = Modifiers {
        ctrl: true,
        ..PLAIN
    };
    const SHIFT: Modifiers = Modifiers {
        shift: true,
        ..PLAIN
    };

    #[test]
    fn ctrl_a_selects_the_text_and_typing_replaces_it() {
        let mut field = Field::holding("old name");
        assert_eq!(field.key(Keysym::a, None, CTRL), Edit::Moved);
        assert_eq!(field.selection(), Some(0..8));
        field.key(Keysym::n, Some("n"), PLAIN);
        assert_eq!((field.text.as_str(), field.cursor), ("n", 1));
        assert_eq!(field.selection(), None);
    }

    #[test]
    fn the_selection_is_copied_and_cut_for_the_clipboard() {
        let mut field = Field::holding("old name");
        assert_eq!(field.key(Keysym::c, None, CTRL), Edit::Passed);
        field.key(
            Keysym::Left,
            None,
            Modifiers {
                ctrl: true,
                shift: true,
                ..PLAIN
            },
        );
        assert_eq!(
            field.key(Keysym::c, None, CTRL),
            Edit::Copy("name".to_owned())
        );
        assert_eq!(
            field.key(Keysym::x, None, CTRL),
            Edit::Cut("name".to_owned())
        );
        assert_eq!(field.text, "old ");
        assert_eq!(field.key(Keysym::v, None, CTRL), Edit::Paste);
    }

    #[test]
    fn ctrl_moves_and_deletes_by_word() {
        let mut field = Field::holding("rust — new tab");
        field.key(Keysym::Left, None, CTRL);
        assert_eq!(&field.text[field.cursor..], "tab");
        field.key(Keysym::BackSpace, None, CTRL);
        assert_eq!(field.text, "rust — tab");
        field.key(Keysym::Home, None, PLAIN);
        field.key(Keysym::Right, None, CTRL);
        assert_eq!(field.cursor, 4);
        field.key(Keysym::Delete, None, CTRL);
        assert_eq!(field.text, "rust");
    }

    #[test]
    fn shift_extends_the_selection_and_a_plain_arrow_leaves_it_at_its_edge() {
        let mut field = Field::holding("héllo");
        field.cursor = 0;
        field.key(Keysym::Right, None, SHIFT);
        field.key(Keysym::Right, None, SHIFT);
        assert_eq!(field.selection(), Some(0..3));
        field.key(Keysym::Left, None, SHIFT);
        field.key(Keysym::Left, None, SHIFT);
        assert_eq!(field.selection(), None);
        field.key(Keysym::End, None, SHIFT);
        field.key(Keysym::Left, None, PLAIN);
        assert_eq!((field.cursor, field.selection()), (0, None));
    }

    #[test]
    fn backspace_takes_out_the_selection_alone() {
        let mut field = Field::holding("old name");
        field.key(Keysym::Left, None, SHIFT);
        assert_eq!(field.key(Keysym::BackSpace, None, PLAIN), Edit::Changed);
        assert_eq!(field.text, "old nam");
    }

    fn label(title: &str) -> TabLabel {
        TabLabel {
            title: title.to_owned(),
            attention: false,
            busy: false,
            session: None,
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
    fn a_layout_puts_the_buttons_it_names_on_its_sides() {
        let buttons = WindowButtons::from_layout("close,minimize:appmenu,maximize");
        assert_eq!(buttons.start, [Part::Close, Part::Minimize]);
        assert_eq!(buttons.end, [Part::Maximize]);
        assert_eq!(
            WindowButtons::from_layout(":close"),
            WindowButtons::default()
        );
        let chrome = Chrome {
            drawn: true,
            header_bar: true,
            buttons,
            ..Chrome::default()
        };
        let size = (800, 500);
        let m = f64::from(chrome.margin());
        let middle = m + f64::from(HEADER) / 2.0;
        let start = f64::from(BUTTON_END);
        let step = f64::from(BUTTON_STEP);
        assert_eq!(chrome.part_at((m + start, middle), size), Some(Part::Close));
        assert_eq!(
            chrome.part_at((m + start + step, middle), size),
            Some(Part::Minimize)
        );
        assert_eq!(
            chrome.part_at((m + 800.0 - start, middle), size),
            Some(Part::Maximize)
        );
        assert_eq!(
            chrome.part_at((m + start + 2.0 * step, middle), size),
            Some(Part::NewTab)
        );
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
    fn a_tab_whose_command_runs_a_while_has_a_ring() {
        let mut chrome = Chrome {
            floating: false,
            tabs: vec![label("one"), label("two")],
            ..floating()
        };
        let rings = |chrome: &Chrome| {
            let (quads, _) = chrome.quads((800, 500), 1.0, 0, 0, |_| None);
            quads.iter().filter(|quad| quad.cut[2] > 0.0).count()
        };
        assert_eq!(rings(&chrome), 0);
        chrome.tabs[1].busy = true;
        assert_eq!(rings(&chrome), 1);
    }

    #[test]
    fn a_session_s_tab_lists_its_tabs_and_is_found_under_a_menu() {
        let session = |title: &str| TabLabel {
            session: Some(SessionLabel {
                tabs: 2,
                color: None,
            }),
            ..label(title)
        };
        let mut chrome = Chrome {
            floating: false,
            tabs: vec![session("one"), session("two")],
            ..floating()
        };
        let size = (800, 500);
        let middle = HEADER as f32 + TAB_BAR as f32 / 2.0;
        assert_eq!(
            chrome.part_at((800.0 - 3.0 - 19.0, f64::from(middle)), size),
            Some(Part::TabMenu(1))
        );
        assert_eq!(
            chrome.under_tab(1, 800),
            Some((400.0, (HEADER + TAB_BAR) as f32))
        );
        chrome.menu = Some(Menu::new(Vec::new(), (0.0, 0.0), false));
        assert_eq!(chrome.strip_at((500.0, middle), 800), Some(1));
        assert_eq!(chrome.strip_at((500.0, middle + 30.0), 800), None);
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
