pub const APP_ID: &str = "io.github.sachesi.caret";
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
