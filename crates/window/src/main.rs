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
mod gpu;
mod grid;
mod i18n;
mod keys;
mod notifications;
mod palette;
mod panes;
mod render;
mod renderer;
mod shortcuts;
mod state;
mod text;
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
        .with_writer(std::io::stderr)
        .init();
    // SAFETY: no thread has started yet.
    #[expect(unsafe_code, reason = "the locale is set before any thread starts")]
    unsafe {
        i18n::init();
    }
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
        Ok(cli::Parsed::Sessions) => {
            return match cli::sessions() {
                Ok(sessions) => {
                    print!("{}", cli::listing(&sessions));
                    ExitCode::SUCCESS
                }
                Err(error) => {
                    eprintln!("caret: {error}");
                    ExitCode::FAILURE
                }
            };
        }
        Ok(cli::Parsed::Attach(target)) => {
            return match attach(&target) {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("caret: {error}");
                    ExitCode::FAILURE
                }
            };
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

/// Shows the session `target` names: in the window this runs in, if it runs in one of
/// Caret's terminals; otherwise drawn in this terminal.
fn attach(target: &str) -> Result<(), String> {
    let sessions = cli::sessions()?;
    let session = cli::find(&sessions, target)?;
    match cli::own_terminal() {
        Some(terminal) => cli::show_in_window(terminal, session),
        None => text::run(session),
    }
}
