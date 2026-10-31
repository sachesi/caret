//! A program in a pseudoterminal of its own: the controlling side for the terminal to read
//! and write, the process, and a descriptor that tells when it exits.

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::OnceLock;
use std::time::Duration;

use caret_protocol::Size;

/// The limit on open files the server was started with, once it raised its own.
static STARTED_WITH: OnceLock<libc::rlimit> = OnceLock::new();

/// Lets the server open as many files as it may. Each terminal takes five descriptors and
/// each client three, so the usual limit of 1024 is reached at about 200 panes. Programs
/// still start with the limit they would have had, as some cannot take a larger one.
#[expect(unsafe_code, reason = "the limits are only reached through libc")]
pub fn raise_descriptor_limit() {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: the pointer is to a live rlimit, which the call fills in.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut limit) } != 0 {
        return;
    }
    let started_with = limit;
    limit.rlim_cur = limit.rlim_max;
    // SAFETY: the pointer is to a live rlimit, which the call reads.
    if unsafe { libc::setrlimit(libc::RLIMIT_NOFILE, &raw const limit) } == 0 {
        let _ = STARTED_WITH.set(started_with);
    }
}

fn winsize(size: Size) -> libc::winsize {
    libc::winsize {
        ws_row: size.lines,
        ws_col: size.columns,
        ws_xpixel: size.columns.saturating_mul(size.cell_width),
        ws_ypixel: size.lines.saturating_mul(size.cell_height),
    }
}

pub struct Pty {
    /// The controlling side, which reads what the program writes and writes what it
    /// reads; it does not block.
    pub controller: File,
    child: Child,
    /// Readable once the program has exited.
    exited: OwnedFd,
}

fn check(status: libc::c_int) -> io::Result<libc::c_int> {
    if status == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(status)
    }
}

#[expect(unsafe_code, reason = "fcntl through libc")]
fn add_flags(
    fd: BorrowedFd<'_>,
    get: libc::c_int,
    set: libc::c_int,
    flags: libc::c_int,
) -> io::Result<()> {
    // SAFETY: fcntl with these commands only reads and sets the descriptor's flags.
    unsafe {
        let current = check(libc::fcntl(fd.as_raw_fd(), get))?;
        check(libc::fcntl(fd.as_raw_fd(), set, current | flags))?;
    }
    Ok(())
}

impl Pty {
    /// Starts `program` with the other side of a new pseudoterminal `size` large as its
    /// controlling terminal, in a session of its own.
    #[expect(
        unsafe_code,
        reason = "the pseudoterminal, termios and the child's setup through libc"
    )]
    pub fn spawn(mut program: Command, size: Size) -> io::Result<Self> {
        // Close-on-exec from the moment they are open: other terminals' threads start
        // programs at the same time, and a program inheriting the controlling side would
        // keep this terminal's from ever being told it hung up.
        // SAFETY: posix_openpt takes flags and returns a new descriptor.
        let controller =
            check(unsafe { libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC) })?;
        // SAFETY: posix_openpt opened it and nothing else owns it.
        let controller = unsafe { OwnedFd::from_raw_fd(controller) };
        // SAFETY: both take the open descriptor; TIOCGPTPEER opens the other side, and
        // returns a new descriptor.
        let terminal = unsafe {
            check(libc::unlockpt(controller.as_raw_fd()))?;
            check(libc::ioctl(
                controller.as_raw_fd(),
                libc::TIOCGPTPEER,
                libc::O_RDWR | libc::O_NOCTTY | libc::O_CLOEXEC,
            ))?
        };
        // SAFETY: the kernel opened it and nothing else owns it.
        let terminal = unsafe { OwnedFd::from_raw_fd(terminal) };
        let winsize = winsize(size);
        // SAFETY: TIOCSWINSZ reads a winsize, which outlives the call.
        check(unsafe {
            libc::ioctl(controller.as_raw_fd(), libc::TIOCSWINSZ, &raw const winsize)
        })?;

        // Input is UTF-8, so that erasing a character takes all its bytes.
        // SAFETY: an all-zero termios is a valid value for tcgetattr to fill in.
        let mut termios: libc::termios = unsafe { std::mem::zeroed() };
        // SAFETY: the descriptor is open and the termios pointer is to a live value.
        unsafe {
            check(libc::tcgetattr(terminal.as_raw_fd(), &raw mut termios))?;
            termios.c_iflag |= libc::IUTF8;
            check(libc::tcsetattr(
                terminal.as_raw_fd(),
                libc::TCSANOW,
                &raw const termios,
            ))?;
        }

        program
            .stdin(Stdio::from(terminal.try_clone()?))
            .stdout(Stdio::from(terminal.try_clone()?))
            .stderr(Stdio::from(terminal));
        let started_with = STARTED_WITH.get().copied();
        // SAFETY: between fork and exec the closure calls only setsid, ioctl, signal and
        // setrlimit, which are async-signal-safe, and allocates nothing.
        unsafe {
            program.pre_exec(move || {
                check(libc::setsid())?;
                check(libc::ioctl(0, libc::TIOCSCTTY, 0))?;
                // A shell starting the server in the background leaves SIGINT and SIGQUIT
                // ignored, and nohup SIGHUP, and an exec keeps that: Ctrl+C would stop
                // nothing in the session, and hanging up would end nothing.
                for signal in [libc::SIGINT, libc::SIGQUIT, libc::SIGHUP] {
                    libc::signal(signal, libc::SIG_DFL);
                }
                if let Some(limit) = &started_with {
                    check(libc::setrlimit(libc::RLIMIT_NOFILE, limit))?;
                }
                Ok(())
            });
        }
        let child = program.spawn()?;
        add_flags(
            controller.as_fd(),
            libc::F_GETFL,
            libc::F_SETFL,
            libc::O_NONBLOCK,
        )?;

        let pid = child.id().cast_signed();
        // SAFETY: pidfd_open takes a process id and flags, and returns a new descriptor.
        let exited = check(unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) } as libc::c_int)?;
        // SAFETY: pidfd_open opened it and nothing else owns it.
        let exited = unsafe { OwnedFd::from_raw_fd(exited) };
        Ok(Self {
            controller: File::from(controller),
            child,
            exited,
        })
    }

    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Readable once the program has exited.
    pub fn exited(&self) -> BorrowedFd<'_> {
        self.exited.as_fd()
    }

    #[expect(unsafe_code, reason = "TIOCSWINSZ through libc")]
    pub fn resize(&self, size: Size) {
        let winsize = winsize(size);
        // SAFETY: TIOCSWINSZ reads a winsize, which outlives the call. The program learns
        // of the size from SIGWINCH, which the kernel sends.
        let status = unsafe {
            libc::ioctl(
                self.controller.as_raw_fd(),
                libc::TIOCSWINSZ,
                &raw const winsize,
            )
        };
        if status == -1 {
            tracing::warn!("resizing a terminal: {}", io::Error::last_os_error());
        }
    }

    /// The exit status, once the program has exited.
    pub fn status(&mut self) -> Option<ExitStatus> {
        self.child.try_wait().ok().flatten()
    }

    /// Hangs up on the program, and kills it if it has not ended after `patience`.
    #[expect(unsafe_code, reason = "kill through libc")]
    pub fn hang_up(mut self, patience: Duration) -> ExitStatus {
        let pid = self.child.id().cast_signed();
        // SAFETY: kill only sends a signal; the process is this one's child and has not
        // been waited for, so its id is still its own.
        unsafe { libc::kill(pid, libc::SIGHUP) };
        drop(self.controller);
        if !wait_readable(&[self.exited.as_fd()], Some(patience)).is_ok_and(|ready| ready[0]) {
            let _ = self.child.kill();
        }
        self.child
            .wait()
            .unwrap_or_else(|_| ExitStatus::from_raw(libc::SIGKILL))
    }
}

/// Waits until one of `fds` is readable, or `timeout` passes, and says which are.
pub fn wait_readable(fds: &[BorrowedFd<'_>], timeout: Option<Duration>) -> io::Result<Vec<bool>> {
    wait(fds, &[], timeout).map(|(readable, _)| readable)
}

/// Waits until one of `readers` is readable or one of `writers` writable, or `timeout`
/// passes, and says which are.
#[expect(unsafe_code, reason = "poll through libc")]
pub fn wait(
    readers: &[BorrowedFd<'_>],
    writers: &[BorrowedFd<'_>],
    timeout: Option<Duration>,
) -> io::Result<(Vec<bool>, Vec<bool>)> {
    let mut polled: Vec<libc::pollfd> = readers
        .iter()
        .map(|fd| (fd, libc::POLLIN))
        .chain(writers.iter().map(|fd| (fd, libc::POLLOUT)))
        .map(|(fd, events)| libc::pollfd {
            fd: fd.as_raw_fd(),
            events,
            revents: 0,
        })
        .collect();
    let timeout = timeout.map_or(-1, |timeout| {
        libc::c_int::try_from(timeout.as_millis().max(u128::from(!timeout.is_zero())))
            .unwrap_or(libc::c_int::MAX)
    });
    loop {
        // SAFETY: the pointer and length describe `polled`, which outlives the call.
        let status =
            unsafe { libc::poll(polled.as_mut_ptr(), polled.len() as libc::nfds_t, timeout) };
        match check(status) {
            Ok(_) => break,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    // A hung-up or failed descriptor counts as ready: reading it says why.
    let ready = |pollfd: &libc::pollfd| pollfd.revents != 0;
    let (readable, writable) = polled.split_at(readers.len());
    Ok((
        readable.iter().map(ready).collect(),
        writable.iter().map(ready).collect(),
    ))
}

/// The process group in the foreground of the terminal `controller` controls.
#[expect(unsafe_code, reason = "tcgetpgrp through libc")]
pub fn foreground(controller: BorrowedFd<'_>) -> Option<u32> {
    // SAFETY: tcgetpgrp only reads the descriptor, which the caller keeps open.
    let group = unsafe { libc::tcgetpgrp(controller.as_raw_fd()) };
    (group > 0).then(|| group.cast_unsigned())
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};

    use super::*;

    fn size() -> Size {
        Size {
            columns: 80,
            lines: 24,
            cell_width: 8,
            cell_height: 16,
        }
    }

    /// What `pty`'s program writes before it exits.
    fn output(pty: &mut Pty) -> String {
        let mut output = Vec::new();
        let mut buffer = [0; 4096];
        loop {
            let ready = wait_readable(&[pty.controller.as_fd()], Some(Duration::from_secs(5)));
            assert_eq!(ready.expect("poll works"), [true]);
            match pty.controller.read(&mut buffer) {
                Ok(count) if count > 0 => output.extend_from_slice(&buffer[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                _ => return String::from_utf8_lossy(&output).into_owned(),
            }
        }
    }

    #[test]
    fn a_program_starts_with_the_limit_the_server_was_started_with() {
        let mut before = libc::rlimit {
            rlim_cur: 0,
            rlim_max: 0,
        };
        #[expect(unsafe_code, reason = "getrlimit through libc")]
        // SAFETY: the pointer is to a live rlimit, which the call fills in.
        unsafe {
            libc::getrlimit(libc::RLIMIT_NOFILE, &raw mut before);
        }
        raise_descriptor_limit();
        let mut shell = Command::new("sh");
        shell.args(["-c", "ulimit -n"]);
        let mut pty = Pty::spawn(shell, size()).expect("sh starts");
        let told = output(&mut pty);
        assert_eq!(told.trim(), before.rlim_cur.to_string());
    }

    #[test]
    fn a_program_has_a_named_terminal_of_the_right_size_that_it_controls() {
        let mut shell = Command::new("sh");
        shell.args(["-c", "tty; stty size; test -r /dev/tty && echo controlling"]);
        let mut pty = Pty::spawn(shell, size()).expect("sh starts");
        let told = output(&mut pty);
        assert!(told.contains("/dev/pts/"), "{told}");
        assert!(told.contains("24 80"), "{told}");
        assert!(told.contains("controlling"), "{told}");
    }

    #[test]
    #[expect(unsafe_code, reason = "signal through libc")]
    fn ctrl_c_stops_a_program_when_the_server_ignores_the_signal() {
        // SAFETY: SIG_IGN holds no handler, and nothing else in these tests relies on SIGINT.
        unsafe { libc::signal(libc::SIGINT, libc::SIG_IGN) };
        let size = Size {
            columns: 80,
            lines: 24,
            cell_width: 8,
            cell_height: 16,
        };
        let mut pty = Pty::spawn(Command::new("cat"), size).expect("cat starts");
        // SAFETY: SIG_DFL holds no handler.
        unsafe { libc::signal(libc::SIGINT, libc::SIG_DFL) };
        pty.controller
            .write_all(b"\x03")
            .expect("the terminal takes input");
        let ready = wait_readable(&[pty.exited()], Some(Duration::from_secs(5)));
        assert_eq!(ready.expect("poll works"), [true]);
        let status = pty.status().expect("cat has exited");
        assert_eq!(status.signal(), Some(libc::SIGINT));
    }
}
