//! The pointer: selecting, reporting the mouse to programs that ask, scrolling, and
//! pasting the primary selection.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use caret_protocol::encode::{self, MouseAction, MouseButton};
use caret_protocol::{
    Bytes, Mode, Point, Question, Request, Scroll, Select, SelectionKind, SessionId,
    TerminalId,
};
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};

use smithay_client_toolkit::reexports::calloop::channel::Sender;

use gettextrs::gettext;

use super::gliding::Motion;
use super::scroll_bars::HeldBar;
use super::{App, Asked};
use crate::chrome::{Chrome, Part};
use crate::i18n::gettext_f;
use crate::panes::Divider;

/// Linux's button codes.
pub(super) const BUTTON_LEFT: u32 = 0x110;
pub(super) const BUTTON_RIGHT: u32 = 0x111;
pub(super) const BUTTON_MIDDLE: u32 = 0x112;

/// Clicks this close together count as a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(400);

/// How long the + button is held for its menu rather than a tab.
const LONG_PRESS: Duration = Duration::from_millis(500);

/// Lines a notch of the wheel scrolls.
const NOTCH: f64 = 3.0;

#[derive(Default)]
pub struct Pointer {
    pub proxy: Option<WlPointer>,
    pub shape: Option<WpCursorShapeDeviceV1>,
    /// It is over the window or its menu.
    here: bool,
    /// The serial of the pointer's entering the window, which setting its shape needs,
    /// and the shape set since.
    entered: u32,
    shown: Option<Shape>,
    /// The cell under the pointer, as the terminal was told.
    cell: Option<Point>,
    /// The last click on the header bar, which a second soon after maximizes the window.
    header_click: Option<Instant>,
    /// When the + button was pressed.
    new_tab_pressed: Option<Instant>,
    /// Logical pixels from the surface's corner.
    pub(super) position: (f64, f64),
    /// Selecting with the left button held.
    selecting: bool,
    /// The tab held by the left button, which moves along the strip with the pointer.
    dragged_tab: Option<TerminalId>,
    /// The session's tab, or the listed tab, held by the left button: where it was
    /// pressed, and whether it has been dragged since.
    pub(super) dragged_session: Option<(SessionId, (f64, f64), bool)>,
    pub(super) dragged_row: Option<(TerminalId, (f64, f64), bool)>,
    /// The dividers between panes held by the left button, which follow the pointer.
    dividing: Vec<Divider>,
    /// The pane whose scroll bar the pointer is over, which `scroll-bars` at `"hover"`
    /// shows.
    pub(super) bar_hovered: Option<TerminalId>,
    /// The scroll bar held by the left button, whose thumb follows the pointer.
    pub(super) held_bar: Option<HeldBar>,
    /// The pane held by the left button, to move where it is let go of.
    pub(super) moving: Option<TerminalId>,
    /// The button a program was told is held, to tell it of motion and release.
    reported: Option<MouseButton>,
    last_click: Option<(Instant, Point)>,
    clicks: u32,
    /// Part of a line scrolled but not yet sent.
    scrolled: f64,
    /// Part of a notch of the wheel turned with Ctrl held, in 120ths, not yet zoomed by.
    zoomed: i32,
    /// Part of a notch of the wheel turned over a session's tab, in 120ths, not yet stepped
    /// by.
    tab_wheel: i32,
    /// Part of a notch turned over the list of shortcuts, in 120ths, not yet scrolled by.
    pub turned_list: i32,
}

impl App {
    /// The cell under a point in logical pixels, and whether the point is in its right
    /// half.
    fn cell_at(&self, position: (f64, f64)) -> Option<(Point, bool)> {
        self.cell_in(self.terminal()?, position)
    }

    /// The same in the pane of `terminal`, which need not be the focused one.
    fn cell_in(&self, terminal: TerminalId, (x, y): (f64, f64)) -> Option<(Point, bool)> {
        let grid = self.grid_of(terminal)?;
        let (columns, lines) = (grid.columns, grid.lines);
        if columns == 0 || lines == 0 {
            return None;
        }
        let scale = self.factor();
        let cell = self.fonts.cell();
        let (origin_x, origin_y) = self
            .placed()
            .rect(terminal)
            .or_else(|| self.area())
            .map_or((0, 0), |rect| (rect.x, rect.y));
        let x = (x * scale - f64::from(origin_x)) / f64::from(cell.width);
        let y =
            (y * scale - f64::from(origin_y - self.glide_shift(terminal))) / f64::from(cell.height);
        let column = (x.floor().max(0.0) as usize).min(columns - 1);
        let line = (y.floor().max(0.0) as usize).min(lines - 1);
        let point = Point {
            line: u16::try_from(line).ok()?,
            column: u16::try_from(column).ok()?,
        };
        Some((point, x - x.floor() >= 0.5))
    }

    /// Where the pointer is, in device pixels from the surface's corner.
    pub(super) fn device_point(&self) -> (f64, f64) {
        let (x, y) = self.pointer.position;
        (x * self.factor(), y * self.factor())
    }

    /// How far from where dividers meet a press still takes hold of them all, in device
    /// pixels.
    fn junction_reach(&self) -> f64 {
        f64::from(self.gap()) / 2.0
    }

    /// Moves the dividers held to where the pointer is, resizing the panes around them.
    fn divide(&mut self) {
        let point = self.device_point();
        let gap = self.gap();
        let Some(tab) = self.tabs.get_mut(self.selected) else {
            return;
        };
        let mut moved = false;
        for divider in &self.pointer.dividing {
            moved |= tab
                .root
                .divide(divider.before, divider.share_under(point, gap));
        }
        if moved {
            self.resize_panes(self.selected);
            self.dirty = true;
            self.draw();
        }
    }

    /// Lets go of the dividers held, telling the server where they are now.
    fn let_go_of_dividers(&mut self) {
        for divider in std::mem::take(&mut self.pointer.dividing) {
            let share = self
                .tabs
                .get(self.selected)
                .and_then(|tab| tab.root.share(divider.before));
            if let Some(share) = share {
                self.server.send(&Request::Divide {
                    terminal: divider.before,
                    share,
                });
            }
        }
    }

    /// The dividers a press where the pointer is would take hold of.
    pub(super) fn dividers_under_pointer(&self) -> Vec<Divider> {
        self.placed()
            .dividers_at(self.device_point(), self.junction_reach())
    }

    /// Where the pointer is, in logical pixels from the window's corner.
    pub(super) fn pointer_at(&self) -> (f32, f32) {
        let margin = f64::from(self.chrome.margin());
        let (x, y) = self.pointer.position;
        ((x - margin) as f32, (y - margin) as f32)
    }

    pub(super) fn pointer_cell(&self) -> Option<Point> {
        self.cell_at(self.pointer.position).map(|(point, _)| point)
    }

    /// The part of the window's frame under the pointer; none over the terminal.
    pub(super) fn part(&self) -> Option<Part> {
        self.chrome.part_at(self.pointer.position, self.size?)
    }

    /// Follows the pointer over the frame: the button it hovers and the cursor's shape.
    pub(super) fn hover(&mut self) {
        let part = self.part();
        if part == self.chrome.hovered {
            return;
        }
        let redraw = Chrome::hover_shows(self.chrome.hovered, part);
        self.chrome.hovered = part;
        self.set_cursor();
        self.follow_submenu();
        if redraw {
            self.dirty = true;
            self.draw();
        }
    }

    /// The cursor's shape where the pointer is.
    pub(super) fn set_cursor(&mut self) {
        let dividers = if self.pointer.dividing.is_empty() {
            self.dividers_under_pointer()
        } else {
            self.pointer.dividing.clone()
        };
        let upright = dividers.iter().any(|divider| divider.vertical);
        let level = dividers.iter().any(|divider| !divider.vertical);
        let shape = match self.chrome.hovered {
            _ if self.pointer.moving.is_some() => Shape::Grabbing,
            _ if matches!(self.pointer.dragged_session, Some((_, _, true)))
                || matches!(self.pointer.dragged_row, Some((_, _, true))) =>
            {
                Shape::Grabbing
            }
            Some(part) => cursor(part),
            None if self.pointer.held_bar.is_some() || self.over_thumb() => Shape::Default,
            None if upright && level => Shape::AllScroll,
            None if upright => Shape::ColResize,
            None if level => Shape::RowResize,
            None if self.over_bar() => Shape::Default,
            None if self.modifiers.ctrl && self.chrome.link.is_some() => Shape::Pointer,
            None if self.grid.mode.intersects(Mode::MOUSE_MODE) => Shape::Default,
            None => Shape::Text,
        };
        if let Some(device) = &self.pointer.shape
            && self.pointer.shown != Some(shape)
        {
            device.set_shape(self.pointer.entered, shape);
            self.pointer.shown = Some(shape);
        }
    }

    /// Tells the terminal which cell the pointer is over, for it to mark the address
    /// there.
    fn hover_cell(&mut self) {
        let over_focused = self.placed().pane_at(self.device_point()) == self.terminal();
        let cell = if self.chrome.hovered.is_none() && over_focused {
            self.cell_at(self.pointer.position).map(|(point, _)| point)
        } else {
            None
        };
        if cell != self.pointer.cell
            && let Some(terminal) = self.terminal()
        {
            self.pointer.cell = cell;
            self.server.send(&Request::Hover {
                terminal,
                point: cell,
            });
        }
        self.show_link();
    }

    /// Tells `terminal` the pointer is over none of its cells.
    pub(super) fn unhover(&mut self, terminal: TerminalId) {
        if self.pointer.cell.take().is_some() {
            self.server.send(&Request::Hover {
                terminal,
                point: None,
            });
        }
    }

    /// Shows the address of the link under the pointer, as the last frame found it, and
    /// the cursor that opens it. With Ctrl held any address shows; without, a program's
    /// link still does, since its text may say otherwise.
    pub(super) fn show_link(&mut self) {
        let link = self
            .grid
            .hovered
            .as_ref()
            .filter(|link| self.pointer.cell.is_some() && (self.modifiers.ctrl || link.marked))
            .map(|link| link.uri.clone());
        if link != self.chrome.link {
            self.chrome.link = link;
            self.dirty = true;
            self.draw();
        }
        self.set_cursor();
    }

    /// Takes a press of `button` on the menu's item at `index` other than a click, saying
    /// whether it did anything.
    fn pressed_listed(&mut self, index: usize, button: u32) -> bool {
        match button {
            BUTTON_RIGHT => {
                self.listed_menu(index, false);
                true
            }
            BUTTON_MIDDLE if self.config.middle_click_detach => {
                self.detach_listed(index);
                true
            }
            _ => false,
        }
    }

    fn pressed_frame(&mut self, part: Part, button: u32, serial: u32) {
        // A press outside the menu closes it and goes no further, unless it resizes.
        if self.chrome.menu.is_some() && !in_menus(part) {
            self.close_menu();
            if part == Part::OffMenu {
                return;
            }
        }
        if matches!(part, Part::MenuItem(index) if self.pressed_listed(index, button)) {
            return;
        }
        if self.grouped() && self.pressed_session_tab(part, button) {
            return;
        }
        match (part, button) {
            (Part::MainMenu, BUTTON_LEFT) => {
                self.open_main_menu(None);
                return;
            }
            (Part::Menu | Part::Dialog | Part::ShortcutList, _) => return,
            (Part::OffShortcutList, _) => return self.close_shortcut_list(),
            (
                Part::Close
                | Part::Minimize
                | Part::Maximize
                | Part::NewTab
                | Part::CloseTab(_)
                | Part::FindUp
                | Part::FindDown
                | Part::Response(_)
                | Part::ToastButton
                | Part::Swatch(_)
                | Part::MenuItem(_)
                | Part::MenuClose(_)
                | Part::SubmenuItem(_),
                BUTTON_LEFT,
            ) => {
                if let Part::MenuItem(index) = part {
                    self.hold_listed(index);
                }
                if part == Part::NewTab {
                    self.pointer.new_tab_pressed = Some(Instant::now());
                }
                self.chrome.pressed = Some(part);
                self.dirty = true;
                self.draw();
                return;
            }
            (Part::Tab(index), BUTTON_LEFT) => {
                self.show(index);
                self.pointer.dragged_tab = self.tabs.get(index).map(|tab| tab.key);
                return;
            }
            (Part::NewTab, BUTTON_RIGHT) => {
                self.open_new_menu();
                return;
            }
            (Part::SearchField, BUTTON_LEFT) => {
                self.open_search();
                return;
            }
            (Part::Tab(index) | Part::CloseTab(index), BUTTON_MIDDLE) => {
                self.close_tab(index);
                return;
            }
            (Part::Tab(index) | Part::CloseTab(index), BUTTON_RIGHT) => {
                self.tab_menu(index);
                return;
            }
            _ => {}
        }
        let Some(seat) = self.seat.clone() else {
            return;
        };
        match (part, button) {
            (Part::Edge(edge), BUTTON_LEFT) => self.window.resize(&seat, serial, edge),
            (Part::Header, BUTTON_LEFT) => {
                let now = Instant::now();
                let again = self
                    .pointer
                    .header_click
                    .is_some_and(|at| now.duration_since(at) < MULTI_CLICK);
                if again {
                    self.pointer.header_click = None;
                    self.toggle_maximized();
                } else {
                    self.pointer.header_click = Some(now);
                    self.window.move_(&seat, serial);
                }
            }
            (Part::Header | Part::Close | Part::Minimize | Part::Maximize, BUTTON_RIGHT) => {
                let margin = f64::from(self.chrome.margin());
                let (x, y) = self.pointer.position;
                self.window.show_window_menu(
                    &seat,
                    serial,
                    ((x - margin) as i32, (y - margin) as i32),
                );
            }
            _ => {}
        }
    }

    fn toggle_maximized(&mut self) {
        if self.chrome.maximized {
            self.window.unset_maximized();
        } else {
            self.window.set_maximized();
        }
    }

    /// Clicks the button the pointer was pressed on, if it is still over it.
    fn released_frame(&mut self) {
        let Some(pressed) = self.chrome.pressed.take() else {
            return;
        };
        self.dirty = true;
        if self.chrome.hovered == Some(pressed) {
            match pressed {
                Part::Close => self.close(),
                Part::Minimize => self.window.set_minimized(),
                Part::Maximize => self.toggle_maximized(),
                Part::NewTab => {
                    let held = self.pointer.new_tab_pressed.take();
                    if held.is_some_and(|at| at.elapsed() >= LONG_PRESS) {
                        self.open_new_menu();
                    } else {
                        self.new_tab();
                    }
                }
                Part::CloseTab(index) => self.close_tab(index),
                Part::FindUp => self.find(true),
                Part::FindDown => self.find(false),
                Part::Response(index) => self.press_response(index),
                Part::ToastButton => {
                    self.chrome.toast = None;
                    self.chrome.toast_button = None;
                    self.act(crate::shortcuts::Action::Sessions);
                }
                Part::Swatch(index) => self.pick_swatch(index),
                Part::MenuItem(index) => self.choose(index),
                Part::MenuClose(index) => self.close_listed(index),
                Part::SubmenuItem(index) => self.choose_in_submenu(index),
                _ => {}
            }
        }
        self.draw();
    }

    /// Lets go of what the button held: a tab, a pane, a divider, a button of the frame,
    /// or the terminal's selection.
    pub(super) fn let_go(&mut self, button: u32) {
        if let Some(terminal) = self.pointer.dragged_tab.take()
            && self.outside()
        {
            self.tab_to_window(terminal);
        }
        if self.let_go_of_bar() {
            return;
        }
        if self.drop_listed() {
            self.chrome.pressed = None;
            self.set_cursor();
            self.dirty = true;
            self.draw();
        } else if self.pointer.moving.is_some() {
            self.drop_pane();
            self.set_cursor();
        } else if !self.pointer.dividing.is_empty() {
            self.let_go_of_dividers();
            self.set_cursor();
        } else if self.chrome.pressed.is_some() {
            self.released_frame();
        } else {
            self.released(button);
        }
    }

    /// Lets go of what the button held of a menu, without doing what a release would.
    fn let_go_of_menu(&mut self) {
        self.pointer.dragged_row = None;
        if matches!(
            self.chrome.pressed,
            Some(Part::MenuItem(_) | Part::MenuClose(_) | Part::SubmenuItem(_))
        ) {
            self.chrome.pressed = None;
            self.dirty = true;
            self.draw();
        }
    }

    /// Whether the program gets the mouse: it asked, and Shift is not held to select
    /// anyway.
    fn reporting(&self) -> bool {
        self.grid.mode.intersects(Mode::MOUSE_MODE) && !self.modifiers.shift
    }

    fn report(&mut self, button: MouseButton, action: MouseAction) {
        if let Some(terminal) = self.terminal() {
            self.report_to(terminal, button, action);
        }
    }

    fn report_to(&mut self, terminal: TerminalId, button: MouseButton, action: MouseAction) {
        let (Some(grid), Some((point, _))) = (
            self.grid_of(terminal),
            self.cell_in(terminal, self.pointer.position),
        ) else {
            return;
        };
        let bytes = encode::mouse(
            button,
            action,
            self.modifiers,
            usize::from(point.column),
            usize::from(point.line),
            grid.mode,
        );
        if let Some(bytes) = bytes {
            self.server.send(&Request::Input {
                terminal,
                bytes: Bytes(bytes),
                bottom: false,
            });
        }
    }

    pub(super) fn select(&mut self, select: Select) {
        if let Some(terminal) = self.terminal() {
            self.server.send(&Request::Select { terminal, select });
        }
    }

    /// Takes hold of the thumb, the divider or the bar under the pointer, in that order: the
    /// bar reaches into the gap, where only the thumb goes before the divider.
    fn took_hold_of_edge(&mut self) -> bool {
        let moves_pane = self.control || self.modifiers.alt;
        if !moves_pane && self.over_thumb() {
            self.hold_bar();
            return true;
        }
        let dividers = self.dividers_under_pointer();
        if !dividers.is_empty() {
            self.pointer.dividing = dividers;
            return true;
        }
        if !moves_pane && self.over_bar() {
            self.hold_bar();
            return true;
        }
        false
    }

    fn pressed(&mut self, button: u32, serial: u32) {
        self.serial = serial;
        // A click in the terminal gives it the keys back from the find bar.
        if let Some(search) = self.chrome.search.as_mut().filter(|search| search.focused) {
            search.focused = false;
            self.dirty = true;
            self.draw();
        }
        if button == BUTTON_LEFT && self.took_hold_of_edge() {
            return;
        }
        let placed = self.placed();
        let at = self.device_point();
        if button == BUTTON_LEFT
            && (self.control || self.modifiers.alt)
            && let Some(pane) = placed.pane_at(at)
        {
            self.focus_pane(pane);
            self.pointer.moving = Some(pane);
            self.set_cursor();
            return;
        }
        // A click in another pane gives it the keys, and goes on there.
        if let Some(pane) = placed
            .pane_at(at)
            .filter(|&pane| Some(pane) != self.terminal())
        {
            self.focus_pane(pane);
        }
        let button = match button {
            BUTTON_LEFT => MouseButton::Left,
            BUTTON_MIDDLE => MouseButton::Middle,
            BUTTON_RIGHT => MouseButton::Right,
            _ => return,
        };
        if self.reporting() {
            self.pointer.reported = Some(button);
            self.report(button, MouseAction::Press);
            return;
        }
        match button {
            MouseButton::Left if self.modifiers.ctrl => {
                if let (Some(terminal), Some((point, _))) =
                    (self.terminal(), self.cell_at(self.pointer.position))
                {
                    let id = self.server.ask(Question::LinkAt { terminal, point });
                    self.asked.insert(id, Asked::Link);
                }
            }
            MouseButton::Left => {
                let Some((point, right)) = self.cell_at(self.pointer.position) else {
                    return;
                };
                let now = Instant::now();
                let again = self.pointer.last_click.is_some_and(|(at, last)| {
                    now.duration_since(at) < MULTI_CLICK && last == point
                });
                self.pointer.clicks = if again {
                    self.pointer.clicks % 3 + 1
                } else {
                    1
                };
                self.pointer.last_click = Some((now, point));
                self.pointer.selecting = true;
                if self.modifiers.shift {
                    self.select(Select::Extend { point, right });
                    return;
                }
                let kind = match self.pointer.clicks {
                    2 => SelectionKind::Word,
                    3 => SelectionKind::Line,
                    _ => SelectionKind::Simple,
                };
                self.select(Select::Start { kind, point, right });
            }
            MouseButton::Middle => self.paste(true),
            MouseButton::Right => self.open_terminal_menu(),
            _ => {}
        }
    }

    fn released(&mut self, button: u32) {
        if let Some(reported) = self.pointer.reported.take() {
            self.report(reported, MouseAction::Release);
            return;
        }
        if button == BUTTON_LEFT && std::mem::take(&mut self.pointer.selecting) {
            let Some(terminal) = self.terminal() else {
                return;
            };
            let id = self.server.ask(Question::SelectionText { terminal });
            self.asked.insert(id, Asked::Copy { primary: true });
        }
    }

    fn moved(&mut self, position: (f64, f64)) {
        self.pointer.position = position;
        if let Some(button) = self.pointer.reported {
            if self
                .grid
                .mode
                .intersects(Mode::MOUSE_DRAG | Mode::MOUSE_MOTION)
            {
                self.report(button, MouseAction::Motion);
            }
            return;
        }
        if self.pointer.selecting {
            if let Some((point, right)) = self.cell_at(position) {
                self.select(Select::Extend { point, right });
            }
        } else if self.grid.mode.contains(Mode::MOUSE_MOTION) && self.reporting() {
            self.report(MouseButton::None, MouseAction::Motion);
        }
    }

    /// The pointer or the finger has left: nothing is under it any more.
    pub(super) fn leave(&mut self) {
        if Chrome::hover_shows(self.chrome.hovered.take(), None) {
            self.dirty = true;
        }
        if let Some(terminal) = self.terminal() {
            self.unhover(terminal);
        }
        self.hover_bar(None);
        self.show_link();
        self.draw();
    }

    pub(super) fn pointer_moved(&mut self, position: (f64, f64)) {
        self.pointer.position = position;
        if self.pointer.held_bar.is_some() {
            self.drag_bar();
        } else if self.pointer.dragged_session.is_some() || self.pointer.dragged_row.is_some() {
            self.drag_listed();
            self.hover();
        } else if self.pointer.moving.is_some() {
            self.dirty = true;
            self.draw();
        } else if !self.pointer.dividing.is_empty() {
            self.divide();
        } else if self.dragging() {
            self.moved(position);
        } else if let Some(terminal) = self.pointer.dragged_tab {
            let (x, _) = self.pointer_at();
            self.drag_tab(terminal, f64::from(x));
            self.hover();
        } else {
            self.hover();
            self.follow_bar_under_pointer();
            self.hover_cell();
            if self.chrome.hovered.is_none() {
                self.moved(position);
            }
        }
    }

    pub(super) fn pointer_pressed(&mut self, position: (f64, f64), button: u32, serial: u32) {
        self.pointer.position = position;
        match self.part() {
            Some(part) if !self.dragging() => {
                self.serial = serial;
                self.pressed_frame(part, button, serial);
            }
            _ => self.pressed(button, serial),
        }
    }

    /// The terminal keeps a drag that began in it, wherever the pointer goes.
    fn dragging(&self) -> bool {
        self.pointer.selecting
            || self.pointer.reported.is_some()
            || self.pointer.held_bar.is_some()
            || !self.pointer.dividing.is_empty()
    }

    /// The pane the wheel turns in. A drag keeps it in the pane the drag began in, and
    /// between panes there is only the focused one to mean.
    fn wheel_pane(&self) -> Option<TerminalId> {
        let focused = self.terminal();
        if !self.config.scroll_under_pointer || self.dragging() {
            return focused;
        }
        self.placed().pane_at(self.device_point()).or(focused)
    }

    /// Scrolls by `lines`, up for negative ones, as the program or the view takes it.
    pub(super) fn scrolled(&mut self, lines: f64, motion: Motion) {
        if self.glides()
            && let Some(terminal) = self.wheel_pane()
            && let Some(mode) = self.grid_of(terminal).map(|grid| grid.mode)
            && (self.modifiers.shift
                || !(mode.intersects(Mode::MOUSE_MODE)
                    || mode.contains(Mode::ALT_SCREEN | Mode::ALTERNATE_SCROLL)))
        {
            self.glide(terminal, lines, motion);
            return;
        }
        let total = self.pointer.scrolled + lines;
        let whole = total.trunc();
        self.pointer.scrolled = total - whole;
        let whole = whole as i32;
        let Some(terminal) = self.wheel_pane() else {
            return;
        };
        if whole == 0 {
            return;
        }
        let Some(mode) = self.grid_of(terminal).map(|grid| grid.mode) else {
            return;
        };
        if mode.intersects(Mode::MOUSE_MODE) && !self.modifiers.shift {
            let button = if whole < 0 {
                MouseButton::WheelUp
            } else {
                MouseButton::WheelDown
            };
            for _ in 0..whole.unsigned_abs() {
                self.report_to(terminal, button, MouseAction::Press);
            }
        } else if mode.contains(Mode::ALT_SCREEN | Mode::ALTERNATE_SCROLL) && !self.modifiers.shift
        {
            // Programs on the alternate screen have no history to scroll: they get arrows.
            let key = if whole < 0 {
                encode::Key::Up
            } else {
                encode::Key::Down
            };
            if let Some(bytes) = encode::key(key, encode::Modifiers::default(), mode) {
                self.server.send(&Request::Input {
                    terminal,
                    bytes: Bytes(bytes.repeat(whole.unsigned_abs() as usize)),
                    bottom: false,
                });
            }
        } else {
            self.server.send(&Request::Scroll {
                terminal,
                scroll: Scroll::Lines { lines: -whole },
            });
        }
    }
}

impl PointerHandler for App {
    fn pointer_frame(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let position = self.in_window(&event.surface, event.position);
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    self.pointer.here = true;
                    self.pointer.position = position;
                    self.pointer.entered = serial;
                    self.pointer.shown = None;
                    self.chrome.hovered = self.part();
                    self.follow_submenu();
                    self.follow_bar_under_pointer();
                    self.hover_cell();
                }
                PointerEventKind::Leave { .. } => {
                    self.leave();
                    self.pointer.here = false;
                    // Gone from the menu and not on to the window, where the compositor may not
                    // tell of the button's release. An enter comes with a leave between them.
                    self.events.insert_idle(|app| {
                        if !app.pointer.here {
                            app.let_go_of_menu();
                        }
                    });
                }
                PointerEventKind::Motion { .. } => self.pointer_moved(position),
                PointerEventKind::Press { button, serial, .. } => {
                    self.settle_glides();
                    self.pointer_pressed(position, button, serial);
                }
                PointerEventKind::Release { button, .. } => self.let_go(button),
                PointerEventKind::Axis { vertical, .. } if self.chrome.menu.is_some() => {
                    self.scroll_menu(vertical.value120, vertical.discrete, vertical.absolute);
                }
                PointerEventKind::Axis { vertical, .. } if self.chrome.shortcut_list.is_some() => {
                    self.scroll_shortcut_list(vertical.value120, vertical.discrete);
                }
                // Only a wheel steps: a touchpad's scrolling would step by the pixel.
                PointerEventKind::Axis { vertical, .. } if self.over_session_tab().is_some() => {
                    let turned = if vertical.value120 != 0 {
                        vertical.value120
                    } else {
                        vertical.discrete * 120
                    };
                    self.pointer.tab_wheel += turned;
                    let notches = self.pointer.tab_wheel / 120;
                    self.pointer.tab_wheel %= 120;
                    if notches != 0
                        && let Some(session) = self.over_session_tab()
                    {
                        self.step_tab_of(session, notches);
                    }
                }
                PointerEventKind::Axis { .. } if self.chrome.hovered.is_some() => {}
                // Only a wheel zooms: a touchpad's scrolling would zoom by the pixel.
                PointerEventKind::Axis { vertical, .. } if self.modifiers.ctrl => {
                    let turned = if vertical.value120 != 0 {
                        vertical.value120
                    } else {
                        vertical.discrete * 120
                    };
                    self.pointer.zoomed += turned;
                    let notches = self.pointer.zoomed / 120;
                    self.pointer.zoomed %= 120;
                    if notches != 0 {
                        self.zoom(Some(-notches));
                    }
                }
                PointerEventKind::Axis { vertical, .. } => {
                    let (lines, motion) = if vertical.value120 != 0 {
                        (f64::from(vertical.value120) / 120.0 * NOTCH, Motion::Wheel)
                    } else if vertical.discrete != 0 {
                        (f64::from(vertical.discrete) * NOTCH, Motion::Wheel)
                    } else {
                        let height = f64::from(self.fonts.cell().height) / self.factor();
                        (vertical.absolute / height, Motion::Touch)
                    };
                    self.scrolled(lines * self.config.scroll_speed, motion);
                }
            }
        }
    }
}

/// Opens `uri` with the program the desktop has for it, telling `toasts` if it cannot.
pub(super) fn open(uri: &str, toasts: &Sender<String>) {
    let failed =
        |reason: &str| gettext_f("Could not open the link: {reason}", &[("reason", reason)]);
    let mut command = Command::new("xdg-open");
    command.arg(uri).stdin(Stdio::null());
    let mut opener = match caret_protocol::start_with_usual_turns(&mut command).spawn() {
        Ok(opener) => opener,
        Err(error) => {
            tracing::warn!("opening {uri}: {error}");
            let _ = toasts.send(failed(&gettext("no program opens links")));
            return;
        }
    };
    let (uri, toasts) = (uri.to_owned(), toasts.clone());
    // Waited for elsewhere, so that the window goes on meanwhile and it leaves no zombie.
    let waited = std::thread::Builder::new()
        .name("open".into())
        .spawn(move || match opener.wait() {
            Ok(status) if !status.success() => {
                tracing::warn!("opening {uri}: {status}");
                // The codes xdg-open exits with.
                let reason = match status.code() {
                    Some(2) => gettext("it does not exist"),
                    Some(3) => gettext("no program opens it"),
                    _ => gettext("the program that opens it failed"),
                };
                let _ = toasts.send(failed(&reason));
            }
            Ok(_) => {}
            Err(error) => tracing::warn!("opening {uri}: {error}"),
        });
    if let Err(error) = waited {
        tracing::warn!("waiting for a link to open: {error}");
    }
}

/// The cursor over `part` of the window.
fn cursor(part: Part) -> Shape {
    match part {
        Part::SearchField => Shape::Text,
        Part::Header
        | Part::Close
        | Part::Minimize
        | Part::Maximize
        | Part::NewTab
        | Part::Tab(_)
        | Part::CloseTab(_)
        | Part::TabMenu(_)
        | Part::FindUp
        | Part::FindDown
        | Part::Response(_)
        | Part::ToastButton
        | Part::Swatch(_)
        | Part::Dialog
        | Part::MainMenu
        | Part::MenuItem(_)
        | Part::MenuClose(_)
        | Part::SubmenuItem(_)
        | Part::Menu
        | Part::OffMenu
        | Part::ShortcutList
        | Part::OffShortcutList => Shape::Default,
        Part::Edge(edge) => match edge {
            ResizeEdge::Top => Shape::NResize,
            ResizeEdge::Bottom => Shape::SResize,
            ResizeEdge::Left => Shape::WResize,
            ResizeEdge::Right => Shape::EResize,
            ResizeEdge::TopLeft => Shape::NwResize,
            ResizeEdge::TopRight => Shape::NeResize,
            ResizeEdge::BottomLeft => Shape::SwResize,
            _ => Shape::SeResize,
        },
    }
}

/// Whether `part` is of the open menu or its submenu.
fn in_menus(part: Part) -> bool {
    matches!(
        part,
        Part::MenuItem(_) | Part::MenuClose(_) | Part::Menu | Part::SubmenuItem(_)
    )
}
