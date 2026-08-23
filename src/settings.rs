//! The application's GSettings and what they resolve to.

use crate::adw::prelude::*;
use crate::gtk::pango;
use crate::{adw, config, gio};

thread_local! {
    static SETTINGS: gio::Settings = gio::Settings::new(config::APP_ID);
}

pub fn settings() -> gio::Settings {
    SETTINGS.with(Clone::clone)
}

/// The font chosen in the preferences, or the desktop's monospace font.
pub fn font() -> pango::FontDescription {
    let chosen = settings().string("font");
    let name = if chosen.is_empty() {
        adw::StyleManager::default().monospace_font_name()
    } else {
        chosen
    };
    let mut font = pango::FontDescription::from_string(&name);
    if font.family().is_none_or(|family| family.is_empty()) {
        font.set_family("Monospace");
    }
    if font.size() <= 0 {
        font.set_size(11 * pango::SCALE);
    }
    font
}

pub fn apply_color_scheme() {
    let scheme = match settings().string("color-scheme").as_str() {
        "light" => adw::ColorScheme::ForceLight,
        "dark" => adw::ColorScheme::ForceDark,
        _ => adw::ColorScheme::Default,
    };
    adw::StyleManager::default().set_color_scheme(scheme);
}
