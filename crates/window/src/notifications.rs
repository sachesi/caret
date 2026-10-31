//! Desktop notifications for tabs out of sight, sent to the session's notification
//! server on a thread of their own; clicking one tells the window to show its tab.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use caret_protocol::TerminalId;
use gettextrs::gettext;
use smithay_client_toolkit::reexports::calloop::channel::Sender;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::Value;

/// The time between notifications. A program can send them as fast as it can write, and
/// each goes to the desktop's notification server.
const PAUSE: Duration = Duration::from_millis(100);

pub struct Notice {
    pub terminal: TerminalId,
    pub title: String,
    pub body: String,
}

/// A click on a notification: the tab it was for, and the token that lets the window
/// take the focus, if the server gave one.
pub struct Clicked {
    pub terminal: TerminalId,
    pub token: Option<String>,
}

pub struct Notifier(mpsc::Sender<Notice>);

impl Notifier {
    /// Starts sending notifications, telling `clicked` of the clicks on them.
    pub fn start(clicked: Sender<Clicked>) -> Self {
        let (notices, received) = mpsc::channel();
        let spawned = std::thread::Builder::new()
            .name("notifications".into())
            .spawn(move || {
                if let Err(error) = send(&received, clicked) {
                    tracing::info!("sending notifications: {error}");
                }
            });
        if let Err(error) = spawned {
            tracing::warn!("starting to send notifications: {error}");
        }
        Self(notices)
    }

    pub fn notify(&self, notice: Notice) {
        let _ = self.0.send(notice);
    }
}

/// The notifications shown: which tab each is for, and the last for each tab, which
/// the next for it replaces.
#[derive(Default)]
struct Shown {
    terminals: HashMap<u32, TerminalId>,
    last: HashMap<TerminalId, u32>,
}

fn send(notices: &mpsc::Receiver<Notice>, clicked: Sender<Clicked>) -> zbus::Result<()> {
    let connection = crate::desktop::session()?;
    let server = Proxy::new(
        &connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )?;
    let shown = Arc::new(Mutex::new(Shown::default()));
    follow_clicks(&connection, Arc::clone(&shown), clicked)?;
    let hints = HashMap::from([
        ("desktop-entry", Value::from(crate::APP_ID)),
        ("urgency", Value::from(1u8)),
    ]);
    while let Ok(first) = notices.recv() {
        std::thread::sleep(PAUSE);
        for notice in newest(first, notices) {
            let replaces = shown
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .last
                .get(&notice.terminal)
                .copied()
                .unwrap_or(0);
            let id: u32 = server.call(
                "Notify",
                &(
                    "Caret",
                    replaces,
                    crate::APP_ID,
                    &notice.title,
                    &notice.body,
                    ["default", gettext("Show").as_str()].as_slice(),
                    &hints,
                    -1i32,
                ),
            )?;
            let mut shown = shown.lock().unwrap_or_else(PoisonError::into_inner);
            shown.terminals.insert(id, notice.terminal);
            shown.last.insert(notice.terminal, id);
        }
    }
    Ok(())
}

/// `first` and those waiting after it, only the last for each tab: a tab's next
/// notification replaces the one before, so the others would never be read.
fn newest(first: Notice, waiting: &mpsc::Receiver<Notice>) -> Vec<Notice> {
    let mut notices = vec![first];
    for notice in waiting.try_iter() {
        notices.retain(|earlier| earlier.terminal != notice.terminal);
        notices.push(notice);
    }
    notices
}

/// Follows clicks on the notifications in `shown` on a thread of its own, telling
/// `clicked` of them with the token the server sends first.
fn follow_clicks(
    connection: &Connection,
    shown: Arc<Mutex<Shown>>,
    clicked: Sender<Clicked>,
) -> zbus::Result<()> {
    let server = Proxy::new(
        connection,
        "org.freedesktop.Notifications",
        "/org/freedesktop/Notifications",
        "org.freedesktop.Notifications",
    )?;
    let signals = server.receive_all_signals()?;
    let spawned = std::thread::Builder::new()
        .name("notification clicks".into())
        .spawn(move || {
            let mut tokens: HashMap<u32, String> = HashMap::new();
            for signal in signals {
                let header = signal.header();
                let Some(member) = header.member() else {
                    continue;
                };
                let body = signal.body();
                match member.as_str() {
                    "ActivationToken" => {
                        if let Ok((id, token)) = body.deserialize::<(u32, String)>() {
                            tokens.insert(id, token);
                        }
                    }
                    "ActionInvoked" => {
                        let Ok((id, _)) = body.deserialize::<(u32, String)>() else {
                            continue;
                        };
                        let terminal = shown
                            .lock()
                            .unwrap_or_else(PoisonError::into_inner)
                            .terminals
                            .get(&id)
                            .copied();
                        let token = tokens.remove(&id);
                        if let Some(terminal) = terminal
                            && clicked.send(Clicked { terminal, token }).is_err()
                        {
                            break;
                        }
                    }
                    "NotificationClosed" => {
                        if let Ok((id, _)) = body.deserialize::<(u32, u32)>() {
                            tokens.remove(&id);
                            let mut shown = shown.lock().unwrap_or_else(PoisonError::into_inner);
                            if let Some(terminal) = shown.terminals.remove(&id)
                                && shown.last.get(&terminal) == Some(&id)
                            {
                                shown.last.remove(&terminal);
                            }
                        }
                    }
                    _ => {}
                }
            }
        });
    if let Err(error) = spawned {
        tracing::warn!("starting to follow notification clicks: {error}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn notice(terminal: u32, body: &str) -> Notice {
        Notice {
            terminal: TerminalId(terminal),
            title: String::new(),
            body: body.to_owned(),
        }
    }

    #[test]
    fn only_the_last_notification_waiting_for_a_tab_is_sent() {
        let (sender, waiting) = mpsc::channel();
        for body in ["a", "b", "c"] {
            sender.send(notice(1, body)).unwrap();
        }
        sender.send(notice(2, "d")).unwrap();
        sender.send(notice(1, "e")).unwrap();
        let sent = newest(notice(1, "first"), &waiting);
        let sent: Vec<(u32, &str)> = sent
            .iter()
            .map(|notice| (notice.terminal.0, notice.body.as_str()))
            .collect();
        assert_eq!(sent, [(2, "d"), (1, "e")]);
    }
}
