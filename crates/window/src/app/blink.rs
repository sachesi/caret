//! The cursor's blinking, which runs for a while after each key and then rests, so that
//! a window nobody types in does not keep waking up to draw it.

use std::time::{Duration, Instant};

use smithay_client_toolkit::reexports::calloop::RegistrationToken;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};

use super::App;

const HALF_PERIOD: Duration = Duration::from_millis(600);
const BLINKS_FOR: Duration = Duration::from_secs(10);

#[derive(Default)]
pub struct Blink {
    /// In the half of a blink that hides the cursor.
    pub hidden: bool,
    timer: Option<RegistrationToken>,
}

impl App {
    /// Shows the cursor, and blinks it from now on if the window has the keys.
    pub(super) fn restart_blink(&mut self) {
        if let Some(timer) = self.blink.timer.take() {
            self.events.remove(timer);
        }
        if std::mem::take(&mut self.blink.hidden) && self.grid.cursor_blinks {
            self.dirty = true;
            self.draw();
        }
        if !self.focused || self.desktop.steady_cursor {
            return;
        }
        let started = Instant::now();
        let timer =
            self.events
                .insert_source(Timer::from_duration(HALF_PERIOD), move |_, (), app| {
                    let done = started.elapsed() >= BLINKS_FOR;
                    app.blink.hidden = !done && !app.blink.hidden;
                    if app.grid.cursor_blinks {
                        app.dirty = true;
                        app.draw();
                    }
                    if done {
                        app.blink.timer = None;
                        TimeoutAction::Drop
                    } else {
                        TimeoutAction::ToDuration(HALF_PERIOD)
                    }
                });
        match timer {
            Ok(timer) => self.blink.timer = Some(timer),
            Err(error) => tracing::warn!("timing the cursor's blinking: {error}"),
        }
    }
}
