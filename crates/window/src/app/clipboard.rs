//! The clipboard and the primary selection: what the window offers others, and what
//! it pastes from them.

use std::io::{Read, Write};

use caret_protocol::encode;
use caret_protocol::{Bytes, Mode, Request};
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
    pastes: Sender<Vec<u8>>,
}

impl Clipboard {
    pub fn new(
        manager: Option<DataDeviceManagerState>,
        primary_manager: Option<PrimarySelectionManagerState>,
        pastes: Sender<Vec<u8>>,
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
        let (qh, serial) = (&self.qh, self.serial);
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
            .spawn(move || {
                let mut bytes = Vec::new();
                match pipe.read_to_end(&mut bytes) {
                    Ok(_) => {
                        let _ = pastes.send(bytes);
                    }
                    Err(error) => tracing::warn!("reading the pasted text: {error}"),
                }
            });
        if let Err(error) = spawned {
            tracing::warn!("starting to read a paste: {error}");
        }
    }

    /// Types text read from a selection, marked as pasted for a program that asks.
    pub(super) fn pasted(&mut self, bytes: &[u8]) {
        let Some(terminal) = self.terminal() else {
            return;
        };
        let bracketed = self.grid.mode.contains(Mode::BRACKETED_PASTE);
        self.server.send(&Request::Input {
            terminal,
            bytes: Bytes(encode::paste(bytes, bracketed)),
            bottom: true,
        });
    }
}

/// The first of the text types an offer has, as Caret prefers them.
fn chosen(offered: &[String]) -> Option<String> {
    TEXT.iter()
        .find(|mime| offered.iter().any(|offered| offered == *mime))
        .map(|mime| (*mime).to_owned())
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
    }

    fn leave(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn motion(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice, _: f64, _: f64) {}

    fn selection(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}

    fn drop_performed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &WlDataDevice) {}
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
