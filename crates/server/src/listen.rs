//! The server's socket, handed over by systemd or made in the user's runtime directory,
//! and a reader and a writer thread for each client that connects.

#![expect(
    unsafe_code,
    reason = "the descriptor systemd passes and the peer's credentials are only reached through libc"
)]

use std::fs;
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;

use caret_protocol::Request;

use crate::server::{ClientId, Input};

/// The first descriptor systemd passes.
const LISTEN_FDS_START: RawFd = 3;

/// The socket systemd activated the server with, if it did. Called before any thread
/// starts: it takes the variables that say so out of the environment, so that no program
/// started later reads them as its own.
///
/// # Safety
///
/// No other thread is running.
pub unsafe fn activated() -> Option<UnixListener> {
    let for_us = std::env::var("LISTEN_PID")
        .ok()
        .and_then(|pid| pid.parse::<u32>().ok())
        == Some(std::process::id());
    let count = std::env::var("LISTEN_FDS")
        .ok()
        .and_then(|count| count.parse::<i32>().ok())
        .unwrap_or(0);
    for name in ["LISTEN_PID", "LISTEN_FDS", "LISTEN_FDNAMES"] {
        // SAFETY: the caller has started no thread that could be reading the environment.
        unsafe { std::env::remove_var(name) };
    }
    if !for_us || count < 1 {
        return None;
    }
    // SAFETY: fcntl on a descriptor number only fails for one that is not open.
    let flags = unsafe { libc::fcntl(LISTEN_FDS_START, libc::F_GETFD) };
    if flags < 0 {
        return None;
    }
    // Passed without close-on-exec, which every program a terminal starts would inherit.
    // SAFETY: as above, on the descriptor just found open.
    unsafe { libc::fcntl(LISTEN_FDS_START, libc::F_SETFD, flags | libc::FD_CLOEXEC) };
    // SAFETY: systemd passed a listening Unix socket here, which nothing else owns.
    Some(unsafe { UnixListener::from_raw_fd(LISTEN_FDS_START) })
}

/// A socket at `path`, in a directory only the user can enter.
///
/// # Errors
///
/// When the directory or the socket cannot be made, or another server listens there.
pub fn bind(path: &Path) -> io::Result<UnixListener> {
    if let Some(directory) = path.parent() {
        fs::create_dir_all(directory)?;
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700))?;
    }
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            return Err(io::Error::new(
                io::ErrorKind::AddrInUse,
                format!("a server already listens at {}", path.display()),
            ));
        }
        // Left behind by a server that ended.
        fs::remove_file(path)?;
    }
    UnixListener::bind(path)
}

/// The user a connection comes from.
fn peer_uid(stream: &UnixStream) -> io::Result<u32> {
    let mut credentials = libc::ucred {
        pid: 0,
        uid: 0,
        gid: 0,
    };
    let mut length = size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: the buffer is a ucred, and `length` says how big it is.
    let status = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            (&raw mut credentials).cast(),
            &raw mut length,
        )
    };
    if status == 0 {
        Ok(credentials.uid)
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Takes connections until the loop ends, each on threads of its own.
pub fn accept(listener: &UnixListener, events: &mpsc::Sender<Input>) {
    static CLIENTS: AtomicU64 = AtomicU64::new(1);
    // SAFETY: getuid cannot fail.
    let own = unsafe { libc::getuid() };
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                tracing::warn!("accepting a client: {error}");
                continue;
            }
        };
        match peer_uid(&stream) {
            Ok(uid) if uid == own => {}
            Ok(uid) => {
                tracing::warn!("refusing a client of user {uid}");
                continue;
            }
            Err(error) => {
                tracing::warn!("reading a client's credentials: {error}");
                continue;
            }
        }
        let client = ClientId(CLIENTS.fetch_add(1, Ordering::Relaxed));
        let events = events.clone();
        let spawned = std::thread::Builder::new()
            .name(format!("client {}", client.0))
            .spawn(move || serve(client, stream, &events));
        if let Err(error) = spawned {
            tracing::warn!("starting a client's thread: {error}");
        }
    }
}

/// Reads one client's requests into the loop, and starts the thread that writes what the
/// loop sends it.
fn serve(client: ClientId, mut stream: UnixStream, events: &mpsc::Sender<Input>) {
    match caret_protocol::welcome(&mut stream) {
        Ok(Some(_)) => {}
        Ok(None) => return,
        Err(error) => {
            tracing::warn!("greeting a client: {error}");
            return;
        }
    }
    let (writer, written) = mpsc::channel::<Vec<u8>>();
    let mut out = match stream.try_clone() {
        Ok(out) => out,
        Err(error) => {
            tracing::warn!("opening a client's socket for writing: {error}");
            return;
        }
    };
    let spawned = std::thread::Builder::new()
        .name(format!("client {} writer", client.0))
        .spawn(move || {
            // Ends when the loop forgets the client, or the client stops reading.
            for bytes in written {
                if out.write_all(&bytes).is_err() {
                    break;
                }
            }
            let _ = out.shutdown(std::net::Shutdown::Both);
        });
    if let Err(error) = spawned {
        tracing::warn!("starting a client's writer: {error}");
        return;
    }
    if events.send(Input::Connected { client, writer }).is_err() {
        return;
    }
    loop {
        match caret_protocol::receive::<Request>(&mut stream) {
            Ok(Some(request)) => {
                if events.send(Input::Request(client, request)).is_err() {
                    return;
                }
            }
            Ok(None) => break,
            Err(error) => {
                tracing::warn!("reading from client {}: {error}", client.0);
                break;
            }
        }
    }
    let _ = events.send(Input::Disconnected(client));
}
