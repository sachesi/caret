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
pub mod shell;
pub mod terminal_view;
pub mod window;

pub use adw::{gdk, gio, glib, gtk};
pub use libadwaita as adw;

/// Set beside `GSK_RENDERER` when Caret chose GTK's renderer itself, so that the
/// programs it starts get neither.
pub const CHOSE_RENDERER: &str = "CARET_CHOSE_GSK_RENDERER";

/// Translations, resources, the application name, GTK's renderer: everything that is
/// settled before GTK starts.
///
/// # Safety
///
/// Call it first in `main`, before any thread is started: it sets the locale and the
/// environment, which other threads may be reading.
pub unsafe fn init_early() {
    // SAFETY: the caller has started no thread yet.
    unsafe { gettextrs::setlocale(gettextrs::LocaleCategory::LcAll, "") };
    // The terminal draws into OpenGL textures, which GTK's GL renderer takes as they are.
    // Its Vulkan renderer exports each one as a dmabuf, which some drivers cannot import
    // (radv refuses Mesa's implicit modifier), and then copies every frame through the
    // CPU: 12 ms a frame on the main thread instead of 2.
    if std::env::var_os("GSK_RENDERER").is_none() {
        // SAFETY: as above.
        unsafe {
            std::env::set_var("GSK_RENDERER", "gl");
            std::env::set_var(CHOSE_RENDERER, "1");
        }
    }
    gettextrs::bindtextdomain(config::GETTEXT_PACKAGE, config::LOCALEDIR).ok();
    gettextrs::bind_textdomain_codeset(config::GETTEXT_PACKAGE, "UTF-8").ok();
    gettextrs::textdomain(config::GETTEXT_PACKAGE).ok();

    gio::resources_register_include!("caret.gresource").expect("register resources");
    glib::set_application_name("Caret");
}
