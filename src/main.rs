mod application;
mod box_drawing;
mod config;
mod connection;
mod encode;
mod fonts;
mod frame;
mod i18n;
mod palette;
mod preferences;
mod renderer;
mod settings;
mod terminal_view;
mod timings;
mod window;

use gio::prelude::*;
use gtk::{gio, glib};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

use crate::application::Application;

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
#[expect(
    unsafe_code,
    reason = "sets the locale and the environment, unsound once threads run"
)]
unsafe fn init_early() {
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
    i18n::init();

    gio::resources_register_include!("resources.gresource")
        .expect("the resources are compiled into the binary");
    glib::set_application_name("Caret");
}

#[expect(unsafe_code, reason = "calls init_early before any thread starts")]
fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::WARN.into())
                .from_env_lossy(),
        )
        .init();
    // SAFETY: no thread has been started yet.
    unsafe { init_early() };
    Application::new().run_with_args(&application::arguments())
}
