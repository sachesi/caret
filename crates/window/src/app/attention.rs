//! What programs do to be noticed: ringing the bell, and finishing a command or asking
//! for a notification while their tab is out of sight, which marks the tab and, for a
//! long command or a notification, notifies the desktop.

use std::time::Duration;

use caret_protocol::TerminalId;
use gettextrs::gettext;
use smithay_client_toolkit::reexports::calloop;
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::xdg::system_bell::v1::client::xdg_system_bell_v1::{
    self, XdgSystemBellV1,
};
use smithay_client_toolkit::shell::WaylandSurface;

use super::App;
use crate::notifications::{Clicked, Notice, Notifier};

/// A command that ran this long notifies the desktop when it finishes out of sight.
const NOTIFY_AFTER: Duration = Duration::from_secs(10);

/// The system bell, if the compositor rings one for clients.
pub fn bind_bell(globals: &GlobalList, qh: &QueueHandle<App>) -> Option<XdgSystemBellV1> {
    globals.bind(qh, 1..=1, Objects).ok()
}

impl App {
    pub(super) fn bell(&mut self, terminal: TerminalId) {
        if self.tab_of(terminal).is_none() {
            return;
        }
        if self.config.audible_bell
            && let Some(bell) = &self.offered.system_bell
        {
            bell.ring(Some(self.window.wl_surface()));
        }
        self.draw_attention(terminal);
    }

    /// A notification a program asked for, titled with its tab's title if it has none.
    pub(super) fn program_notification(
        &mut self,
        terminal: TerminalId,
        title: String,
        body: String,
    ) {
        if !self.config.program_notifications || !self.draw_attention(terminal) {
            return;
        }
        let title = if title.is_empty() {
            self.tab_title(terminal)
        } else {
            title
        };
        self.notify(Notice {
            terminal,
            title,
            body,
        });
    }

    pub(super) fn command_finished(
        &mut self,
        terminal: TerminalId,
        seconds: u64,
        status: Option<i32>,
    ) {
        if !self.draw_attention(terminal) || Duration::from_secs(seconds) < NOTIFY_AFTER {
            return;
        }
        let title = match status {
            Some(0) | None => gettext("Command Finished"),
            Some(_) => gettext("Command Failed"),
        };
        self.notify(Notice {
            terminal,
            title,
            body: self.tab_title(terminal),
        });
    }

    fn notify(&mut self, notice: Notice) {
        if self.notifier.is_none() {
            let (sender, clicks) = calloop::channel::channel();
            let followed = self.events.insert_source(clicks, |event, (), app| {
                if let calloop::channel::Event::Msg(clicked) = event {
                    app.notification_clicked(clicked);
                }
            });
            if let Err(error) = followed {
                tracing::warn!("following notification clicks: {error}");
            }
            self.notifier = Some(Notifier::start(sender));
        }
        if let Some(notifier) = &self.notifier {
            notifier.notify(notice);
        }
    }

    /// Shows the tab a clicked notification was for and brings the window up.
    fn notification_clicked(&mut self, clicked: Clicked) {
        let Some(index) = self.tab_of(clicked.terminal) else {
            return;
        };
        self.show(index);
        if let (Some(activation), Some(token)) = (&self.offered.activation, clicked.token) {
            activation.activate(token, self.window.wl_surface());
        }
    }
}

/// What the objects here carry, to be told their events.
pub struct Objects;

impl Dispatch2<XdgSystemBellV1, App> for Objects {
    fn event(
        &self,
        _: &mut App,
        _: &XdgSystemBellV1,
        _: xdg_system_bell_v1::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}
