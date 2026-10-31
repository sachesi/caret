//! The server's socket, handed over by systemd or made in the user's runtime directory,
//! and a reader and a writer thread for each client that connects.

#![expect(
    unsafe_code,
    reason = "the descriptor systemd passes and the peer's credentials are only reached through libc"
)]

use std::fs::{self, File};
use std::io::{self, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::Path;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use caret_protocol::{LONGEST, Request};

use crate::server::{ClientId, Input};

/// The first descriptor systemd passes.
const LISTEN_FDS_START: RawFd = 3;

/// Bytes queued for a client before it is hung up on: room for two of the longest
/// messages.
const BACKLOG: usize = 2 * LONGEST;

/// How long to wait after failing to take a connection. It stays waiting, so with the
/// server out of descriptors trying again at once would spin and flood the log.
const ACCEPT_PAUSE: Duration = Duration::from_millis(200);

/// What a queued message costs beyond its bytes, counted against [`BACKLOG`] so that
/// many short ones are not more than it allows: its allocation and its place in the
/// queue.
const OVERHEAD: usize = 64;

/// Messages on their way to a client, queued for its writer thread. A client that falls
/// [`BACKLOG`] bytes behind, stopped or stuck while its programs go on, is hung up on,
/// before what it does not read takes the server's memory.
#[derive(Clone)]
pub struct Outbox {
    queue: mpsc::Sender<Vec<u8>>,
    queued: Arc<AtomicUsize>,
    socket: Arc<UnixStream>,
}

/// The writer's end of an [`Outbox`]: the messages, and the count of bytes queued it
/// takes them off.
pub struct Queued {
    pub messages: mpsc::Receiver<Vec<u8>>,
    queued: Arc<AtomicUsize>,
}

impl Outbox {
    /// An outbox for the client on `socket`, which it shuts to hang up.
    pub fn new(socket: UnixStream) -> (Self, Queued) {
        let (queue, messages) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let outbox = Self {
            queue,
            queued: queued.clone(),
            socket: Arc::new(socket),
        };
        (outbox, Queued { messages, queued })
    }

    /// Queues `bytes`; nothing happens once the client is gone, which the loop hears of
    /// from its reader.
    pub fn send(&self, bytes: Vec<u8>) {
        let length = bytes.len() + OVERHEAD;
        let before = self.queued.fetch_add(length, Ordering::Relaxed);
        if before + length > BACKLOG {
            if before <= BACKLOG {
                tracing::warn!("hanging up on a client {before} bytes behind");
                let _ = self.socket.shutdown(std::net::Shutdown::Both);
            }
            return;
        }
        let _ = self.queue.send(bytes);
    }
}

impl Queued {
    /// `bytes` of a message were written to the client.
    fn written(&self, bytes: usize) {
        self.queued.fetch_sub(bytes, Ordering::Relaxed);
    }
}

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
    // Two servers starting at once over a socket left behind would otherwise each remove
    // it, the second removing the one the first has just made and leaving it unreachable.
    let lock = File::create(path.with_extension("lock"))?;
    lock.lock()?;
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
                std::thread::sleep(ACCEPT_PAUSE);
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
    let version = match caret_protocol::welcome(&mut stream) {
        Ok(Some(version)) => version,
        Ok(None) => return,
        Err(error) => {
            tracing::warn!("greeting a client: {error}");
            return;
        }
    };
    let (mut out, hang_up) = match stream
        .try_clone()
        .and_then(|out| Ok((out, stream.try_clone()?)))
    {
        Ok(clones) => clones,
        Err(error) => {
            tracing::warn!("opening a client's socket for writing: {error}");
            return;
        }
    };
    let (writer, queued) = Outbox::new(hang_up);
    let spawned = std::thread::Builder::new()
        .name(format!("client {} writer", client.0))
        .spawn(move || {
            // Ends when the loop forgets the client, or the client stops reading.
            for bytes in &queued.messages {
                if out.write_all(&bytes).is_err() {
                    break;
                }
                queued.written(bytes.len() + OVERHEAD);
            }
            let _ = out.shutdown(std::net::Shutdown::Both);
        });
    if let Err(error) = spawned {
        tracing::warn!("starting a client's writer: {error}");
        return;
    }
    if events
        .send(Input::Connected {
            client,
            version,
            writer,
        })
        .is_err()
    {
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

#[cfg(test)]
mod tests {
    use std::io::Read;

    use super::*;

    #[test]
    fn servers_starting_at_once_over_a_stale_socket_leave_one_listening() {
        let directory = std::env::temp_dir().join(format!("caret-bind-{}", std::process::id()));
        fs::create_dir_all(&directory).expect("a directory");
        let path = directory.join("server");
        let mut started = 0;
        for _ in 0..50 {
            drop(UnixListener::bind(&path).expect("a socket left behind"));
            let barrier = Arc::new(std::sync::Barrier::new(8));
            let starting: Vec<_> = (0..8)
                .map(|_| {
                    let (path, barrier) = (path.clone(), barrier.clone());
                    std::thread::spawn(move || {
                        barrier.wait();
                        bind(&path)
                    })
                })
                .collect();
            let listening: Vec<UnixListener> = starting
                .into_iter()
                .filter_map(|thread| thread.join().expect("binding does not panic").ok())
                .collect();
            // None at times: a program another test is starting has a copy of the socket
            // just dropped until its exec, so that it still seems to listen.
            assert!(listening.len() <= 1, "{} listening", listening.len());
            started += listening.len();
            drop(listening);
            fs::remove_file(&path).expect("the socket");
        }
        assert!(started > 0);
        fs::remove_dir_all(&directory).expect("the directory");
    }

    #[test]
    fn a_client_too_far_behind_is_hung_up_on() {
        let (ours, mut theirs) = UnixStream::pair().expect("a socket pair");
        let (outbox, queued) = Outbox::new(ours);
        outbox.send(vec![1; 16]);
        assert_eq!(queued.messages.try_recv().ok(), Some(vec![1; 16]));
        queued.written(16 + OVERHEAD);
        outbox.send(vec![0; BACKLOG]);
        assert!(queued.messages.try_recv().is_err());
        let mut byte = [0];
        assert_eq!(theirs.read(&mut byte).expect("the socket reads"), 0);
    }
}
