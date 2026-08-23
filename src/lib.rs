pub mod application;
pub mod box_drawing;
pub mod config;
pub mod encode;
pub mod fonts;
pub mod frame;
pub mod links;
pub mod palette;
pub mod preferences;
pub mod renderer;
pub mod session;
pub mod settings;
pub mod terminal_view;
pub mod window;

pub use adw::{gdk, gio, glib, gtk};
pub use libadwaita as adw;

/// Translations, resources, the application name: everything that does not touch GTK.
///
/// # Safety
///
/// Call it first in `main`, before any thread is started: it sets the locale, which reads
/// the environment and changes state other threads may be reading.
pub unsafe fn init_early() {
    // SAFETY: the caller has started no thread yet.
    unsafe { gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "") };
    gettextrs::bindtextdomain(config::GETTEXT_PACKAGE, config::LOCALEDIR).ok();
    gettextrs::bind_textdomain_codeset(config::GETTEXT_PACKAGE, "UTF-8").ok();
    gettextrs::textdomain(config::GETTEXT_PACKAGE).ok();

    gio::resources_register_include!("tangent.gresource").expect("register resources");
    glib::set_application_name("Tangent");
}
