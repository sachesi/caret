pub const APP_ID: &str = "io.github.sachesi.caret";
/// Debug builds run as a separate application, so `cargo run` never hands its window to an
/// installed release that is already running, and keeps its own settings.
pub const DEVEL: bool = cfg!(debug_assertions);
pub const APPLICATION_ID: &str = if DEVEL {
    concat!("io.github.sachesi.caret", ".Devel")
} else {
    APP_ID
};
pub const RESOURCE_PATH: &str = "/io/github/sachesi/caret";
pub const VERSION: &str = match option_env!("CARET_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
pub const LOCALEDIR: &str = match option_env!("CARET_LOCALEDIR") {
    Some(v) => v,
    None => "/usr/local/share/locale",
};
pub const GETTEXT_PACKAGE: &str = "caret";
