//! The pointer: selecting, reporting the mouse to programs that ask, scrolling, and
//! pasting the primary selection.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use caret_protocol::encode::{self, MouseAction, MouseButton};
use caret_protocol::{
    Bytes, Mode, Point, Question, Request, Scroll, Select, SelectionKind, TerminalId,
};
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};

use smithay_client_toolkit::reexports::calloop::channel::Sender;

use super::{App, Asked};
use crate::chrome::{Chrome, Part};

/// Linux's button codes.
const BUTTON_LEFT: u32 = 0x110;
const BUTTON_RIGHT: u32 = 0x111;
const BUTTON_MIDDLE: u32 = 0x112;

/// Clicks this close together count as a double or triple click.
const MULTI_CLICK: Duration = Duration::from_millis(400);

/// Lines a notch of the wheel scrolls.
const NOTCH: f64 = 3.0;

#[derive(Default)]
pub struct Pointer {
    pub proxy: Option<WlPointer>,
    pub shape: Option<WpCursorShapeDeviceV1>,
    /// The serial of the pointer's entering the window, which setting its shape needs,
    /// and the shape set since.
    entered: u32,
    shown: Option<Shape>,
    /// The cell under the pointer, as the terminal was told.
    cell: Option<Point>,
    /// The last click on the header bar, which a second soon after maximizes the window.
    header_click: Option<Instant>,
    /// Logical pixels from the surface's corner.
    position: (f64, f64),
    /// Selecting with the left button held.
    selecting: bool,
    /// The tab held by the left button, which moves along the strip with the pointer.
    dragged_tab: Option<TerminalId>,
    /// The button a program was told is held, to tell it of motion and release.
    reported: Option<MouseButton>,
    last_click: Option<(Instant, Point)>,
    clicks: u32,
    /// Part of a line scrolled but not yet sent.
    scrolled: f64,
    /// Part of a notch of the wheel turned with Ctrl held, in 120ths, not yet zoomed by.
    zoomed: i32,
}

impl App {
    /// The cell under a point in logical pixels, and whether the point is in its right
    /// half.
    fn cell_at(&self, (x, y): (f64, f64)) -> Option<(Point, bool)> {
        let (columns, lines) = (self.grid.columns, self.grid.lines);
        if columns == 0 || lines == 0 {
            return None;
        }
        let scale = self.factor();
        let cell = self.fonts.cell();
        let (origin_x, origin_y) = self.origin();
        let x = (x * scale - f64::from(origin_x)) / f64::from(cell.width);
        let y = (y * scale - f64::from(origin_y)) / f64::from(cell.height);
        let column = (x.floor().max(0.0) as usize).min(columns - 1);
        let line = (y.floor().max(0.0) as usize).min(lines - 1);
        let point = Point {
            line: u16::try_from(line).ok()?,
            column: u16::try_from(column).ok()?,
        };
        Some((point, x - x.floor() >= 0.5))
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
    fn part(&self) -> Option<Part> {
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
        if redraw {
            self.dirty = true;
            self.draw();
        }
    }

    /// The cursor's shape where the pointer is.
    fn set_cursor(&mut self) {
        let shape = match self.chrome.hovered {
            Some(part) => cursor(part),
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
        let cell = if self.chrome.hovered.is_none() {
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

    fn pressed_frame(&mut self, part: Part, button: u32, serial: u32) {
        // A press outside the menu closes it and goes no further, unless it resizes.
        if self.chrome.menu.is_some() && !matches!(part, Part::MenuItem(_) | Part::Menu) {
            self.close_menu();
            if part == Part::OffMenu {
                return;
            }
        }
        match (part, button) {
            (Part::MainMenu, BUTTON_LEFT) => {
                self.open_main_menu();
                return;
            }
            (Part::Menu | Part::Dialog, _) => return,
            (
                Part::Close
                | Part::NewTab
                | Part::CloseTab(_)
                | Part::FindUp
                | Part::FindDown
                | Part::Response(_)
                | Part::MenuItem(_),
                BUTTON_LEFT,
            ) => {
                self.chrome.pressed = Some(part);
                self.dirty = true;
                self.draw();
                return;
            }
            (Part::Tab(index), BUTTON_LEFT) => {
                self.show(index);
                self.pointer.dragged_tab = self.terminal();
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
                    if self.chrome.maximized {
                        self.window.unset_maximized();
                    } else {
                        self.window.set_maximized();
                    }
                } else {
                    self.pointer.header_click = Some(now);
                    self.window.move_(&seat, serial);
                }
            }
            (Part::Header | Part::Close, BUTTON_RIGHT) => {
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

    /// Clicks the button the pointer was pressed on, if it is still over it.
    fn released_frame(&mut self) {
        let Some(pressed) = self.chrome.pressed.take() else {
            return;
        };
        self.dirty = true;
        if self.chrome.hovered == Some(pressed) {
            match pressed {
                Part::Close => self.close(),
                Part::NewTab => self.new_tab(),
                Part::CloseTab(index) => self.close_tab(index),
                Part::FindUp => self.find(true),
                Part::FindDown => self.find(false),
                Part::Response(index) => self.respond(index == 1),
                Part::MenuItem(index) => self.choose(index),
                _ => {}
            }
        }
        self.draw();
    }

    /// Whether the program gets the mouse: it asked, and Shift is not held to select
    /// anyway.
    fn reporting(&self) -> bool {
        self.grid.mode.intersects(Mode::MOUSE_MODE) && !self.modifiers.shift
    }

    fn report(&mut self, button: MouseButton, action: MouseAction) {
        let (Some(terminal), Some((point, _))) =
            (self.terminal(), self.cell_at(self.pointer.position))
        else {
            return;
        };
        let bytes = encode::mouse(
            button,
            action,
            self.modifiers,
            usize::from(point.column),
            usize::from(point.line),
            self.grid.mode,
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

    fn pressed(&mut self, button: u32, serial: u32) {
        self.serial = serial;
        // A click in the terminal gives it the keys back from the find bar.
        if let Some(search) = self.chrome.search.as_mut().filter(|search| search.focused) {
            search.focused = false;
            self.dirty = true;
            self.draw();
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

    /// Scrolls by `lines`, up for negative ones, as the program or the view takes it.
    fn scrolled(&mut self, lines: f64) {
        let total = self.pointer.scrolled + lines;
        let whole = total.trunc();
        self.pointer.scrolled = total - whole;
        let whole = whole as i32;
        let Some(terminal) = self.terminal() else {
            return;
        };
        if whole == 0 {
            return;
        }
        let mode = self.grid.mode;
        if self.reporting() {
            let button = if whole < 0 {
                MouseButton::WheelUp
            } else {
                MouseButton::WheelDown
            };
            for _ in 0..whole.unsigned_abs() {
                self.report(button, MouseAction::Press);
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
            // The terminal keeps a drag that began in it, wherever the pointer goes.
            let dragging = self.pointer.selecting || self.pointer.reported.is_some();
            match event.kind {
                PointerEventKind::Enter { serial } => {
                    self.pointer.position = event.position;
                    self.pointer.entered = serial;
                    self.pointer.shown = None;
                    self.chrome.hovered = self.part();
                    self.hover_cell();
                }
                PointerEventKind::Leave { .. } => {
                    if Chrome::hover_shows(self.chrome.hovered.take(), None) {
                        self.dirty = true;
                    }
                    if let Some(terminal) = self.terminal() {
                        self.unhover(terminal);
                    }
                    self.show_link();
                    self.draw();
                }
                PointerEventKind::Motion { .. } if dragging => self.moved(event.position),
                PointerEventKind::Motion { .. } if self.pointer.dragged_tab.is_some() => {
                    self.pointer.position = event.position;
                    if let Some(terminal) = self.pointer.dragged_tab {
                        let (x, _) = self.pointer_at();
                        self.drag_tab(terminal, f64::from(x));
                    }
                    self.hover();
                }
                PointerEventKind::Motion { .. } => {
                    self.pointer.position = event.position;
                    self.hover();
                    self.hover_cell();
                    if self.chrome.hovered.is_none() {
                        self.moved(event.position);
                    }
                }
                PointerEventKind::Press { button, serial, .. } => {
                    self.pointer.position = event.position;
                    match self.part() {
                        Some(part) if !dragging => {
                            self.serial = serial;
                            self.pressed_frame(part, button, serial);
                        }
                        _ => self.pressed(button, serial),
                    }
                }
                PointerEventKind::Release { button, .. } => {
                    self.pointer.dragged_tab = None;
                    if self.chrome.pressed.is_some() {
                        self.released_frame();
                    } else {
                        self.released(button);
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
                    let lines = if vertical.value120 != 0 {
                        f64::from(vertical.value120) / 120.0 * NOTCH
                    } else if vertical.discrete != 0 {
                        f64::from(vertical.discrete) * NOTCH
                    } else {
                        let height = f64::from(self.fonts.cell().height) / self.factor();
                        vertical.absolute / height
                    };
                    self.scrolled(lines);
                }
            }
        }
    }
}

/// Opens `uri` with the program the desktop has for it, telling `toasts` if it cannot.
pub(super) fn open(uri: &str, toasts: &Sender<String>) {
    let failed = |reason: &str| format!("Could not open the link: {reason}");
    let mut opener = match Command::new("xdg-open")
        .arg(uri)
        .stdin(Stdio::null())
        .spawn()
    {
        Ok(opener) => opener,
        Err(error) => {
            tracing::warn!("opening {uri}: {error}");
            let _ = toasts.send(failed("no program opens links"));
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
                    Some(2) => "it does not exist",
                    Some(3) => "no program opens it",
                    _ => "the program that opens it failed",
                };
                let _ = toasts.send(failed(reason));
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
        | Part::NewTab
        | Part::Tab(_)
        | Part::CloseTab(_)
        | Part::FindUp
        | Part::FindDown
        | Part::Response(_)
        | Part::Dialog
        | Part::MainMenu
        | Part::MenuItem(_)
        | Part::Menu
        | Part::OffMenu => Shape::Default,
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
