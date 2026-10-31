//! Desktop notifications for tabs out of sight, sent to the session's notification
//! server on a thread of their own; clicking one tells the window to show its tab.

use std::collections::HashMap;
use std::sync::mpsc;
use std::sync::{Arc, Mutex, PoisonError};

use caret_protocol::TerminalId;
use gettextrs::gettext;
use smithay_client_toolkit::reexports::calloop::channel::Sender;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::Value;

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
    for notice in notices {
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
    Ok(())
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
