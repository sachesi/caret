//! Toasts: short messages over the bottom of the window that go after a while.

use std::time::Duration;

use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};

use super::App;

/// How long a toast shows.
const SHOWN: Duration = Duration::from_secs(5);

impl App {
    /// Shows `text` in a toast, in place of the one showing.
    pub(super) fn toast(&mut self, text: String) {
        self.chrome.toast = Some(text);
        self.toasts += 1;
        let toast = self.toasts;
        let timer = self
            .events
            .insert_source(Timer::from_duration(SHOWN), move |_, (), app| {
                if app.toasts == toast && app.chrome.toast.take().is_some() {
                    app.dirty = true;
                    app.draw();
                }
                TimeoutAction::Drop
            });
        if let Err(error) = timer {
            tracing::warn!("timing a toast: {error}");
        }
        self.dirty = true;
        self.draw();
    }
}
