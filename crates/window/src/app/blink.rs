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
    /// When the cursor last showed again, while it blinks.
    since: Option<Instant>,
}

impl App {
    /// Shows the cursor, and blinks it from now on if the window has the keys.
    pub(super) fn restart_blink(&mut self) {
        if std::mem::take(&mut self.blink.hidden) && self.grid.cursor_blinks {
            self.dirty = true;
            self.draw();
        }
        let blinks = self.focused && !self.desktop.steady_cursor;
        self.blink.since = blinks.then(Instant::now);
        // A timer going on sees the new start: one taken out could have fired already,
        // which the event loop warns of.
        if !blinks || self.blink.timer.is_some() {
            return;
        }
        let timer = self
            .events
            .insert_source(Timer::from_duration(HALF_PERIOD), |_, (), app| {
                let Some(since) = app.blink.since else {
                    app.blink.timer = None;
                    return TimeoutAction::Drop;
                };
                let shown = since.elapsed();
                // Started again since the last tick: the cursor shows a whole half first.
                if shown < HALF_PERIOD {
                    return TimeoutAction::ToDuration(HALF_PERIOD.saturating_sub(shown));
                }
                let done = shown >= BLINKS_FOR;
                app.blink.hidden = !done && !app.blink.hidden;
                if app.grid.cursor_blinks {
                    app.dirty = true;
                    app.draw();
                }
                if done {
                    app.blink.timer = None;
                    app.blink.since = None;
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
