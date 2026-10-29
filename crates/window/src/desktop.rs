//! The desktop's preferences the window follows, read from the settings portal: a light
//! or dark style, and the monospace font.

use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::mpsc;

use smithay_client_toolkit::reexports::calloop::channel::Sender;
use zbus::blocking::{Connection, Proxy};
use zbus::zvariant::{OwnedValue, Value};

const APPEARANCE: &str = "org.freedesktop.appearance";
const INTERFACE: &str = "org.gnome.desktop.interface";

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Desktop {
    /// The desktop prefers dark styles; without a preference, light ones.
    pub dark: bool,
    /// The desktop's monospace font, as a family and a size.
    pub monospace: Option<String>,
}

/// Follows the desktop's preferences on a thread of their own, sending them to `changes`
/// as they are at first and then each change. The receiver gets the first too, and
/// nothing if the portal cannot be reached.
pub fn follow(changes: Sender<Desktop>) -> mpsc::Receiver<Desktop> {
    let (first, receiver) = mpsc::sync_channel(1);
    let spawned = std::thread::Builder::new()
        .name("desktop".into())
        .spawn(move || {
            if let Err(error) = listen(&first, &changes) {
                tracing::info!("reading the desktop's preferences: {error}");
            }
        });
    if let Err(error) = spawned {
        tracing::warn!("starting to follow the desktop's preferences: {error}");
    }
    receiver
}

fn listen(first: &mpsc::SyncSender<Desktop>, changes: &Sender<Desktop>) -> zbus::Result<()> {
    let connection = session()?;
    let portal = Proxy::new(
        &connection,
        "org.freedesktop.portal.Desktop",
        "/org/freedesktop/portal/desktop",
        "org.freedesktop.portal.Settings",
    )?;
    // Listening before reading misses no change in between.
    let signals = portal.receive_signal("SettingChanged")?;
    let mut desktop = Desktop {
        dark: read(&portal, APPEARANCE, "color-scheme").is_some_and(prefers_dark),
        monospace: read(&portal, INTERFACE, "monospace-font-name")
            .and_then(|font| String::try_from(font).ok()),
    };
    // The window may have stopped waiting, so the reading goes on as a change too.
    let _ = first.send(desktop.clone());
    if changes.send(desktop.clone()).is_err() {
        return Ok(());
    }
    for signal in signals {
        let (namespace, key, value): (String, String, OwnedValue) = signal.body().deserialize()?;
        let changed = match (namespace.as_str(), key.as_str()) {
            (APPEARANCE, "color-scheme") => Desktop {
                dark: prefers_dark(value),
                ..desktop.clone()
            },
            (INTERFACE, "monospace-font-name") => Desktop {
                monospace: String::try_from(value).ok(),
                ..desktop.clone()
            },
            _ => continue,
        };
        if changed != desktop {
            desktop = changed;
            if changes.send(desktop.clone()).is_err() {
                break;
            }
        }
    }
    Ok(())
}

/// The session bus. Connecting here, not through zbus, keeps zbus from starting the
/// pool it connects in, whose thread stays and wakes twice a second.
fn session() -> zbus::Result<Connection> {
    let address = std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default();
    let path = address
        .split(';')
        .filter_map(|address| address.strip_prefix("unix:"))
        .flat_map(|options| options.split(','))
        .find_map(|option| option.strip_prefix("path="))
        .map(PathBuf::from)
        .or_else(|| Some(PathBuf::from(std::env::var_os("XDG_RUNTIME_DIR")?).join("bus")))
        .ok_or_else(|| zbus::Error::Address("no session bus".into()))?;
    let stream = UnixStream::connect(path)?;
    zbus::blocking::connection::Builder::async_io_unix_stream(stream).build()
}

/// A setting, through the portal's newer method or its first one, whose answer wraps it
/// in a variant more.
fn read(portal: &Proxy, namespace: &str, key: &str) -> Option<OwnedValue> {
    if let Ok(value) = portal.call::<_, _, OwnedValue>("ReadOne", &(namespace, key)) {
        return Some(value);
    }
    let value: OwnedValue = portal.call("Read", &(namespace, key)).ok()?;
    match &*value {
        Value::Value(inner) => OwnedValue::try_from(&**inner).ok(),
        _ => Some(value),
    }
}

/// Whether `color-scheme` says the desktop prefers dark styles: 1, where 2 prefers light
/// ones and 0 neither.
fn prefers_dark(value: OwnedValue) -> bool {
    u32::try_from(value) == Ok(1)
}
