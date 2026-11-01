//! A finger on the window: it works the frame as the pointer does, and on the terminal a
//! tap clicks, a drag scrolls and a long press opens the terminal's menu.

use std::time::Duration;

use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::protocol::wl_touch::WlTouch;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::seat::touch::TouchHandler;

use super::App;
use super::pointer::BUTTON_LEFT;

/// Logical pixels a finger may stray and still be taken for holding still.
const SLOP: f64 = 8.0;
const LONG_PRESS: Duration = Duration::from_millis(500);

#[derive(Default)]
pub struct Touch {
    pub proxy: Option<WlTouch>,
    finger: Option<Finger>,
    /// The menu's surface the finger went down on.
    on_popup: Option<WlSurface>,
    /// Counts the long presses begun, so that a timer knows its finger still holds.
    presses: u32,
}

struct Finger {
    id: i32,
    /// Where it came down, and where it was last.
    start: (f64, f64),
    last: (f64, f64),
    /// It came down on the frame, a divider or a pane in control mode, and goes on as
    /// the pointer's left button.
    direct: bool,
    scrolling: bool,
    held: bool,
}

/// Whether a finger has gone from `start` to `at`, far enough not to be a tap.
fn strayed(start: (f64, f64), at: (f64, f64)) -> bool {
    (at.0 - start.0).hypot(at.1 - start.1) > SLOP
}

impl App {
    fn start_long_press(&mut self) {
        self.touch.presses = self.touch.presses.wrapping_add(1);
        let press = self.touch.presses;
        let timer =
            self.events
                .insert_source(Timer::from_duration(LONG_PRESS), move |_, (), app| {
                    if app.touch.presses == press
                        && let Some(finger) = app.touch.finger.as_mut()
                    {
                        finger.held = true;
                        if let Some(index) = app.pressed_listed_row() {
                            app.chrome.pressed = None;
                            app.listed_menu(index, false);
                        } else {
                            app.open_terminal_menu();
                        }
                    }
                    TimeoutAction::Drop
                });
        if let Err(error) = timer {
            tracing::warn!("timing a long press: {error}");
        }
    }

    fn lift(&mut self) {
        self.touch.presses = self.touch.presses.wrapping_add(1);
        self.leave();
    }
}

impl TouchHandler for App {
    fn down(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlTouch,
        serial: u32,
        _: u32,
        surface: WlSurface,
        id: i32,
        position: (f64, f64),
    ) {
        let position = self.in_window(&surface, position);
        self.touch.on_popup = self.is_popup(&surface).then_some(surface);
        if self.touch.finger.is_some() {
            return;
        }
        self.pointer.position = position;
        self.hover();
        let direct =
            self.part().is_some() || self.control || !self.dividers_under_pointer().is_empty();
        self.touch.finger = Some(Finger {
            id,
            start: position,
            last: position,
            direct,
            scrolling: false,
            held: false,
        });
        self.serial = serial;
        if direct {
            self.pointer_pressed(position, BUTTON_LEFT, serial);
        }
        if !direct || self.pressed_listed_row().is_some() {
            self.start_long_press();
        }
    }

    fn up(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlTouch,
        serial: u32,
        _: u32,
        id: i32,
    ) {
        let Some(finger) = self.touch.finger.take_if(|finger| finger.id == id) else {
            return;
        };
        if finger.direct {
            self.let_go(BUTTON_LEFT);
        } else if !finger.scrolling && !finger.held {
            self.pointer_pressed(finger.start, BUTTON_LEFT, serial);
            self.let_go(BUTTON_LEFT);
        }
        self.lift();
    }

    fn motion(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlTouch,
        _: u32,
        id: i32,
        position: (f64, f64),
    ) {
        let position = match &self.touch.on_popup {
            Some(surface) => self.in_window(surface, position),
            None => position,
        };
        let height = f64::from(self.fonts.cell().height) / self.factor();
        let Some(finger) = self.touch.finger.as_mut().filter(|finger| finger.id == id) else {
            return;
        };
        if finger.direct {
            self.pointer_moved(position);
            return;
        }
        if !finger.scrolling && !finger.held && strayed(finger.start, position) {
            finger.scrolling = true;
            self.touch.presses = self.touch.presses.wrapping_add(1);
        }
        if finger.scrolling {
            let travelled = position.1 - finger.last.1;
            finger.last = position;
            self.scrolled(-travelled / height);
        }
    }

    fn shape(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlTouch,
        _: i32,
        _: f64,
        _: f64,
    ) {
    }

    fn orientation(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch, _: i32, _: f64) {}

    fn cancel(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlTouch) {
        if self.touch.finger.take().is_some_and(|finger| finger.direct) {
            self.chrome.pressed = None;
            self.let_go(BUTTON_LEFT);
        }
        self.lift();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_finger_that_stays_near_where_it_came_down_is_a_tap() {
        assert!(!strayed((100.0, 100.0), (105.0, 104.0)));
        assert!(strayed((100.0, 100.0), (100.0, 109.0)));
    }
}
