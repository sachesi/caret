//! Passing the focus between Caret's windows: a window opened from another starts with
//! a token the compositor gave for the key or click that asked for it, which lets it
//! take the focus, and a notification clicked brings its window forward.

use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::xdg::activation::v1::client::xdg_activation_token_v1::{
    self, XdgActivationTokenV1,
};
use smithay_client_toolkit::reexports::protocols::xdg::activation::v1::client::xdg_activation_v1::{
    self, XdgActivationV1,
};
use smithay_client_toolkit::shell::WaylandSurface;

use super::App;

/// What lets the window take the focus.
pub fn bind(globals: &GlobalList, qh: &QueueHandle<App>) -> Option<XdgActivationV1> {
    globals.bind(qh, 1..=1, Objects).ok()
}

impl App {
    /// Takes the focus with the token the window was started with, if it was.
    pub(super) fn activate_at_start(&self) {
        if let (Some(activation), Ok(token)) = (
            &self.offered.activation,
            std::env::var("XDG_ACTIVATION_TOKEN"),
        ) {
            activation.activate(token, self.window.wl_surface());
        }
    }

    /// Opens another window with `arguments`, with a token to take the focus once the
    /// compositor gives one.
    pub(super) fn open_window(&self, arguments: Vec<String>) {
        let Some(activation) = &self.offered.activation else {
            super::new_window(&arguments, None);
            return;
        };
        let token = activation.get_activation_token(&self.qh, Opening(arguments));
        if let Some(seat) = &self.seat {
            token.set_serial(self.serial, seat);
        }
        token.set_surface(self.window.wl_surface());
        token.set_app_id(crate::APP_ID.to_owned());
        token.commit();
    }
}

/// What the objects here carry, to be told their events.
pub struct Objects;

impl Dispatch2<XdgActivationV1, App> for Objects {
    fn event(
        &self,
        _: &mut App,
        _: &XdgActivationV1,
        _: xdg_activation_v1::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

/// The arguments of a window to open once its token comes.
pub struct Opening(Vec<String>);

impl Dispatch2<XdgActivationTokenV1, App> for Opening {
    fn event(
        &self,
        _: &mut App,
        token: &XdgActivationTokenV1,
        event: xdg_activation_token_v1::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let xdg_activation_token_v1::Event::Done { token: given } = event {
            super::new_window(&self.0, Some(&given));
            token.destroy();
        }
    }
}
