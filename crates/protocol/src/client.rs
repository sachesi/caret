//! A client's end of the socket: finding the server, starting one when none listens,
//! and the handshake.

use std::io;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, ExitStatus};
use std::time::{Duration, Instant};

const SERVER: &str = "caret-server";

#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    #[error("XDG_RUNTIME_DIR is not set")]
    NoRuntimeDirectory,
    #[error("connecting to {}: {source}", path.display())]
    Connect { path: PathBuf, source: io::Error },
    #[error("starting {}: {source}", program.display())]
    Start { program: PathBuf, source: io::Error },
    #[error("{} ended: {status}", program.display())]
    Ended {
        program: PathBuf,
        status: ExitStatus,
    },
    #[error("greeting the server: {0}")]
    Greet(io::Error),
}

/// A connection to the server of the application `app_id`, greeted with versions up to
/// `newest`, and the version agreed. systemd starts the server for its socket where its
/// units are installed; elsewhere this does.
///
/// # Errors
///
/// When there is no runtime directory, the server cannot be started or reached, or it
/// speaks no version this client does.
pub fn connect(app_id: &str, newest: u16) -> Result<(UnixStream, u16), ConnectError> {
    let path = crate::socket_path(app_id).ok_or(ConnectError::NoRuntimeDirectory)?;
    let mut stream = connect_or_start(&path)?;
    let version = crate::greet(&mut stream, newest).map_err(ConnectError::Greet)?;
    Ok((stream, version))
}

/// The server binary: beside this one, where builds and installs put it, else on the
/// path.
fn server_program() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.parent()?.join(SERVER)))
        .filter(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from(SERVER))
}

fn connect_or_start(path: &Path) -> Result<UnixStream, ConnectError> {
    let refused = match UnixStream::connect(path) {
        Ok(stream) => return Ok(stream),
        Err(error) => error,
    };
    if !matches!(
        refused.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    ) {
        return Err(ConnectError::Connect {
            path: path.to_owned(),
            source: refused,
        });
    }
    let program = server_program();
    let mut server = process::Command::new(&program)
        .arg("--socket")
        .arg(path)
        .stdin(process::Stdio::null())
        .stdout(process::Stdio::null())
        // Out of the window's process group, so that a signal to it leaves the terminals.
        .process_group(0)
        .spawn()
        .map_err(|source| ConnectError::Start {
            program: program.clone(),
            source,
        })?;
    let started = Instant::now();
    let stream = loop {
        match UnixStream::connect(path) {
            Ok(stream) => break Ok(stream),
            Err(_) if started.elapsed() < Duration::from_secs(5) => {}
            Err(source) => {
                break Err(ConnectError::Connect {
                    path: path.to_owned(),
                    source,
                });
            }
        }
        if let Ok(Some(status)) = server.try_wait() {
            break Err(ConnectError::Ended { program, status });
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    // Reaped when it ends, so that it does not linger as a zombie of this process.
    let _ = std::thread::Builder::new()
        .name("server reaper".to_owned())
        .spawn(move || server.wait());
    stream
}
