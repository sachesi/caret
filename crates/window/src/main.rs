//! Caret's window: it shows the terminals the server keeps, drawn with Vulkan.

mod app;
mod atlas;
mod connection;
mod fonts;
mod gpu;
mod grid;
mod keys;
mod palette;
mod renderer;

use std::process::ExitCode;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

/// Debug builds run as the separate `.Devel` application, with a server of their own.
const APP_ID: &str = if cfg!(debug_assertions) {
    "io.github.sachesi.caret.Devel"
} else {
    "io.github.sachesi.caret"
};

fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::WARN.into())
                .from_env_lossy(),
        )
        .init();
    match app::run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("caret: {error}");
            ExitCode::FAILURE
        }
    }
}
