//! Caret's terminal server: it keeps the programs, their screens and their history, for
//! the windows that attach to it to show.

mod capture;
mod listen;
mod palette;
mod pty;
mod server;
mod terminal;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::LevelFilter;

pub const VERSION: &str = match option_env!("CARET_VERSION") {
    Some(version) => version,
    None => env!("CARGO_PKG_VERSION"),
};

/// The application whose windows connect: debug builds serve the separate `.Devel` one.
const APP_ID: &str = if cfg!(debug_assertions) {
    "io.github.sachesi.caret.Devel"
} else {
    "io.github.sachesi.caret"
};

const USAGE: &str = "Usage: caret-server [--socket PATH]

Keeps Caret's terminals for its windows, listening at PATH or in the runtime
directory, and ends once it keeps none and no window is attached. The first window
starts it, or systemd does for its socket.";

#[expect(
    unsafe_code,
    reason = "takes systemd's socket before any thread starts"
)]
fn main() -> ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            EnvFilter::builder()
                .with_default_directive(LevelFilter::WARN.into())
                .from_env_lossy(),
        )
        .with_writer(std::io::stderr)
        .init();

    let mut socket = None;
    let mut arguments = std::env::args_os().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--socket") => {
                let Some(path) = arguments.next() else {
                    eprintln!("--socket needs a path\n\n{USAGE}");
                    return ExitCode::FAILURE;
                };
                socket = Some(PathBuf::from(path));
            }
            Some("--version") => {
                println!("caret-server {VERSION}");
                return ExitCode::SUCCESS;
            }
            Some("--help") => {
                println!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("unknown argument {}\n\n{USAGE}", argument.to_string_lossy());
                return ExitCode::FAILURE;
            }
        }
    }

    pty::raise_descriptor_limit();
    // SAFETY: no thread has been started yet.
    let activated = unsafe { listen::activated() };
    let listener = if let Some(listener) = activated {
        listener
    } else {
        let Some(path) = socket.or_else(|| caret_protocol::socket_path(APP_ID)) else {
            eprintln!("XDG_RUNTIME_DIR is not set; give a socket with --socket");
            return ExitCode::FAILURE;
        };
        match listen::bind(&path) {
            Ok(listener) => listener,
            Err(error) => {
                eprintln!("listening at {}: {error}", path.display());
                return ExitCode::FAILURE;
            }
        }
    };

    let (events, inputs) = mpsc::channel();
    let accepting = events.clone();
    let spawned = std::thread::Builder::new()
        .name("accept".to_owned())
        .spawn(move || listen::accept(&listener, &accepting));
    if let Err(error) = spawned {
        eprintln!("starting to accept clients: {error}");
        return ExitCode::FAILURE;
    }
    server::Server::new(events).run(&inputs);
    ExitCode::SUCCESS
}
