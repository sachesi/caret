//! One program in a pseudoterminal: alacritty_terminal's parser and grid, fed on a thread
//! of its own, and the events it raises passed to the main context.

use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::CStr;
use std::fs::File;
use std::io;
use std::ops::DerefMut;
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{self, Term};
use alacritty_terminal::tty;

use crate::config;
use crate::shell::{FilteredPty, Report};

/// What the widget hears from the terminal. A wakeup is sent once until the widget has
/// taken it, however much output arrives meanwhile, so the channel holds at most one of
/// them and a few rare events besides.
#[derive(Clone)]
pub struct Listener {
    events: async_channel::Sender<Event>,
    woken: Arc<AtomicBool>,
}

impl EventListener for Listener {
    fn send_event(&self, event: Event) {
        match event {
            Event::MouseCursorDirty | Event::CursorBlinkingChange => return,
            Event::Wakeup if self.woken.swap(true, Ordering::SeqCst) => return,
            _ => {}
        }
        // Unbounded: the sender is the parser, which holds the terminal's lock, and a
        // full channel would stall it while the main context waits for the same lock.
        let _ = self.events.try_send(event);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GridSize {
    pub columns: usize,
    pub lines: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.lines
    }

    fn screen_lines(&self) -> usize {
        self.lines
    }

    fn columns(&self) -> usize {
        self.columns
    }
}

pub struct Session {
    pub term: Arc<FairMutex<Term<Listener>>>,
    sender: EventLoopSender,
    woken: Arc<AtomicBool>,
    child: u32,
    /// A second handle on the controlling side of the pseudoterminal, to ask which process
    /// group is in the foreground.
    controller: File,
}

/// What to run, where, and with what environment.
#[derive(Debug, Clone, Default)]
pub struct Command {
    /// A program and its arguments; the user's shell when empty.
    pub argv: Vec<String>,
    pub directory: Option<PathBuf>,
    /// The environment of the command line that asked for the program, which may not be
    /// the one Tangent was started from; Tangent's own when empty.
    pub env: HashMap<String, String>,
}

/// Variables that describe the terminal a command line was typed in, or a multiplexer
/// running in it, and would mislead programs in this one; and those alacritty_terminal
/// sets for its own windows.
const FOREIGN: [&str; 26] = [
    "ALACRITTY_LOG",
    "ALACRITTY_SOCKET",
    "ALACRITTY_WINDOW_ID",
    "COLUMNS",
    "DESKTOP_STARTUP_ID",
    "GNOME_TERMINAL_SCREEN",
    "GNOME_TERMINAL_SERVICE",
    "KITTY_PID",
    "KITTY_PUBLIC_KEY",
    "KITTY_WINDOW_ID",
    "KONSOLE_DBUS_SERVICE",
    "KONSOLE_DBUS_SESSION",
    "KONSOLE_DBUS_WINDOW",
    "KONSOLE_VERSION",
    "LINES",
    "OLDPWD",
    "PTYXIS_VERSION",
    "PWD",
    "STY",
    "TMUX",
    "TMUX_PANE",
    "VTE_VERSION",
    "WEZTERM_PANE",
    "WEZTERM_UNIX_SOCKET",
    "WINDOWID",
    "XDG_ACTIVATION_TOKEN",
];

/// The account's entry in the password database: its name, home and shell.
fn account() -> Option<(String, String, String)> {
    let mut buffer = vec![0; 16 * 1024];
    // SAFETY: an all-zero passwd is a valid value for getpwuid_r to fill in.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found = std::ptr::null_mut();
    // SAFETY: every pointer is to memory of the given size that outlives the call.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &mut found,
        )
    };
    if status != 0 || found.is_null() {
        return None;
    }
    let field = |pointer: *const libc::c_char| {
        // SAFETY: getpwuid_r left the entry's strings NUL-terminated in `buffer`, which
        // is still alive.
        (!pointer.is_null())
            .then(|| {
                unsafe { CStr::from_ptr(pointer) }
                    .to_str()
                    .ok()
                    .map(str::to_owned)
            })
            .flatten()
    };
    Some((
        field(entry.pw_name)?,
        field(entry.pw_dir)?,
        field(entry.pw_shell)?,
    ))
}

impl Command {
    /// The environment the program gets, whole.
    fn environment(&self) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = if self.env.is_empty() {
            std::env::vars_os()
                .filter_map(|(name, value)| {
                    Some((name.into_string().ok()?, value.into_string().ok()?))
                })
                .collect()
        } else {
            self.env.clone()
        };
        env.retain(|name, _| !FOREIGN.contains(&name.as_str()));
        if env.remove(crate::CHOSE_RENDERER).is_some() {
            env.remove("GSK_RENDERER");
        }
        if !["USER", "HOME", "SHELL"]
            .iter()
            .all(|name| env.contains_key(*name))
            && let Some((user, home, shell)) = account()
        {
            env.entry("USER".to_owned()).or_insert(user);
            env.entry("HOME".to_owned()).or_insert(home);
            env.entry("SHELL".to_owned()).or_insert(shell);
        }
        env.extend(
            [
                ("TERM", "xterm-256color"),
                ("COLORTERM", "truecolor"),
                ("TERM_PROGRAM", "tangent"),
                ("TERM_PROGRAM_VERSION", config::VERSION),
            ]
            .map(|(name, value)| (name.to_owned(), value.to_owned())),
        );
        env
    }

    /// The program to run: the one named, or the user's shell.
    pub fn program(&self) -> String {
        let shell = || {
            self.env
                .get("SHELL")
                .cloned()
                .or_else(|| std::env::var("SHELL").ok())
                .or_else(|| account().map(|(_, _, shell)| shell))
        };
        self.argv
            .first()
            .cloned()
            .or_else(shell)
            .unwrap_or_else(|| "/bin/sh".to_owned())
    }

    /// Whether `program` can be run with `env`, found the way `env` finds it; the error
    /// is why not.
    fn check(&self, program: &str, env: &HashMap<String, String>) -> io::Result<()> {
        let runnable = |path: &Path| {
            path.metadata().is_ok_and(|metadata| {
                metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
            })
        };
        let found = if program.contains('/') {
            let path = Path::new(program);
            match &self.directory {
                Some(directory) if path.is_relative() => runnable(&directory.join(path)),
                _ => runnable(path),
            }
        } else {
            env.get("PATH")
                .map_or("/usr/local/bin:/usr/bin:/bin", String::as_str)
                .split(':')
                .filter(|directory| !directory.is_empty())
                .any(|directory| runnable(&Path::new(directory).join(program)))
        };
        if found {
            Ok(())
        } else {
            Err(io::Error::from_raw_os_error(libc::ENOENT))
        }
    }
}

impl Session {
    pub fn spawn(
        command: &Command,
        size: WindowSize,
        config: term::Config,
    ) -> io::Result<(
        Self,
        async_channel::Receiver<Event>,
        async_channel::Receiver<Report>,
    )> {
        let (events, received) = async_channel::unbounded();
        let woken = Arc::new(AtomicBool::new(false));
        let listener = Listener {
            events,
            woken: woken.clone(),
        };

        // alacritty_terminal can add variables but not take any away, so the program is
        // started by `env -i` with the environment it is meant to have, whole.
        let env = command.environment();
        let program = command.program();
        command.check(&program, &env)?;
        let mut args = vec!["-i".to_owned(), "--".to_owned()];
        args.extend(env.iter().map(|(name, value)| format!("{name}={value}")));
        args.push(program);
        args.extend(command.argv.iter().skip(1).cloned());
        let options = tty::Options {
            shell: Some(tty::Shell::new("env".to_owned(), args)),
            working_directory: command.directory.clone(),
            drain_on_exit: true,
            env: HashMap::new(),
        };
        let pty = tty::new(&options, size, 0)?;
        let child = pty.child().id();
        let controller = pty.file().try_clone()?;
        let (reports, reported) = async_channel::unbounded();
        let pty = FilteredPty::new(pty, move |report| {
            let _ = reports.try_send(report);
        })?;

        let grid = GridSize {
            columns: usize::from(size.num_cols),
            lines: usize::from(size.num_lines),
        };
        let term = Arc::new(FairMutex::new(Term::new(config, &grid, listener.clone())));
        let event_loop = EventLoop::new(term.clone(), listener, pty, true, false)?;
        let sender = event_loop.channel();
        // Detached: the thread ends when the program does or on `Msg::Shutdown`, and takes
        // the pseudoterminal with it.
        drop(event_loop.spawn());

        Ok((
            Self {
                term,
                sender,
                woken,
                child,
                controller,
            },
            received,
            reported,
        ))
    }

    /// Lets the next output wake the widget again; called when it takes a wakeup.
    pub fn rearm(&self) {
        self.woken.store(false, Ordering::SeqCst);
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let bytes = bytes.into();
        if !bytes.is_empty() {
            let _ = self.sender.send(Msg::Input(bytes));
        }
    }

    /// The terminal, for the main context. It takes the lock without the lease a search
    /// holds from start to end, so it waits for one step of a search at most, as for one
    /// read of the program's output.
    pub fn lock(&self) -> impl DerefMut<Target = Term<Listener>> + '_ {
        self.term.lock_unfair()
    }

    pub fn resize(&self, size: WindowSize) {
        self.lock().resize(GridSize {
            columns: usize::from(size.num_cols),
            lines: usize::from(size.num_lines),
        });
        let _ = self.sender.send(Msg::Resize(size));
    }

    /// The name of the program in the foreground when it is not the one the session
    /// started, the shell usually: something that closing would interrupt.
    pub fn busy(&self) -> Option<String> {
        // SAFETY: tcgetpgrp only reads the descriptor, which `controller` keeps open.
        let group = unsafe { libc::tcgetpgrp(self.controller.as_raw_fd()) };
        if group <= 0 || group.cast_unsigned() == self.child {
            return None;
        }
        let name = std::fs::read_to_string(format!("/proc/{group}/comm")).unwrap_or_default();
        Some(name.trim_end().to_owned())
    }

    /// Where the started program is now, for a new tab to start in the same place.
    pub fn directory(&self) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{}/cwd", self.child)).ok()
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.sender.send(Msg::Shutdown);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(env: &[(&str, &str)]) -> Command {
        Command {
            env: env
                .iter()
                .map(|&(name, value)| (name.to_owned(), value.to_owned()))
                .collect(),
            ..Command::default()
        }
    }

    #[test]
    fn the_environment_is_the_command_lines_without_other_terminals() {
        let command = command(&[
            ("EDITOR", "vi"),
            ("TMUX", "/tmp/tmux-1000/default,1,0"),
            ("VTE_VERSION", "7800"),
            ("TERM", "screen"),
            ("USER", "someone"),
            ("HOME", "/home/someone"),
            ("SHELL", "/bin/zsh"),
        ]);
        let env = command.environment();
        assert_eq!(env.get("EDITOR").map(String::as_str), Some("vi"));
        assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-256color"));
        assert!(!env.contains_key("TMUX"));
        assert!(!env.contains_key("VTE_VERSION"));
        assert_eq!(command.program(), "/bin/zsh");
    }

    #[test]
    fn the_renderer_tangent_chose_is_not_passed_on() {
        let chosen = command(&[("GSK_RENDERER", "gl"), (crate::CHOSE_RENDERER, "1")]);
        let env = chosen.environment();
        assert!(!env.contains_key("GSK_RENDERER"));
        assert!(!env.contains_key(crate::CHOSE_RENDERER));
        let own = command(&[("GSK_RENDERER", "vulkan")]);
        assert_eq!(
            own.environment().get("GSK_RENDERER").map(String::as_str),
            Some("vulkan")
        );
    }

    #[test]
    fn a_program_is_looked_for_in_the_path_it_will_run_with() {
        let command = Command::default();
        let path = HashMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
        assert!(command.check("sh", &path).is_ok());
        assert!(command.check("/bin/sh", &HashMap::new()).is_ok());
        assert!(command.check("no-such-program", &path).is_err());
        let nowhere = HashMap::from([("PATH".to_owned(), "/nonexistent".to_owned())]);
        assert!(command.check("sh", &nowhere).is_err());
    }
}
