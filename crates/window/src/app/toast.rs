//! Toasts: short messages over the bottom of the window that go after a while.

use std::time::Duration;

use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};

use super::App;
use crate::chrome::ToastButton;

/// How long a toast shows.
const SHOWN: Duration = Duration::from_secs(5);
/// The room either side of a toast button's label, as the toast draws it.
const TOAST_BUTTON_ROOM: f32 = 12.0;

impl App {
    /// Shows `text` in a toast with a button that lists the sessions.
    pub(super) fn toast_sessions(&mut self, text: String, button: String) {
        let measure = |text: &str| self.fonts.label_advance(text, false) / self.factor() as f32;
        // The label is bold, and a little wider than this measures.
        let label_width = measure(&button) * 1.1;
        let room = label_width + 2.0 * TOAST_BUTTON_ROOM;
        let text_width = measure(&text) + room;
        self.toast(text);
        self.chrome.toast_button = Some(ToastButton {
            label: button,
            text_width,
            label_width,
        });
        self.dirty = true;
        self.draw();
    }

    /// Shows `text` in a toast, in place of the one showing.
    pub(super) fn toast(&mut self, text: String) {
        self.chrome.toast = Some(text);
        self.chrome.toast_button = None;
        self.toasts += 1;
        let toast = self.toasts;
        let timer = self
            .events
            .insert_source(Timer::from_duration(SHOWN), move |_, (), app| {
                if app.toasts == toast && app.chrome.toast.take().is_some() {
                    app.chrome.toast_button = None;
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
