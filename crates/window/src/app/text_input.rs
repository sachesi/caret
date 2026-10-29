//! Input methods: text composed outside the keyboard's keymap, shown at the cursor while
//! it is composed and typed once it is done.

use caret_protocol::{Bytes, Request};
use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_seat::WlSeat;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::{
    self, ZwpTextInputManagerV3,
};
use smithay_client_toolkit::reexports::protocols::wp::text_input::zv3::client::zwp_text_input_v3::{
    self, ContentHint, ContentPurpose, ZwpTextInputV3,
};

use super::App;
use crate::chrome::Preedit;
use crate::grid::packed;

/// The seat's text input, enabled while the window has the keyboard.
pub struct TextInput {
    proxy: ZwpTextInputV3,
    enabled: bool,
    /// What the input method sent since its last `done`.
    preedit: Option<String>,
    commit: Option<String>,
    /// The cursor's rectangle as last told, in logical pixels from the surface's corner.
    rectangle: Option<[i32; 4]>,
}

impl TextInput {
    /// The seat's text input, if the compositor offers input methods.
    pub fn new(
        manager: Option<&ZwpTextInputManagerV3>,
        seat: &WlSeat,
        qh: &QueueHandle<App>,
    ) -> Option<Self> {
        Some(Self {
            proxy: manager?.get_text_input(seat, qh, Objects),
            enabled: false,
            preedit: None,
            commit: None,
            rectangle: None,
        })
    }

    pub fn bind(globals: &GlobalList, qh: &QueueHandle<App>) -> Option<ZwpTextInputManagerV3> {
        globals.bind(qh, 1..=1, Objects).ok()
    }

    fn enable(&mut self) {
        self.enabled = true;
        self.rectangle = None;
        self.proxy.enable();
        self.proxy
            .set_content_type(ContentHint::None, ContentPurpose::Terminal);
        self.proxy.commit();
    }

    fn disable(&mut self) {
        self.enabled = false;
        self.proxy.disable();
        self.proxy.commit();
    }
}

impl App {
    /// Tells the input method where the cursor is, for it to place what it shows there.
    pub(super) fn place_text_input(&mut self) {
        let Some(cursor) = self.grid.cursor else {
            return;
        };
        let scale = self.factor();
        let cell = self.fonts.cell();
        let (x, y) = self.origin();
        let logical = |device: i32| (f64::from(device) / scale).round() as i32;
        let rectangle = [
            logical(x + i32::from(cursor.point.column) * cell.width),
            logical(y + i32::from(cursor.point.line) * cell.height),
            logical(cell.width).max(1),
            logical(cell.height).max(1),
        ];
        let Some(input) = self.text_input.as_mut().filter(|input| input.enabled) else {
            return;
        };
        if input.rectangle != Some(rectangle) {
            input.rectangle = Some(rectangle);
            let [x, y, width, height] = rectangle;
            input.proxy.set_cursor_rectangle(x, y, width, height);
            input.proxy.commit();
        }
    }

    /// Where the text being composed shows: over the cursor's cell, in device pixels.
    pub(super) fn place_preedit(&mut self) {
        let Some(cursor) = self.grid.cursor else {
            return;
        };
        let cell = self.fonts.cell();
        let (x, y) = self.origin();
        if let Some(preedit) = &mut self.chrome.preedit {
            preedit.at = (
                (x + i32::from(cursor.point.column) * cell.width) as f32,
                (y + i32::from(cursor.point.line) * cell.height) as f32,
            );
            preedit.height = cell.height as f32;
            preedit.color = (packed(self.palette.foreground) << 8) | 0xff;
        }
    }

    /// Takes what the input method composed: the text still being composed to show,
    /// and the text done to type.
    fn text_input_done(&mut self) {
        let Some(input) = &mut self.text_input else {
            return;
        };
        let (preedit, commit) = (input.preedit.take(), input.commit.take());
        let preedit = preedit.filter(|text| !text.is_empty()).map(|text| Preedit {
            text,
            ..Preedit::default()
        });
        if preedit.as_ref().map(|preedit| &preedit.text)
            != self.chrome.preedit.as_ref().map(|preedit| &preedit.text)
        {
            self.chrome.preedit = preedit;
            self.dirty = true;
            self.draw();
        }
        let Some(text) = commit.filter(|text| !text.is_empty()) else {
            return;
        };
        if let Some(field) = self
            .chrome
            .dialog
            .as_mut()
            .filter(|dialog| dialog.focused.is_none())
            .and_then(|dialog| dialog.field.as_mut())
        {
            field.type_text(&text);
            self.dirty = true;
            self.draw();
            return;
        }
        if self.search_type(&text) {
            return;
        }
        if let Some(terminal) = self.terminal() {
            self.server.send(&Request::Input {
                terminal,
                bytes: Bytes(text.into_bytes()),
                bottom: self.config.scroll_on_keystroke,
            });
        }
    }
}

/// What the objects here carry, to be told their events.
pub struct Objects;

impl Dispatch2<ZwpTextInputV3, App> for Objects {
    fn event(
        &self,
        app: &mut App,
        _: &ZwpTextInputV3,
        event: zwp_text_input_v3::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        let Some(input) = &mut app.text_input else {
            return;
        };
        match event {
            zwp_text_input_v3::Event::Enter { .. } => {
                input.enable();
                app.place_text_input();
            }
            zwp_text_input_v3::Event::Leave { .. } => {
                input.disable();
                if app.chrome.preedit.take().is_some() {
                    app.dirty = true;
                    app.draw();
                }
            }
            zwp_text_input_v3::Event::PreeditString { text, .. } => input.preedit = text,
            zwp_text_input_v3::Event::CommitString { text } => input.commit = text,
            zwp_text_input_v3::Event::Done { .. } => app.text_input_done(),
            _ => {}
        }
    }
}

impl Dispatch2<ZwpTextInputManagerV3, App> for Objects {
    fn event(
        &self,
        _: &mut App,
        _: &ZwpTextInputManagerV3,
        _: zwp_text_input_manager_v3::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}
