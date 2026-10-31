//! The open menu as a popup of the window: the compositor keeps it on the screen, and it
//! may pass the window's edges. Its pointer events are told in the window's coordinates, so
//! the rest of the window finds its parts as it finds its own.

use std::collections::HashMap;

use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;
use smithay_client_toolkit::reexports::protocols::xdg::shell::client::xdg_positioner::{
    Anchor, ConstraintAdjustment, Gravity,
};
use smithay_client_toolkit::shell::xdg::XdgPositioner;
use smithay_client_toolkit::shell::xdg::XdgSurface;
use smithay_client_toolkit::shell::xdg::popup::{Popup, PopupConfigure, PopupHandler};
use smithay_client_toolkit::shm::slot::{Buffer, SlotPool};
use smithay_client_toolkit::shm::{Shm, ShmHandler};

use super::App;
use crate::atlas::{COLOR, GpuGlyph};
use crate::chrome::{self, Drawing};
use crate::fonts::{Fonts, Raster};
use crate::paint;

/// What was asked of the compositor, which needs a new popup when it changes.
#[derive(Clone, Copy, PartialEq)]
struct Placement {
    at: (f32, f32),
    main: bool,
    size: (i32, i32),
}

pub struct MenuPopup {
    popup: Popup,
    placement: Placement,
    configured: bool,
    viewport: Option<WpViewport>,
    /// What has been drawn, so that a hover does not shape its text again.
    known: HashMap<Drawing, u32>,
    rasters: Vec<Raster>,
    /// Kept while the compositor may be showing it.
    buffer: Option<Buffer>,
}

impl App {
    /// Makes the popup match the menu: opens, replaces or closes it, or draws it again.
    pub(super) fn sync_popup(&mut self) {
        let scale = self.factor() as f32;
        let fonts = &self.fonts;
        let Some(menu) = &mut self.chrome.menu else {
            self.menu_popup = None;
            return;
        };
        menu.fit(|text| fonts.label_advance(text, false) / scale);
        let (width, height) = menu.size();
        let placement = Placement {
            at: menu.at,
            main: menu.main,
            size: (width.ceil() as i32, height.ceil() as i32),
        };
        if self
            .menu_popup
            .as_ref()
            .is_none_or(|popup| popup.placement != placement)
        {
            self.menu_popup = None;
            if let Some(menu) = &mut self.chrome.menu {
                menu.placed = None;
            }
            self.open_popup(placement);
            return;
        }
        self.draw_popup();
    }

    fn open_popup(&mut self, placement: Placement) {
        let Some((window_width, window_height)) = self.size else {
            return;
        };
        let positioner = match XdgPositioner::new(&self.shell) {
            Ok(positioner) => positioner,
            Err(error) => {
                tracing::warn!("placing a menu: {error}");
                return;
            }
        };
        // The anchor has to touch the window.
        let x = (placement.at.0 as i32).clamp(0, window_width.saturating_sub(1) as i32);
        let y = (placement.at.1 as i32).clamp(0, window_height.saturating_sub(1) as i32);
        positioner.set_size(placement.size.0, placement.size.1);
        positioner.set_anchor_rect(x, y, 1, 1);
        positioner.set_anchor(Anchor::TopLeft);
        positioner.set_gravity(if placement.main {
            Gravity::BottomLeft
        } else {
            Gravity::BottomRight
        });
        positioner.set_constraint_adjustment(
            ConstraintAdjustment::FlipX
                | ConstraintAdjustment::FlipY
                | ConstraintAdjustment::SlideX
                | ConstraintAdjustment::SlideY
                | ConstraintAdjustment::ResizeY,
        );
        let popup = match Popup::new(
            self.window.xdg_surface(),
            &positioner,
            &self.qh,
            &self.compositor,
            &self.shell,
        ) {
            Ok(popup) => popup,
            Err(error) => {
                tracing::warn!("opening a menu: {error}");
                return;
            }
        };
        let room = chrome::MENU_SHADOW as i32;
        popup
            .xdg_surface()
            .set_window_geometry(room, room, placement.size.0, placement.size.1);
        if let Some(seat) = &self.seat {
            popup.xdg_popup().grab(seat, self.serial);
        }
        let viewport = self.fractional.as_ref().map(|fractional| {
            fractional
                .viewporter
                .get_viewport(popup.wl_surface(), &self.qh, super::scale::Objects)
        });
        popup.wl_surface().commit();
        self.menu_popup = Some(MenuPopup {
            popup,
            placement,
            configured: false,
            viewport,
            known: HashMap::new(),
            rasters: Vec::new(),
            buffer: None,
        });
    }

    /// Paints the menu into a new buffer of the popup's surface.
    fn draw_popup(&mut self) {
        let factor = self.factor();
        let scale = factor as f32;
        let (Some(popup), Some(menu), Some(shm)) =
            (&mut self.menu_popup, &self.chrome.menu, &self.shm)
        else {
            return;
        };
        if !popup.configured {
            return;
        }
        let (logical_width, logical_height) = menu.shown_size();
        let room = chrome::MENU_SHADOW;
        let device = |logical: f32| ((logical + 2.0 * room) * scale).ceil() as i32;
        let (width, height) = (device(logical_width), device(logical_height));
        let (fonts, theme, known, rasters) = (
            &mut self.fonts,
            self.desktop.icon_theme.as_deref(),
            &mut popup.known,
            &mut popup.rasters,
        );
        let line = fonts.label_height();
        let (quads, clipped_from, clip) = self.chrome.menu_quads(scale, line, |drawing| {
            let index = if let Some(&index) = known.get(drawing) {
                index
            } else {
                rasters.push(drawn(fonts, drawing, scale, theme)?);
                let index = rasters.len() as u32;
                known.insert(drawing.clone(), index);
                index
            };
            let raster = &rasters[index as usize - 1];
            Some((index, glyph(raster)))
        });
        if self.popup_pool.is_none() {
            match SlotPool::new((width * height * 4) as usize, shm) {
                Ok(pool) => self.popup_pool = Some(pool),
                Err(error) => {
                    tracing::warn!("making memory for a menu: {error}");
                    return;
                }
            }
        }
        let Some(pool) = &mut self.popup_pool else {
            return;
        };
        let made = pool.create_buffer(
            width,
            height,
            width * 4,
            smithay_client_toolkit::reexports::client::protocol::wl_shm::Format::Argb8888,
        );
        let (buffer, pixels) = match made {
            Ok(made) => made,
            Err(error) => {
                tracing::warn!("making a buffer for a menu: {error}");
                return;
            }
        };
        pixels.fill(0);
        paint::paint(pixels, width as u32, &quads, rasters, (clipped_from, clip));
        let surface = popup.popup.wl_surface();
        match &popup.viewport {
            Some(viewport) => viewport.set_destination(
                (logical_width + 2.0 * room).ceil() as i32,
                (logical_height + 2.0 * room).ceil() as i32,
            ),
            None => surface.set_buffer_scale(factor.round() as i32),
        }
        if let Err(error) = buffer.attach_to(surface) {
            tracing::warn!("showing a menu: {error}");
            return;
        }
        surface.damage_buffer(0, 0, width, height);
        surface.commit();
        popup.buffer = Some(buffer);
    }

    /// `position` on `surface` in the window's coordinates, where the surface is the menu's.
    pub(super) fn in_window(&self, surface: &WlSurface, position: (f64, f64)) -> (f64, f64) {
        let on_menu = self
            .menu_popup
            .as_ref()
            .is_some_and(|popup| popup.popup.wl_surface() == surface);
        match (
            on_menu,
            self.chrome.menu.as_ref().and_then(|menu| menu.placed),
        ) {
            (true, Some((x, y))) => {
                let shift = f64::from(self.chrome.margin()) - f64::from(chrome::MENU_SHADOW);
                (
                    position.0 + f64::from(x) + shift,
                    position.1 + f64::from(y) + shift,
                )
            }
            _ => position,
        }
    }

    /// Whether `surface` is the open menu's.
    pub(super) fn is_popup(&self, surface: &WlSurface) -> bool {
        self.menu_popup
            .as_ref()
            .is_some_and(|popup| popup.popup.wl_surface() == surface)
    }
}

/// The pixels of `drawing`.
pub(super) fn drawn(
    fonts: &mut Fonts,
    drawing: &Drawing,
    scale: f32,
    theme: Option<&str>,
) -> Option<Raster> {
    match drawing {
        Drawing::Label { text, width, bold } => fonts.label(text, *width, *bold),
        Drawing::Heading { text, width } => fonts.heading(text, *width),
        Drawing::Icon(icon) => theme
            .and_then(|theme| chrome::themed_icon(*icon, scale, theme))
            .or_else(|| Some(chrome::icon(*icon, scale))),
    }
}

/// What a quad needs to know of `raster`, as the atlas would have told it.
fn glyph(raster: &Raster) -> GpuGlyph {
    let half = |value: i32| u32::from(value as i16 as u16);
    GpuGlyph {
        position: 0,
        size: raster.width | (raster.height << 16),
        offset: half(raster.left) | (half(raster.top) << 16),
        flags: if raster.color { COLOR } else { 0 },
    }
}

impl PopupHandler for App {
    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        popup: &Popup,
        config: PopupConfigure,
    ) {
        let (Some(open), Some(menu)) = (&mut self.menu_popup, &mut self.chrome.menu) else {
            return;
        };
        if open.popup.wl_surface() != popup.wl_surface() {
            return;
        }
        open.configured = true;
        menu.placed = Some((config.position.0 as f32, config.position.1 as f32));
        menu.limit_height(config.height as f32);
        self.draw_popup();
        self.hover();
    }

    fn done(&mut self, _: &Connection, _: &QueueHandle<Self>, popup: &Popup) {
        if self.is_popup(popup.wl_surface()) {
            self.close_menu();
        }
    }
}

impl ShmHandler for App {
    fn shm_state(&mut self) -> &mut Shm {
        self.shm
            .as_mut()
            .expect("wl_shm is bound before any shm object")
    }
}
