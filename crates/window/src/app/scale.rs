//! Fractional scales: the compositor says how many device pixels a logical one takes, in
//! 120ths, and the window draws that many and has the compositor fit them to its size.

use smithay_client_toolkit::dispatch2::Dispatch2;
use smithay_client_toolkit::reexports::client::globals::GlobalList;
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::wp_fractional_scale_manager_v1::{
    self, WpFractionalScaleManagerV1,
};
use smithay_client_toolkit::reexports::protocols::wp::fractional_scale::v1::client::wp_fractional_scale_v1::{
    self, WpFractionalScaleV1,
};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::{
    self, WpViewport,
};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewporter::{
    self, WpViewporter,
};

use super::App;

/// A scale of one, in the 120ths scales come in.
pub const WHOLE: u32 = 120;

/// The window's surface, scaled by fractions.
pub struct Fractional {
    #[expect(
        dead_code,
        reason = "held so that the compositor keeps sending the scale"
    )]
    scale: WpFractionalScaleV1,
    /// Fits the pixels drawn to the window's size.
    pub viewport: WpViewport,
}

impl Fractional {
    /// Fractional scaling for `surface`, if the compositor offers it.
    pub fn new(globals: &GlobalList, qh: &QueueHandle<App>, surface: &WlSurface) -> Option<Self> {
        let manager: WpFractionalScaleManagerV1 = globals.bind(qh, 1..=1, Objects).ok()?;
        let viewporter: WpViewporter = globals.bind(qh, 1..=1, Objects).ok()?;
        Some(Self {
            scale: manager.get_fractional_scale(surface, qh, Objects),
            viewport: viewporter.get_viewport(surface, qh, Objects),
        })
    }
}

/// What the objects here carry, to be told their events.
pub struct Objects;

impl Dispatch2<WpFractionalScaleV1, App> for Objects {
    fn event(
        &self,
        app: &mut App,
        _: &WpFractionalScaleV1,
        event: wp_fractional_scale_v1::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
        if let wp_fractional_scale_v1::Event::PreferredScale { scale } = event {
            app.set_scale(scale);
        }
    }
}

impl Dispatch2<WpFractionalScaleManagerV1, App> for Objects {
    fn event(
        &self,
        _: &mut App,
        _: &WpFractionalScaleManagerV1,
        _: wp_fractional_scale_manager_v1::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch2<WpViewporter, App> for Objects {
    fn event(
        &self,
        _: &mut App,
        _: &WpViewporter,
        _: wp_viewporter::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}

impl Dispatch2<WpViewport, App> for Objects {
    fn event(
        &self,
        _: &mut App,
        _: &WpViewport,
        _: wp_viewport::Event,
        _: &Connection,
        _: &QueueHandle<App>,
    ) {
    }
}
