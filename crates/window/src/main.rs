//! Caret's window: it shows the terminals the server keeps, drawn with Vulkan.

mod app;
mod atlas;
mod box_drawing;
mod chrome;
mod cli;
mod config;
mod connection;
mod desktop;
mod fonts;
mod gpu;
mod grid;
mod keys;
mod notifications;
mod palette;
mod panes;
mod renderer;
mod shortcuts;
mod state;
mod timings;

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
    let mut start = match cli::parse(std::env::args_os().skip(1)) {
        Ok(cli::Parsed::Start(start)) => start,
        Ok(cli::Parsed::Help) => {
            println!("{}", cli::USAGE);
            return ExitCode::SUCCESS;
        }
        Ok(cli::Parsed::Version) => {
            println!("caret {}", env!("CARGO_PKG_VERSION"));
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("caret: {error}\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    if start.attach == cli::Attach::Detached {
        let detached = match cli::detached() {
            Ok(detached) => detached,
            Err(error) => {
                eprintln!("caret: {error}");
                return ExitCode::FAILURE;
            }
        };
        let Some((first, others)) = detached.split_first() else {
            eprintln!("caret: no windows were detached");
            return ExitCode::FAILURE;
        };
        for session in others {
            app::new_window(&["--session", &session.0.to_string()], None);
        }
        start.attach = cli::Attach::Session(*first);
    }
    config::import();
    match app::run(start) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("caret: {error}");
            ExitCode::FAILURE
        }
    }
}
