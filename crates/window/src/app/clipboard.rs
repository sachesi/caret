//! The clipboard and the primary selection: what the window offers others, and what
//! it pastes from them; and what is dropped on it, typed as a paste is.

use std::io::{ErrorKind, Read, Write};

use caret_protocol::{Mode, encode};
use smithay_client_toolkit::data_device_manager::data_device::{DataDevice, DataDeviceHandler};
use smithay_client_toolkit::data_device_manager::data_offer::{DataOfferHandler, DragOffer};
use smithay_client_toolkit::data_device_manager::data_source::{
    CopyPasteSource, DataSourceHandler,
};
use smithay_client_toolkit::data_device_manager::{DataDeviceManagerState, WritePipe};
use smithay_client_toolkit::primary_selection::PrimarySelectionManagerState;
use smithay_client_toolkit::primary_selection::device::{
    PrimarySelectionDevice, PrimarySelectionDeviceHandler,
};
use smithay_client_toolkit::primary_selection::selection::{
    PrimarySelectionSource, PrimarySelectionSourceHandler,
};
use smithay_client_toolkit::reexports::calloop::channel::Sender;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device::WlDataDevice;
use smithay_client_toolkit::reexports::client::protocol::wl_data_device_manager::DndAction;
use smithay_client_toolkit::reexports::client::protocol::wl_data_source::WlDataSource;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::primary_selection::zv1::client::zwp_primary_selection_device_v1::ZwpPrimarySelectionDeviceV1;
use smithay_client_toolkit::reexports::protocols::wp::primary_selection::zv1::client::zwp_primary_selection_source_v1::ZwpPrimarySelectionSourceV1;

use super::App;

/// The most of another program's text taken in at once.
const LONGEST_PASTE: u64 = 256 << 20;

/// What a drop of files comes as.
const URI_LIST: &str = "text/uri-list";

/// Text types offered and asked for, the preferred first.
const TEXT: [&str; 4] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
];

pub struct Clipboard {
    manager: Option<DataDeviceManagerState>,
    primary_manager: Option<PrimarySelectionManagerState>,
    device: Option<DataDevice>,
    primary_device: Option<PrimarySelectionDevice>,
    source: Option<CopyPasteSource>,
    primary_source: Option<PrimarySelectionSource>,
    /// What the window offers as the clipboard and as the primary selection.
    text: String,
    primary_text: String,
    /// Where text read from another program's selection goes.
    /// What was read of pastes and drops; none for one past [`LONGEST_PASTE`].
    pastes: Sender<Option<Vec<u8>>>,
}

impl Clipboard {
    pub fn new(
        manager: Option<DataDeviceManagerState>,
        primary_manager: Option<PrimarySelectionManagerState>,
        pastes: Sender<Option<Vec<u8>>>,
    ) -> Self {
        Self {
            manager,
            primary_manager,
            device: None,
            primary_device: None,
            source: None,
            primary_source: None,
            text: String::new(),
            primary_text: String::new(),
            pastes,
        }
    }

    /// What is being dragged over the window.
    fn drag_offer(&self) -> Option<DragOffer> {
        self.device.as_ref()?.data().drag_offer()
    }

    pub fn add_seat(&mut self, qh: &QueueHandle<App>, seat: &WlSeat) {
        if self.device.is_none()
            && let Some(manager) = &self.manager
        {
            self.device = Some(manager.get_data_device(qh, seat));
        }
        if self.primary_device.is_none()
            && let Some(manager) = &self.primary_manager
        {
            self.primary_device = Some(manager.get_selection_device(qh, seat));
        }
    }
}

impl App {
    /// Offers `text` as the clipboard, or as the primary selection.
    pub(super) fn copy(&mut self, text: String, primary: bool) {
        // The compositor takes a selection only with a serial of input it sent lately: the
        // later of the last press and the keys' coming to the window, past a wrap included.
        let newer = self.entered_serial.wrapping_sub(self.serial).cast_signed() > 0;
        let serial = if newer {
            self.entered_serial
        } else {
            self.serial
        };
        let qh = &self.qh;
        let clipboard = &mut self.clipboard;
        if primary {
            let (Some(manager), Some(device)) =
                (&clipboard.primary_manager, &clipboard.primary_device)
            else {
                return;
            };
            let source = manager.create_selection_source(qh, TEXT);
            source.set_selection(device, serial);
            clipboard.primary_source = Some(source);
            clipboard.primary_text = text;
        } else {
            let (Some(manager), Some(device)) = (&clipboard.manager, &clipboard.device) else {
                return;
            };
            let source = manager.create_copy_paste_source(qh, TEXT);
            source.set_selection(device, serial);
            clipboard.source = Some(source);
            clipboard.text = text;
        }
    }

    /// Reads the clipboard, or the primary selection, and types it.
    pub(super) fn paste(&mut self, primary: bool) {
        let clipboard = &self.clipboard;
        let pipe = if primary {
            clipboard
                .primary_device
                .as_ref()
                .and_then(|device| device.data().selection_offer())
                .and_then(|offer| {
                    let mime = offer.with_mime_types(chosen)?;
                    offer.receive(mime).ok()
                })
        } else {
            clipboard
                .device
                .as_ref()
                .and_then(|device| device.data().selection_offer())
                .and_then(|offer| {
                    let mime = offer.with_mime_types(chosen)?;
                    offer.receive(mime).ok()
                })
        };
        let Some(mut pipe) = pipe else {
            return;
        };
        let pastes = clipboard.pastes.clone();
        // The other program writes at its own pace, so the pipe is read off the loop.
        let spawned = std::thread::Builder::new()
            .name("paste".to_owned())
            .spawn(move || match read_whole(&mut pipe) {
                Ok(bytes) => {
                    let _ = pastes.send(Some(bytes));
                }
                Err(error) if error.kind() == ErrorKind::FileTooLarge => {
                    let _ = pastes.send(None);
                }
                Err(error) => tracing::warn!("reading the pasted text: {error}"),
            });
        if let Err(error) = spawned {
            tracing::warn!("starting to read a paste: {error}");
        }
    }

    /// Types text read from a selection, marked as pasted for a program that asks.
    pub(super) fn pasted(&mut self, bytes: &[u8]) {
        if self.type_in_field(&one_line(bytes)) {
            return;
        }
        let Some(terminal) = self.terminal() else {
            return;
        };
        let bracketed = self.grid.mode.contains(Mode::BRACKETED_PASTE);
        self.paste_into(terminal, bytes, bracketed);
    }
}

/// `bytes` as a field can hold them: a line, with spaces for what breaks it.
fn one_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches(['\r', '\n'])
        .chars()
        .map(|letter| if letter.is_control() { ' ' } else { letter })
        .collect()
}

/// All of what another program offers, unless it runs past [`LONGEST_PASTE`]: half a paste
/// could run half of what was meant, and one without end would fill the window's memory.
fn read_whole(pipe: &mut impl Read) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    pipe.take(LONGEST_PASTE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > LONGEST_PASTE {
        return Err(std::io::Error::new(
            ErrorKind::FileTooLarge,
            format!("longer than {LONGEST_PASTE} bytes"),
        ));
    }
    Ok(bytes)
}

/// The first of the text types an offer has, as Caret prefers them.
fn chosen(offered: &[String]) -> Option<String> {
    TEXT.iter()
        .find(|mime| offered.iter().any(|offered| offered == *mime))
        .map(|mime| (*mime).to_owned())
}

/// What Caret takes a drop as: files, or else text.
fn dropped_type(offered: &[String]) -> Option<String> {
    if offered.iter().any(|mime| mime == URI_LIST) {
        Some(URI_LIST.to_owned())
    } else {
        chosen(offered)
    }
}

/// The files of a `text/uri-list` as a line of their paths, each quoted for the shell;
/// an address not of a file is kept as it is written.
fn paths(list: &[u8]) -> Vec<u8> {
    let quoted: Vec<Vec<u8>> = list
        .split(|&byte| byte == b'\n')
        .map(|line| line.strip_suffix(b"\r").unwrap_or(line))
        .filter(|line| !line.is_empty() && !line.starts_with(b"#"))
        .map(|uri| {
            let path = uri
                .strip_prefix(b"file://")
                .and_then(|rest| {
                    rest.iter()
                        .position(|&byte| byte == b'/')
                        .map(|at| &rest[at..])
                })
                .map_or_else(|| uri.to_vec(), unescaped);
            encode::shell_quoted(&path)
        })
        .collect();
    quoted.join(&b' ')
}

/// `text` with each `%XX` turned back into the byte it stands for.
fn unescaped(text: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len());
    let mut rest = text;
    while let Some((&byte, tail)) = rest.split_first() {
        let escaped = (byte == b'%')
            .then(|| tail.get(..2))
            .flatten()
            .and_then(|hex| u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok());
        if let Some(escaped) = escaped {
            bytes.push(escaped);
            rest = &tail[2..];
        } else {
            bytes.push(byte);
            rest = tail;
        }
    }
    bytes
}

/// Writes `text` to a program pasting it, off the loop, since it reads at its own pace.
fn send(text: String, mut pipe: WritePipe) {
    let spawned = std::thread::Builder::new()
        .name("copy".to_owned())
        .spawn(move || {
            if let Err(error) = pipe.write_all(text.as_bytes()) {
                tracing::debug!("writing the copied text: {error}");
            }
        });
    if let Err(error) = spawned {
        tracing::warn!("starting to write a copy: {error}");
    }
}

impl DataDeviceHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataDevice,
        _: f64,
        _: f64,
        _: &WlSurface,
    ) {
        let Some(offer) = self.clipboard.drag_offer() else {
            return;
        };
        let mime = offer.with_mime_types(dropped_type);
        let action = if mime.is_some() {
            DndAction::Copy
        } else {
            DndAction::None
        };
        offer.accept_mime_type(offer.serial, mime);
        offer.set_actions(action, action);
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {
        let Some(offer) = self.clipboard.drag_offer() else {
            return;
        };
        let Some(mime) = offer.with_mime_types(dropped_type) else {
            offer.destroy();
            return;
        };
        let mut pipe = match offer.receive(mime.clone()) {
            Ok(pipe) => pipe,
            Err(error) => {
                tracing::warn!("reading what was dropped: {error}");
                offer.destroy();
                return;
            }
        };
        let pastes = self.clipboard.pastes.clone();
        // The other program writes at its own pace, and may stop once the drop is
        // finished, so the drop is finished once all of it is read.
        let spawned = std::thread::Builder::new()
            .name("drop".to_owned())
            .spawn(move || {
                let read = read_whole(&mut pipe);
                offer.finish();
                offer.destroy();
                match read {
                    Ok(bytes) if mime == URI_LIST => {
                        let _ = pastes.send(Some(paths(&bytes)));
                    }
                    Ok(bytes) => {
                        let _ = pastes.send(Some(bytes));
                    }
                    Err(error) if error.kind() == ErrorKind::FileTooLarge => {
                        let _ = pastes.send(None);
                    }
                    Err(error) => tracing::warn!("reading what was dropped: {error}"),
                }
            });
        if let Err(error) = spawned {
            tracing::warn!("starting to read a drop: {error}");
        }
    }
}

impl DataOfferHandler for App {
    fn source_actions(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }

    fn selected_action(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &mut DragOffer,
        _: DndAction,
    ) {
    }
}

impl DataSourceHandler for App {
    fn accept_mime(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        _: Option<String>,
    ) {
    }

    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &WlDataSource,
        mime: String,
        pipe: WritePipe,
    ) {
        if TEXT.contains(&mime.as_str()) {
            send(self.clipboard.text.clone(), pipe);
        }
    }

    fn cancelled(&mut self, _: &Connection, _: &QueueHandle<Self>, source: &WlDataSource) {
        if self
            .clipboard
            .source
            .as_ref()
            .is_some_and(|ours| ours.inner() == source)
        {
            self.clipboard.source = None;
        }
    }

    fn dnd_dropped(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn dnd_finished(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource) {}

    fn action(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataSource, _: DndAction) {}
}

impl PrimarySelectionDeviceHandler for App {
    fn selection(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpPrimarySelectionDeviceV1,
    ) {
    }
}

impl PrimarySelectionSourceHandler for App {
    fn send_request(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &ZwpPrimarySelectionSourceV1,
        mime: String,
        pipe: WritePipe,
    ) {
        if TEXT.contains(&mime.as_str()) {
            send(self.clipboard.primary_text.clone(), pipe);
        }
    }

    fn cancelled(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        source: &ZwpPrimarySelectionSourceV1,
    ) {
        if self
            .clipboard
            .primary_source
            .as_ref()
            .is_some_and(|ours| ours.inner() == source)
        {
            self.clipboard.primary_source = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paste_is_taken_whole_up_to_its_limit() {
        let mut short: &[u8] = b"text";
        assert_eq!(read_whole(&mut short).unwrap(), b"text");
        let mut endless = std::io::repeat(b'x');
        assert!(read_whole(&mut endless).is_err());
    }

    #[test]
    fn a_paste_into_a_field_stays_on_one_line() {
        assert_eq!(one_line(b"two\nlines\r\n"), "two lines");
    }

    #[test]
    fn dropped_files_become_their_quoted_paths() {
        let list = b"# from a file manager\r\nfile:///home/me/My%20Notes.txt\r\n\
            file://host/tmp/plain\r\nhttps://example.org/a\r\n";
        assert_eq!(
            paths(list),
            b"'/home/me/My Notes.txt' /tmp/plain https://example.org/a"
        );
        assert_eq!(unescaped(b"100%25 %zz%4"), b"100% %zz%4");
    }
}
