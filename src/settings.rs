//! The application's `GSettings` and what they resolve to.

use adw::prelude::*;
use caret_protocol::{CursorShape, Options, Request};
use gtk::{gio, pango};

use crate::config;
use crate::palette::Palette;

thread_local! {
    static SETTINGS: gio::Settings = gio::Settings::new(config::APPLICATION_ID);
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

/// The light or dark palette, as the style is now.
pub fn palette() -> Palette {
    if adw::StyleManager::default().is_dark() {
        Palette::dark()
    } else {
        Palette::light()
    }
}

/// What the server is told of the terminals' colours and behaviour.
pub fn look() -> Request {
    let settings = settings();
    let cursor_shape = match settings.string("cursor-shape").as_str() {
        "beam" => CursorShape::Beam,
        "underline" => CursorShape::Underline,
        _ => CursorShape::Block,
    };
    Request::Look {
        palette: palette().colors(),
        options: Options {
            scrollback: settings.uint("scrollback-lines"),
            cursor_shape,
            cursor_blinks: gtk::Settings::default().is_none_or(|gtk| gtk.is_gtk_cursor_blink()),
            copy_from_programs: settings.boolean("program-clipboard"),
            scroll_on_output: settings.boolean("scroll-on-output"),
        },
    }
}
