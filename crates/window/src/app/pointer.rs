//! The pointer: selecting, reporting the mouse to programs that ask, scrolling, and
//! pasting the primary selection.

use std::time::{Duration, Instant};

use caret_protocol::encode::{self, MouseAction, MouseButton};
use caret_protocol::{Bytes, Mode, Point, Question, Request, Scroll, Select, SelectionKind};
use smithay_client_toolkit::reexports::client::protocol::wl_pointer::WlPointer;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_toplevel::ResizeEdge;
use smithay_client_toolkit::reexports::protocols::wp::cursor_shape::v1::client::wp_cursor_shape_device_v1::{
    Shape, WpCursorShapeDeviceV1,
};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};

use super::{App, Asked};
use crate::chrome::Part;

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
    /// The serial of the pointer's entering the window, which setting its shape needs.
    entered: u32,
    /// The last click on the header bar, which a second soon after maximizes the window.
    header_click: Option<Instant>,
    /// Logical pixels from the surface's corner.
    position: (f64, f64),
    /// Selecting with the left button held.
    selecting: bool,
    /// The button a program was told is held, to tell it of motion and release.
    reported: Option<MouseButton>,
    last_click: Option<(Instant, Point)>,
    clicks: u32,
    /// Part of a line scrolled but not yet sent.
    scrolled: f64,
}

impl App {
    /// The cell under a point in logical pixels, and whether the point is in its right
    /// half.
    fn cell_at(&self, (x, y): (f64, f64)) -> Option<(Point, bool)> {
        let (columns, lines) = (self.grid.columns, self.grid.lines);
        if columns == 0 || lines == 0 {
            return None;
        }
        let scale = f64::from(self.scale);
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

    /// The part of the window's frame under the pointer; none over the terminal.
    fn part(&self) -> Option<Part> {
        self.chrome.part_at(self.pointer.position, self.size?)
    }

    /// Follows the pointer over the frame: the button it hovers and the cursor's shape.
    fn hover(&mut self) {
        let part = self.part();
        if part == self.chrome.hovered {
            return;
        }
        let shown = |part: Option<Part>| part.filter(|&part| part == Part::Close);
        let redraw = shown(part) != shown(self.chrome.hovered);
        self.chrome.hovered = part;
        if let Some(shape) = &self.pointer.shape {
            shape.set_shape(self.pointer.entered, cursor(part));
        }
        if redraw {
            self.dirty = true;
            self.draw();
        }
    }

    fn pressed_frame(&mut self, part: Part, button: u32, serial: u32) {
        let Some(seat) = self.seat.clone() else {
            return;
        };
        match (part, button) {
            (Part::Close, BUTTON_LEFT) => {
                self.chrome.pressed = Some(part);
                self.dirty = true;
                self.draw();
            }
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

    fn released_frame(&mut self) {
        if self.chrome.pressed.take() == Some(Part::Close) {
            if self.chrome.hovered == Some(Part::Close) {
                self.close();
            } else {
                self.dirty = true;
                self.draw();
            }
        }
    }

    /// Whether the program gets the mouse: it asked, and Shift is not held to select
    /// anyway.
    fn reporting(&self) -> bool {
        self.grid.mode.intersects(Mode::MOUSE_MODE) && !self.modifiers.shift
    }

    fn report(&mut self, button: MouseButton, action: MouseAction) {
        let (Some(terminal), Some((point, _))) =
            (self.terminal, self.cell_at(self.pointer.position))
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

    fn select(&mut self, select: Select) {
        if let Some(terminal) = self.terminal {
            self.server.send(&Request::Select { terminal, select });
        }
    }

    fn pressed(&mut self, button: u32, serial: u32) {
        self.serial = serial;
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
            _ => {}
        }
    }

    fn released(&mut self, button: u32) {
        if let Some(reported) = self.pointer.reported.take() {
            self.report(reported, MouseAction::Release);
            return;
        }
        if button == BUTTON_LEFT && std::mem::take(&mut self.pointer.selecting) {
            let Some(terminal) = self.terminal else {
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
        let Some(terminal) = self.terminal else {
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
                    if let Some(shape) = &self.pointer.shape {
                        shape.set_shape(serial, cursor(self.part()));
                    }
                    self.chrome.hovered = self.part();
                }
                PointerEventKind::Leave { .. } => {
                    if self.chrome.hovered.take() == Some(Part::Close) {
                        self.dirty = true;
                        self.draw();
                    }
                }
                PointerEventKind::Motion { .. } if dragging => self.moved(event.position),
                PointerEventKind::Motion { .. } => {
                    self.pointer.position = event.position;
                    self.hover();
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
                    if self.chrome.pressed.is_some() {
                        self.released_frame();
                    } else {
                        self.released(button);
                    }
                }
                PointerEventKind::Axis { .. } if self.chrome.hovered.is_some() => {}
                PointerEventKind::Axis { vertical, .. } => {
                    let lines = if vertical.value120 != 0 {
                        f64::from(vertical.value120) / 120.0 * NOTCH
                    } else if vertical.discrete != 0 {
                        f64::from(vertical.discrete) * NOTCH
                    } else {
                        let height = f64::from(self.fonts.cell().height) / f64::from(self.scale);
                        vertical.absolute / height
                    };
                    self.scrolled(lines);
                }
            }
        }
    }
}

/// The cursor over `part` of the window.
fn cursor(part: Option<Part>) -> Shape {
    match part {
        None => Shape::Text,
        Some(Part::Header | Part::Close) => Shape::Default,
        Some(Part::Edge(edge)) => match edge {
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
