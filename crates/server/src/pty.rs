//! A program in a pseudoterminal of its own: the controlling side for the terminal to read
//! and write, the process, and a descriptor that tells when it exits.

use std::fs::File;
use std::io;
use std::os::fd::{AsFd, AsRawFd, BorrowedFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::process::{CommandExt, ExitStatusExt};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::Duration;

use caret_protocol::Size;

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
        reason = "openpty, termios and the child's setup through libc"
    )]
    pub fn spawn(mut program: Command, size: Size) -> io::Result<Self> {
        let (mut controller, mut terminal): (RawFd, RawFd) = (-1, -1);
        let winsize = winsize(size);
        // SAFETY: the descriptors are written to live integers, the name may be null, and
        // the termios and size pointers are to values that outlive the call.
        check(unsafe {
            libc::openpty(
                &raw mut controller,
                &raw mut terminal,
                std::ptr::null_mut(),
                std::ptr::null(),
                &raw const winsize,
            )
        })?;
        // SAFETY: openpty opened both and nothing else owns them.
        let (controller, terminal) = unsafe {
            (
                OwnedFd::from_raw_fd(controller),
                OwnedFd::from_raw_fd(terminal),
            )
        };
        // Neither is passed on to the program but through its standard streams.
        add_flags(
            controller.as_fd(),
            libc::F_GETFD,
            libc::F_SETFD,
            libc::FD_CLOEXEC,
        )?;
        add_flags(
            terminal.as_fd(),
            libc::F_GETFD,
            libc::F_SETFD,
            libc::FD_CLOEXEC,
        )?;

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
        // SAFETY: between fork and exec the closure calls only setsid and ioctl, which are
        // async-signal-safe, and allocates nothing.
        unsafe {
            program.pre_exec(|| {
                check(libc::setsid())?;
                check(libc::ioctl(0, libc::TIOCSCTTY, 0))?;
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
