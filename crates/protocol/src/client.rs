//! A client's end of the socket: finding the server, starting one when none listens,
//! and the handshake.

use std::fs::File;
use std::io;
use std::os::fd::AsFd;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{self, ExitStatus};
use std::time::{Duration, Instant};

const SERVER: &str = "caret-server";

/// How long a server has to answer, started by systemd for its socket or running.
const PATIENCE: Duration = Duration::from_secs(10);

/// How many times a connection is tried when the server hangs up during the handshake.
const CONNECTIONS: u32 = 3;

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
    connect_at(&path, newest)
}

fn connect_at(path: &Path, newest: u16) -> Result<(UnixStream, u16), ConnectError> {
    let mut attempts = 1;
    loop {
        let mut stream = connect_or_start(path)?;
        match greeted(&mut stream, newest) {
            Ok(version) => return Ok((stream, version)),
            // A server ends with its last session, and may have taken this connection just
            // before: the next attempt starts another.
            Err(error) if attempts < CONNECTIONS && hung_up(&error) => attempts += 1,
            Err(error) => return Err(ConnectError::Greet(error)),
        }
    }
}

fn hung_up(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
    )
}

/// The handshake, within [`PATIENCE`]: a stopped server would otherwise hold a window that
/// is starting forever.
fn greeted(stream: &mut UnixStream, newest: u16) -> io::Result<u16> {
    stream.set_read_timeout(Some(PATIENCE))?;
    let version = crate::greet(stream, newest)?;
    stream.set_read_timeout(None)?;
    Ok(version)
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

/// Where a server started from here writes its log: where this does, unless that is a pipe.
/// The server outlives this client, and whoever reads the pipe, such as `$(caret ... 2>&1)`,
/// would wait for it to end.
fn server_stderr() -> process::Stdio {
    let writes_to_pipe = io::stderr()
        .as_fd()
        .try_clone_to_owned()
        .map(File::from)
        .and_then(|stderr| stderr.metadata())
        .is_ok_and(|stderr| stderr.file_type().is_fifo());
    if writes_to_pipe {
        process::Stdio::null()
    } else {
        process::Stdio::inherit()
    }
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
        .stderr(server_stderr())
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

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixListener;

    use super::*;

    #[test]
    fn a_server_ending_during_the_handshake_is_tried_again() {
        let directory = std::env::temp_dir().join(format!("caret-connect-{}", process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("server");
        let listener = UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || {
            // The first connection is let go of, as by a server that is ending.
            drop(listener.accept().unwrap());
            let (mut stream, _) = listener.accept().unwrap();
            crate::welcome(&mut stream).unwrap()
        });
        let (_, version) = connect_at(&path, crate::NEWEST).unwrap();
        assert_eq!(version, crate::NEWEST);
        assert_eq!(server.join().unwrap(), Some(crate::NEWEST));
        std::fs::remove_dir_all(&directory).unwrap();
    }
}
