//! Caret's window: it shows the terminals the server keeps, drawn with Vulkan.

mod access;
mod app;
mod atlas;
mod box_drawing;
mod chrome;
mod cli;
mod color_glyph;
mod config;
mod connection;
mod desktop;
mod fonts;
mod glide;
mod gpu;
mod grid;
mod i18n;
mod icon_theme;
mod keys;
mod notifications;
mod paint;
mod palette;
mod panes;
mod presets;
mod render;
mod renderer;
mod shortcuts;
mod state;
mod text;
mod timings;

use std::process::ExitCode;

use clap::{CommandFactory, Parser};
use clap_complete::CompleteEnv;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

/// Debug builds run as the separate `.Devel` application, with a server of their own.
const APP_ID: &str = if cfg!(debug_assertions) {
    "io.github.sachesi.caret.Devel"
} else {
    "io.github.sachesi.caret"
};

fn main() -> ExitCode {
    caret_protocol::ask_for_quick_turns();
    CompleteEnv::with_factory(cli::Cli::command)
        .completer("caret")
        .var(cli::COMPLETE)
        .complete();
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::WARN.into())
                .from_env_lossy(),
        )
        .with_writer(std::io::stderr)
        .init();
    // SAFETY: no thread has started yet.
    #[expect(unsafe_code, reason = "the locale is set before any thread starts")]
    unsafe {
        i18n::init();
    }
    let mut start = match cli::Cli::parse().parsed() {
        Err(start) => start,
        Ok(cli::Noun::Session(command)) => return cli::session(command),
        Ok(cli::Noun::Completions(command)) => return cli::completions(&command),
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
