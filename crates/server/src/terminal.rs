//! One program in a pseudoterminal, and the thread that is its terminal: it reads the
//! program's output into the emulator, writes what clients type, and keeps what each
//! client sees of it, sending them frames itself. The server's loop hears only of what
//! concerns the tab: its title, bells, the clipboard, what the shell reports, and the end.

use std::collections::HashMap;
use std::ffi::{CStr, OsStr};
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::AsFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::panic::{self, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::{Arc, OnceLock, mpsc};
use std::time::{Duration, Instant};

use caret_protocol::{
    Answer, Command, Hovered, Message, Palette, Point, Question, Scroll, Select, SelectionKind,
    Size, TerminalId,
};
use caret_term::search::{Progress, Search};
use caret_term::term::{CursorShape, CursorStyle};
use caret_term::view::{Span, Unit, View};
use caret_term::{Report, Term};

use crate::capture::{Sent, capture};
use crate::listen::Outbox;
use crate::palette;
use crate::pty::{self, Pty};
use crate::server::{ClientId, Input, encoded};

/// Bytes read from the program before the clients are sent what they did.
const READ_BUDGET: usize = 1 << 20;

/// How long the program's output is read for before the clients are sent what it did and
/// the keyboard is looked at. What a few bytes cost varies by thousands of times, so bytes
/// alone do not keep Ctrl+C from waiting.
const READ_TIME: Duration = Duration::from_millis(16);

/// Rounds of reading what a program that exited left behind. It is at most what the
/// pseudoterminal holds, unless something it started goes on writing there.
const LEFT_BEHIND: usize = 64;

/// How long a program may hold the screen back while it draws.
const SYNCHRONIZED: Duration = Duration::from_millis(150);

/// Lines a search goes through between reads of the program's output.
const SEARCH_STEP: usize = 2000;

/// How long a program has to end after it is hung up on, before it is killed.
const PATIENCE: Duration = Duration::from_secs(2);

/// The largest terminal there is, whatever size a client asks for: its screens are kept
/// whole in memory, and failing to allocate them would end the server and every program in
/// it. Past what any display shows at a readable size.
const MOST_COLUMNS: u16 = 2048;
const MOST_LINES: u16 = 1024;

/// Bytes waiting for the program past which the emulator's replies are dropped: one that
/// asks and never reads would otherwise fill the server's memory with the answers.
const MOST_BEHIND: usize = 1 << 20;

/// How often a program that closed its side of the terminal is looked at again, as it can
/// open it again from its name while it lives, and nothing tells of that.
const REOPENED: Duration = Duration::from_millis(200);

/// Bytes clients may have waiting for a program that does not read them.
const MOST_INPUT: usize = 256 << 20;

/// Bytes written past which what was written is let go of, while more waits behind it.
const COMPACTED: usize = 1 << 20;

/// What the server's loop hears of a terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The title the program gave; empty when it reset it.
    Title(String),
    Bell,
    Clipboard {
        primary: bool,
        text: String,
    },
    Report(Report),
    /// The program exited, and what it wrote before has been read.
    Exited(ExitStatus),
    /// The program could not be started, and why. The terminal stays, saying so.
    NotStarted(String),
    /// The terminal's thread gave up and hung up on the program, and why.
    Crashed(String),
}

/// How a client shows terminals: its colours, and whether output brings its view down.
#[derive(Debug, Clone)]
pub struct Look {
    pub palette: Palette,
    pub scroll_on_output: bool,
}

/// What the terminal's thread is asked to do.
enum Order {
    Input(Vec<u8>),
    Resize(Size),
    /// The emulator's settings and colours, from the client the terminal follows.
    Follow(caret_term::Options, Palette),
    Show {
        client: ClientId,
        writer: Outbox,
        look: Look,
        row_below: bool,
    },
    Hide(ClientId),
    Look(ClientId, Look),
    Frame(ClientId),
    Hover(ClientId, Option<Point>),
    Scroll(ClientId, Scroll),
    Select(ClientId, Select),
    Focus(ClientId, bool),
    Annotate(String),
    Ask {
        client: ClientId,
        id: u32,
        question: Question,
    },
}

/// The server's handle on a terminal; dropping it hangs up on the program.
pub struct Terminal {
    orders: mpsc::Sender<Order>,
    /// Written to after each order, to wake the thread.
    wake: UnixStream,
    /// Set once the program runs.
    started: Arc<OnceLock<Started>>,
    /// It runs a program it was told to, not the user's shell.
    named_program: bool,
}

/// The name of the process group's leader, as the kernel gives it.
fn program_name_of(group: u32) -> String {
    let name = std::fs::read_to_string(format!("/proc/{group}/comm")).unwrap_or_default();
    name.trim_end().to_owned()
}

/// Whether `name` is a shell's, as `/etc/shells` lists them.
fn is_shell(name: &str) -> bool {
    std::fs::read_to_string("/etc/shells").is_ok_and(|shells| {
        shells
            .lines()
            .filter_map(|line| line.trim().rsplit('/').next())
            .any(|shell| shell == name)
    })
}

/// A live process of the session `leader` leads in a process group of its own: a job its
/// shell stopped or left running behind the prompt, which hanging up ends.
fn job_of(leader: u32) -> Option<u32> {
    std::fs::read_dir("/proc")
        .ok()?
        .flatten()
        .find_map(|entry| {
            let process: u32 = entry.file_name().to_str()?.parse().ok()?;
            let stat = std::fs::read_to_string(entry.path().join("stat")).ok()?;
            // After the name, which may hold spaces and parentheses itself: the state, the
            // parent, the process group and the session.
            let mut fields = stat.get(stat.rfind(')')? + 2..)?.split(' ');
            let state = fields.next()?;
            let group: u32 = fields.nth(1)?.parse().ok()?;
            let session: u32 = fields.next()?.parse().ok()?;
            (session == leader && group != leader && state != "Z").then_some(process)
        })
}

/// What the server's loop asks about a program that runs.
struct Started {
    child: u32,
    /// A second handle on the controlling side of the pseudoterminal, to ask which process
    /// group is in the foreground.
    controller: File,
}

/// What a terminal's thread makes its emulator from.
struct Emulator {
    id: TerminalId,
    size: Size,
    options: caret_term::Options,
    palette: Palette,
    events: mpsc::Sender<Input>,
}

impl Emulator {
    fn worker(self, pty: Option<Pty>, orders: mpsc::Receiver<Order>, woken: UnixStream) -> Worker {
        let (columns, lines) = (usize::from(self.size.columns), usize::from(self.size.lines));
        let mut term = Term::new(columns, lines, self.options);
        term.set_palette_base(palette::base(&self.palette));
        term.set_cell_size(self.size.cell_width, self.size.cell_height);
        Worker {
            id: self.id,
            term,
            reading: pty.is_some(),
            pty,
            unwritten: Unwritten::default(),
            buffer: vec![0; 64 * 1024],
            orders,
            woken,
            events: self.events,
            viewers: HashMap::new(),
            synchronized: None,
            focused: true,
        }
    }
}

/// A terminal's thread until its program runs. Starting one looks for it and enters its
/// directory, which on a mount that has stopped answering takes as long as that does, so
/// it stays off the server's loop and every other terminal.
struct Start {
    command: Command,
    emulator: Emulator,
    started: Arc<OnceLock<Started>>,
}

impl Start {
    fn run(self, orders: mpsc::Receiver<Order>, woken: UnixStream) {
        match self.program() {
            Ok(pty) => self.emulator.worker(Some(pty), orders, woken).run(),
            Err(error) => {
                // The system's words, without the number Rust adds to them.
                let error = error.to_string();
                let reason = error.split(" (os error").next().unwrap_or(&error);
                let told = format!("{}: {reason}", Launch(&self.command).program());
                let failed = Event::NotStarted(reason.to_owned());
                let _ = self
                    .emulator
                    .events
                    .send(Input::Terminal(self.emulator.id, failed));
                let mut worker = self.emulator.worker(None, orders, woken);
                worker.note(&told);
                worker.run();
            }
        }
    }

    fn program(&self) -> io::Result<Pty> {
        let launch = Launch(&self.command);
        let mut env = launch.environment();
        // How `caret attach` run in the terminal finds the window it is in.
        env.insert("CARET_TERMINAL".to_owned(), self.emulator.id.0.to_string());
        let program = launch.program();
        launch.check(&program, &env)?;
        let mut process = std::process::Command::new(&program);
        process
            .args(self.command.argv.iter().skip(1))
            .env_clear()
            .envs(&env);
        if let Some(directory) = launch.directory() {
            process.current_dir(directory);
        }
        let pty = Pty::spawn(process, self.emulator.size)?;
        let controller = match pty.controller.try_clone() {
            Ok(controller) => controller,
            Err(error) => {
                pty.hang_up(Duration::ZERO);
                return Err(error);
            }
        };
        let child = pty.id();
        let _ = self.started.set(Started { child, controller });
        Ok(pty)
    }
}

/// Set beside `GSK_RENDERER` when the window chose GTK's renderer itself, so that the
/// programs it starts get neither.
const CHOSE_RENDERER: &str = "CARET_CHOSE_GSK_RENDERER";

/// Variables that describe the terminal a command line was typed in, or a multiplexer
/// running in it, and would mislead programs in this one.
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
#[expect(
    unsafe_code,
    reason = "getpwuid_r is the only way to the password database"
)]
fn account() -> Option<(String, String, String)> {
    let mut buffer = vec![0; 16 * 1024];
    // SAFETY: an all-zero passwd is a valid value for getpwuid_r to fill in.
    let mut entry: libc::passwd = unsafe { std::mem::zeroed() };
    let mut found = std::ptr::null_mut();
    // SAFETY: every pointer is to memory of the given size that outlives the call.
    let status = unsafe {
        libc::getpwuid_r(
            libc::getuid(),
            &raw mut entry,
            buffer.as_mut_ptr(),
            buffer.len(),
            &raw mut found,
        )
    };
    if status != 0 || found.is_null() {
        return None;
    }
    let field = |pointer: *const libc::c_char| {
        (!pointer.is_null())
            .then(|| {
                // SAFETY: getpwuid_r left the entry's strings NUL-terminated in `buffer`,
                // which is still alive.
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

/// What to run, read from a client's command.
struct Launch<'a>(&'a Command);

/// The name of the program `command` runs, as a tab is called until the program gives
/// it a title.
pub fn program_name(command: &Command) -> String {
    let program = Launch(command).program();
    Path::new(&program)
        .file_name()
        .map_or(program.clone(), |name| name.to_string_lossy().into_owned())
}

impl Launch<'_> {
    fn directory(&self) -> Option<PathBuf> {
        let directory = self.0.directory.as_ref()?;
        Some(PathBuf::from(OsStr::from_bytes(&directory.0)))
    }

    /// The environment the program gets, whole.
    fn environment(&self) -> HashMap<String, String> {
        let mut env: HashMap<String, String> = if self.0.env.is_empty() {
            std::env::vars_os()
                .filter_map(|(name, value)| {
                    Some((name.into_string().ok()?, value.into_string().ok()?))
                })
                .collect()
        } else {
            self.0.env.iter().cloned().collect()
        };
        env.retain(|name, _| !FOREIGN.contains(&name.as_str()));
        if env.remove(CHOSE_RENDERER).is_some() {
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
                ("TERM_PROGRAM", "caret"),
                ("TERM_PROGRAM_VERSION", crate::VERSION),
            ]
            .map(|(name, value)| (name.to_owned(), value.to_owned())),
        );
        env
    }

    /// The program to run: the one named, or the user's shell.
    fn program(&self) -> String {
        let shell = || {
            self.0
                .env
                .iter()
                .find(|(name, _)| name == "SHELL")
                .map(|(_, value)| value.clone())
                .or_else(|| std::env::var("SHELL").ok())
                .or_else(|| account().map(|(_, _, shell)| shell))
        };
        self.0
            .argv
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
            match self.directory() {
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

impl Terminal {
    /// Starts `command` in a terminal `size` large, set up as `options` and `palette` say.
    pub fn spawn(
        id: TerminalId,
        command: &Command,
        size: Size,
        options: caret_term::Options,
        palette: &Palette,
        events: &mpsc::Sender<Input>,
    ) -> io::Result<Self> {
        let started = Arc::new(OnceLock::new());
        let start = Start {
            command: command.clone(),
            emulator: Emulator {
                id,
                size: fitted(size),
                options,
                palette: palette.clone(),
                events: events.clone(),
            },
            started: started.clone(),
        };
        Self::on_a_thread(
            id,
            events,
            started,
            !command.argv.is_empty(),
            |orders, woken| {
                start.run(orders, woken);
            },
        )
    }

    /// A terminal with no program that shows `text`, in place of one that crashed.
    pub fn showing(
        id: TerminalId,
        text: String,
        size: Size,
        options: caret_term::Options,
        palette: &Palette,
        events: &mpsc::Sender<Input>,
    ) -> io::Result<Self> {
        let emulator = Emulator {
            id,
            size: fitted(size),
            options,
            palette: palette.clone(),
            events: events.clone(),
        };
        let started = Arc::new(OnceLock::new());
        Self::on_a_thread(id, events, started, false, move |orders, woken| {
            let mut worker = emulator.worker(None, orders, woken);
            worker.note(&text);
            worker.run();
        })
    }

    fn on_a_thread(
        id: TerminalId,
        events: &mpsc::Sender<Input>,
        started: Arc<OnceLock<Started>>,
        named_program: bool,
        run: impl FnOnce(mpsc::Receiver<Order>, UnixStream) + Send + 'static,
    ) -> io::Result<Self> {
        let (wake, woken) = UnixStream::pair()?;
        woken.set_nonblocking(true)?;
        // A thread busy for long leaves the socket full of wakes it has yet to see. More of
        // them say nothing new, and must not hold up the server's loop.
        wake.set_nonblocking(true)?;
        let (orders, received) = mpsc::channel();
        let told = events.clone();
        std::thread::Builder::new()
            .name(format!("terminal {}", id.0))
            .spawn(move || {
                guarded(id, &told, || run(received, woken));
                give_back_memory();
            })?;
        Ok(Self {
            orders,
            wake,
            started,
            named_program,
        })
    }

    fn order(&self, order: Order) {
        // Both fail only once the thread has ended, when nothing is left to do.
        if self.orders.send(order).is_ok() {
            let _ = (&self.wake).write(&[0]);
        }
    }

    pub fn write(&self, bytes: Vec<u8>) {
        if !bytes.is_empty() {
            self.order(Order::Input(bytes));
        }
    }

    pub fn resize(&self, size: Size) {
        self.order(Order::Resize(size));
    }

    /// Takes the settings and colours of the client the terminal follows now.
    pub fn follow(&self, options: caret_term::Options, palette: Palette) {
        self.order(Order::Follow(options, palette));
    }

    /// Lets `client` see the terminal, through frames sent to `writer`.
    /// Has `client` shown the terminal, sent the row under its view if `row_below`.
    pub fn show(&self, client: ClientId, writer: Outbox, look: Look, row_below: bool) {
        self.order(Order::Show {
            client,
            writer,
            look,
            row_below,
        });
    }

    pub fn hide(&self, client: ClientId) {
        self.order(Order::Hide(client));
    }

    pub fn look(&self, client: ClientId, look: Look) {
        self.order(Order::Look(client, look));
    }

    /// `client` is ready for its next frame.
    pub fn want_frame(&self, client: ClientId) {
        self.order(Order::Frame(client));
    }

    pub fn hover(&self, client: ClientId, point: Option<Point>) {
        self.order(Order::Hover(client, point));
    }

    pub fn scroll(&self, client: ClientId, scroll: Scroll) {
        self.order(Order::Scroll(client, scroll));
    }

    pub fn select(&self, client: ClientId, select: Select) {
        self.order(Order::Select(client, select));
    }

    pub fn focus(&self, client: ClientId, focused: bool) {
        self.order(Order::Focus(client, focused));
    }

    /// Writes `text` under what the program wrote, for a held program that ended.
    pub fn annotate(&self, text: String) {
        self.order(Order::Annotate(text));
    }

    /// Answers a question about what `client` sees, through its writer.
    pub fn ask(&self, client: ClientId, id: u32, question: Question) {
        self.order(Order::Ask {
            client,
            id,
            question,
        });
    }

    /// The name of a program that closing would stop: one in the foreground other than
    /// the shell the terminal started, the program it was started with unless that is a
    /// shell, or a job the shell stopped or runs in the background. A program that took
    /// the terminal into a container, `in_container`, counts as the shell it shows there.
    pub fn busy(&self, in_container: bool) -> Option<String> {
        let started = self.started.get()?;
        let group = pty::foreground(started.controller.as_fd())?;
        let name = program_name_of(group);
        if group != started.child || (self.named_program && !in_container && !is_shell(&name)) {
            return Some(name);
        }
        job_of(started.child).map(program_name_of)
    }

    /// The name of the program in the foreground, unless it is an idle shell the terminal
    /// started: a program named when the terminal was opened counts.
    pub fn foreground_program(&self) -> Option<String> {
        let started = self.started.get()?;
        let group = pty::foreground(started.controller.as_fd())?;
        (group != started.child || self.named_program).then(|| program_name_of(group))
    }

    /// The command line of the program in the foreground; empty when that is the shell
    /// the terminal started.
    pub fn foreground_command(&self) -> Vec<String> {
        let Some(started) = self.started.get() else {
            return Vec::new();
        };
        let Some(group) = pty::foreground(started.controller.as_fd()) else {
            return Vec::new();
        };
        if group == started.child && !self.named_program {
            return Vec::new();
        }
        std::fs::read(format!("/proc/{group}/cmdline"))
            .map(|line| {
                line.split(|&byte| byte == 0)
                    .filter(|word| !word.is_empty())
                    .map(|word| String::from_utf8_lossy(word).into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Where the started program is now, for a new tab to start in the same place.
    pub fn directory(&self) -> Option<PathBuf> {
        let started = self.started.get()?;
        std::fs::read_link(format!("/proc/{}/cwd", started.child)).ok()
    }
}

/// Returns to the system what a terminal that ended held. The allocator keeps memory a thread
/// has freed for the next thread to use, and the server can run for months after the tab
/// with the long history has closed.
#[cfg(target_env = "gnu")]
#[expect(unsafe_code, reason = "malloc_trim is only reached through libc")]
fn give_back_memory() {
    // SAFETY: it takes a number and only changes the allocator's own state.
    unsafe { libc::malloc_trim(0) };
}

#[cfg(not(target_env = "gnu"))]
fn give_back_memory() {}

/// Runs a terminal's thread. A panic in it would otherwise leave a tab that shows the same
/// screen forever and takes no input, the program having been hung up on as it unwound.
fn guarded(id: TerminalId, events: &mpsc::Sender<Input>, run: impl FnOnce()) {
    if panic::catch_unwind(AssertUnwindSafe(run)).is_err() {
        tracing::error!("terminal {} crashed", id.0);
        let crashed = Event::Crashed("The terminal crashed".to_owned());
        let _ = events.send(Input::Terminal(id, crashed));
    }
}

/// The size of the terminal a client asks for at `size`.
fn fitted(size: Size) -> Size {
    Size {
        columns: size.columns.clamp(2, MOST_COLUMNS),
        lines: size.lines.clamp(1, MOST_LINES),
        ..size
    }
}

/// Bytes on their way to the program, in the order they came, as far as it has read them.
/// What it read stays until a lot has been or all is, since the pseudoterminal takes a few
/// kilobytes at a time and moving the rest down for each makes a long paste quadratic.
#[derive(Default)]
struct Unwritten {
    bytes: Vec<u8>,
    written: usize,
}

impl Unwritten {
    fn waiting(&self) -> &[u8] {
        &self.bytes[self.written..]
    }

    fn is_empty(&self) -> bool {
        self.waiting().is_empty()
    }

    /// What a client typed or pasted; dropped, as the only way left, past [`MOST_INPUT`].
    fn input(&mut self, bytes: &[u8]) {
        if self.waiting().len() + bytes.len() > MOST_INPUT {
            tracing::warn!("dropping {} bytes for a program not reading", bytes.len());
            return;
        }
        self.bytes.extend_from_slice(bytes);
    }

    /// What the emulator answers; dropped past [`MOST_BEHIND`].
    fn reply(&mut self, bytes: &[u8]) {
        if self.waiting().len() < MOST_BEHIND {
            self.bytes.extend_from_slice(bytes);
        }
    }

    fn wrote(&mut self, count: usize) {
        self.written += count;
        if self.written == self.bytes.len() {
            self.clear();
        } else if self.written >= COMPACTED {
            self.bytes.drain(..self.written);
            self.written = 0;
        }
    }

    fn clear(&mut self) {
        self.bytes.clear();
        self.written = 0;
    }
}

/// What one client sees of the terminal, and what it was sent.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a separate state of the client"
)]
struct Viewer {
    writer: Outbox,
    view: View,
    /// Its colours, which programs' changes go over.
    base: caret_term::palette::Base,
    scroll_on_output: bool,
    /// It asked for a frame and has not had it yet.
    wants_frame: bool,
    /// Something it shows changed since its last frame.
    dirty: bool,
    sent: Sent,
    hover: Option<Point>,
    focused: bool,
    /// The search it asked for, by the question's id.
    search: Option<(u32, Search)>,
}

impl Viewer {
    fn send(&self, message: &Message) {
        self.writer.send(encoded(message));
    }

    fn answer(&self, id: u32, answer: Answer) {
        self.send(&Message::Answer { id, answer });
    }

    fn set_look(&mut self, look: &Look) {
        self.base = palette::base(&look.palette);
        self.scroll_on_output = look.scroll_on_output;
        self.dirty = true;
    }
}

/// The terminal's thread: the program, the emulator, and every client's view.
struct Worker {
    id: TerminalId,
    term: Term,
    /// None once the program has exited and been waited for.
    pty: Option<Pty>,
    /// The program's output is still read: its side of the terminal is open.
    reading: bool,
    /// Bytes for the program it has not read yet.
    unwritten: Unwritten,
    /// What the program wrote, a read at a time.
    buffer: Vec<u8>,
    orders: mpsc::Receiver<Order>,
    woken: UnixStream,
    events: mpsc::Sender<Input>,
    viewers: HashMap<ClientId, Viewer>,
    /// When the program started holding the screen back.
    synchronized: Option<Instant>,
    /// What the program was last told of the focus.
    focused: bool,
}

impl Worker {
    fn run(mut self) {
        loop {
            let searching = self.viewers.values().any(|viewer| viewer.search.is_some());
            let timeout = if searching {
                Some(Duration::ZERO)
            } else if let Some(since) = self.synchronized {
                Some(SYNCHRONIZED.saturating_sub(since.elapsed()))
            } else {
                (self.pty.is_some() && !self.reading).then_some(REOPENED)
            };
            let mut readers = vec![self.woken.as_fd()];
            let mut writers = Vec::new();
            if let Some(pty) = &self.pty {
                readers.push(pty.exited());
                if self.reading {
                    readers.push(pty.controller.as_fd());
                }
                if !self.unwritten.is_empty() {
                    writers.push(pty.controller.as_fd());
                }
            }
            let ready = pty::wait(&readers, &writers, timeout);
            let (readable, writable) = match ready {
                Ok(ready) => ready,
                Err(error) => {
                    tracing::warn!("waiting on terminal {}: {error}", self.id.0);
                    return;
                }
            };
            if readable[0] && !self.take_orders() {
                return;
            }
            let exited = readable.get(1).copied().unwrap_or(false);
            if readable.get(2).copied().unwrap_or(false) || exited || !self.reading {
                self.read();
            }
            if writable.first().copied().unwrap_or(false) {
                self.write();
            }
            if exited {
                self.exited();
            }
            self.search();
            if self
                .synchronized
                .is_some_and(|since| since.elapsed() >= SYNCHRONIZED)
            {
                self.term.end_synchronized();
            }
            self.after_output();
        }
    }

    /// Writes `text` under what the terminal shows.
    fn note(&mut self, text: &str) {
        let newline = if self.term.cursor_at().1 > 0 {
            "\r\n"
        } else {
            ""
        };
        let text: String = text.chars().filter(|c| !c.is_control()).collect();
        // Bold, then the cursor hidden: nothing is left to type into.
        let text = format!("{newline}\x1b[1m{text}\x1b[0m\x1b[?25l");
        self.term.advance(text.as_bytes());
        for viewer in self.viewers.values_mut() {
            viewer.view.scroll_to_bottom();
        }
        self.touch_all();
    }

    /// Carries out every order waiting; false once the server let go of the terminal.
    fn take_orders(&mut self) -> bool {
        let mut wakes = [0; 64];
        while (&self.woken).read(&mut wakes).is_ok_and(|read| read > 0) {}
        loop {
            match self.orders.try_recv() {
                Ok(order) => self.order(order),
                Err(mpsc::TryRecvError::Empty) => return true,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if let Some(pty) = self.pty.take() {
                        pty.hang_up(PATIENCE);
                    }
                    return false;
                }
            }
        }
    }

    fn order(&mut self, order: Order) {
        match order {
            Order::Input(bytes) => {
                self.unwritten.input(&bytes);
                self.write();
            }
            Order::Resize(size) => {
                let size = fitted(size);
                self.term
                    .resize(usize::from(size.columns), usize::from(size.lines));
                self.term.set_cell_size(size.cell_width, size.cell_height);
                if let Some(pty) = &self.pty {
                    pty.resize(size);
                }
                self.touch_all();
            }
            Order::Follow(options, palette) => {
                self.term.set_options(options);
                self.term.set_palette_base(palette::base(&palette));
                self.write_replies();
                self.touch_all();
            }
            Order::Show {
                client,
                writer,
                look,
                row_below,
            } => {
                let mut viewer = Viewer {
                    writer,
                    view: View::default(),
                    base: caret_term::palette::Base::default(),
                    scroll_on_output: false,
                    wants_frame: false,
                    dirty: true,
                    sent: Sent::new(row_below),
                    hover: None,
                    focused: true,
                    search: None,
                };
                viewer.set_look(&look);
                self.viewers.insert(client, viewer);
            }
            Order::Hide(client) => {
                if let Some(viewer) = self.viewers.remove(&client)
                    && let Some((id, _)) = viewer.search
                {
                    viewer.answer(id, Answer::Gone);
                }
            }
            Order::Look(client, look) => {
                if let Some(viewer) = self.viewers.get_mut(&client) {
                    viewer.set_look(&look);
                }
            }
            Order::Frame(client) => {
                if let Some(viewer) = self.viewers.get_mut(&client) {
                    viewer.wants_frame = true;
                }
            }
            Order::Hover(client, point) => {
                if let Some(viewer) = self.viewers.get_mut(&client)
                    && viewer.hover != point
                {
                    viewer.hover = point;
                    viewer.dirty = true;
                }
            }
            Order::Scroll(client, scroll) => {
                if let Some(viewer) = self.viewers.get_mut(&client) {
                    scroll_view(&mut viewer.view, &self.term, scroll);
                    viewer.dirty = true;
                }
            }
            Order::Select(client, select) => {
                if let Some(viewer) = self.viewers.get_mut(&client) {
                    select_in(viewer, &self.term, select);
                    viewer.dirty = true;
                }
            }
            Order::Focus(client, focused) => {
                if let Some(viewer) = self.viewers.get_mut(&client) {
                    viewer.focused = focused;
                    viewer.dirty = true;
                }
                if focused != self.focused {
                    self.focused = focused;
                    if self.term.modes().focus {
                        self.unwritten
                            .reply(if focused { b"\x1b[I" } else { b"\x1b[O" });
                        self.write();
                    }
                }
            }
            Order::Annotate(text) => self.note(&text),
            Order::Ask {
                client,
                id,
                question,
            } => self.ask(client, id, question),
        }
    }

    fn ask(&mut self, client: ClientId, id: u32, question: Question) {
        let Some(viewer) = self.viewers.get_mut(&client) else {
            return;
        };
        let term = &self.term;
        let answer = match question {
            Question::SelectionText { .. } => Answer::Text {
                text: viewer
                    .view
                    .selected_text(term)
                    .filter(|text| !text.is_empty()),
            },
            Question::LinkAt { point, .. } => Answer::Link {
                uri: viewer
                    .view
                    .link_at(term, usize::from(point.line), usize::from(point.column))
                    .map(|link| link.uri),
            },
            Question::Find { text, upwards, .. } => {
                if let Some((replaced, _)) = viewer.search.take() {
                    viewer.answer(replaced, Answer::Gone);
                }
                let from = viewer.view.search_from(term, upwards);
                if let Some(search) = Search::new(term, &text, upwards, from) {
                    viewer.search = Some((id, search));
                    return;
                }
                viewer.view.clear_selection();
                viewer.dirty = true;
                Answer::Found { found: false }
            }
            _ => Answer::Gone,
        };
        viewer.answer(id, answer);
    }

    /// Takes a step of every search going on.
    fn search(&mut self) {
        let term = &self.term;
        for viewer in self.viewers.values_mut() {
            let Some((id, search)) = &mut viewer.search else {
                continue;
            };
            let id = *id;
            let found = match search.step(term, SEARCH_STEP) {
                Progress::Going => continue,
                Progress::Found(first, last) => {
                    viewer.view.select_cells(first, last);
                    viewer.view.reveal(term, first);
                    viewer.dirty = true;
                    true
                }
                Progress::Missing => false,
            };
            viewer.search = None;
            viewer.answer(id, Answer::Found { found });
        }
    }

    /// Reads what the program wrote, as much as there is up to the budget, and says whether
    /// there may be more.
    fn read(&mut self) -> bool {
        let Some(pty) = &mut self.pty else {
            return false;
        };
        let mut read = 0;
        let mut more = true;
        let started = Instant::now();
        while read < READ_BUDGET && started.elapsed() < READ_TIME {
            match pty.controller.read(&mut self.buffer) {
                Ok(0) => {
                    self.reading = false;
                    more = false;
                    break;
                }
                Ok(count) => {
                    self.reading = true;
                    self.term.advance(&self.buffer[..count]);
                    read += count;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.reading = true;
                    more = false;
                    break;
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                // EIO while no process has the program's side open.
                Err(_) => {
                    self.reading = false;
                    more = false;
                    break;
                }
            }
        }
        if read > 0 {
            self.output();
        }
        more
    }

    /// Writes what the program has not read yet, as far as it takes it.
    fn write(&mut self) {
        let Some(pty) = &mut self.pty else {
            self.unwritten.clear();
            return;
        };
        while !self.unwritten.is_empty() {
            match pty.controller.write(self.unwritten.waiting()) {
                Ok(written) => self.unwritten.wrote(written),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.unwritten.clear();
                    break;
                }
            }
        }
    }

    fn write_replies(&mut self) {
        let replies = self.term.take_replies();
        if !replies.is_empty() {
            self.unwritten.reply(&replies);
            self.write();
        }
    }

    /// What the emulator made of new output: replies to write, events to pass on, and
    /// what the clients see.
    fn output(&mut self) {
        self.write_replies();
        let events = self.term.take_events();
        // Of what was read at once, only the last of what replaces the one before reaches
        // the clients, and one bell: a program doing these in a loop would otherwise send
        // them more than they can take in.
        let last = |wanted: fn(&caret_term::Event) -> bool| events.iter().rposition(wanted);
        let last_title = last(|event| matches!(event, caret_term::Event::Title(_)));
        let last_copy =
            last(|event| matches!(event, caret_term::Event::Clipboard { primary: false, .. }));
        let last_primary =
            last(|event| matches!(event, caret_term::Event::Clipboard { primary: true, .. }));
        let last_notice = last(|event| {
            matches!(
                event,
                caret_term::Event::Report(Report::Notification { .. })
            )
        });
        let mut rung = false;
        for (index, event) in events.into_iter().enumerate() {
            let last = Some(index);
            let event = match event {
                caret_term::Event::Title(title) if last == last_title => Event::Title(title),
                caret_term::Event::Bell if !rung => {
                    rung = true;
                    Event::Bell
                }
                caret_term::Event::Clipboard { primary, text }
                    if last == if primary { last_primary } else { last_copy } =>
                {
                    Event::Clipboard { primary, text }
                }
                caret_term::Event::Report(report @ Report::Notification { .. })
                    if last == last_notice =>
                {
                    Event::Report(report)
                }
                caret_term::Event::Title(_)
                | caret_term::Event::Bell
                | caret_term::Event::Clipboard { .. }
                | caret_term::Event::Report(Report::Notification { .. }) => continue,
                caret_term::Event::Report(report) => Event::Report(report),
            };
            // Fails only once the loop has ended.
            let _ = self.events.send(Input::Terminal(self.id, event));
        }
        let written = self.term.take_written();
        for viewer in self.viewers.values_mut() {
            if let Some(written) = written {
                viewer.view.written(written);
            }
            if viewer.scroll_on_output {
                viewer.view.scroll_to_bottom();
            }
            viewer.dirty = true;
        }
    }

    /// The program exited: what it wrote last is read, and the loop told.
    fn exited(&mut self) {
        // One round ends after a time, which on a busy machine may be before its first read
        // has gone through the emulator, and nothing reads the rest once the program is gone.
        for _ in 0..LEFT_BEHIND {
            if !self.read() {
                break;
            }
        }
        let Some(mut pty) = self.pty.take() else {
            return;
        };
        let status = pty.status();
        self.reading = false;
        if let Some(status) = status {
            let _ = self
                .events
                .send(Input::Terminal(self.id, Event::Exited(status)));
        }
    }

    fn touch_all(&mut self) {
        for viewer in self.viewers.values_mut() {
            viewer.dirty = true;
        }
    }

    /// Sends a frame to every client that is ready for one and has something new to see,
    /// unless the program is in the middle of drawing.
    fn after_output(&mut self) {
        if self.term.modes().synchronized {
            self.synchronized.get_or_insert_with(Instant::now);
            return;
        }
        self.synchronized = None;
        let term = &self.term;
        for viewer in self.viewers.values_mut() {
            if !(viewer.wants_frame && viewer.dirty) {
                continue;
            }
            // A program that reports the mouse gets the click, so no address is marked.
            let link = viewer
                .hover
                .filter(|_| term.modes().mouse == caret_term::term::MouseMode::Off)
                .and_then(|point| {
                    viewer
                        .view
                        .link_at(term, usize::from(point.line), usize::from(point.column))
                });
            let hovered = link.as_ref().map(|link| Hovered {
                uri: link.uri.clone(),
                marked: matches!(link.span, Span::Program(_)),
            });
            let mut palette = term.palette().clone();
            palette.set_base(viewer.base.clone());
            let changes = capture(
                term,
                &viewer.view,
                &palette,
                viewer.focused,
                link.as_ref(),
                &viewer.sent,
            )
            .changes(&mut viewer.sent, hovered);
            viewer.dirty = false;
            // It keeps waiting for a frame until there is something new to see.
            if let Some((frame, whole)) = changes {
                viewer.send(&Message::Frame {
                    terminal: self.id,
                    frame,
                });
                viewer.wants_frame = false;
                viewer.dirty = !whole;
            }
        }
    }
}

fn scroll_view(view: &mut View, term: &Term, scroll: Scroll) {
    match scroll {
        Scroll::Lines { lines } => view.scroll(term, lines as isize),
        Scroll::Page { up } => {
            let page = term.lines() as isize;
            view.scroll(term, if up { page } else { -page });
        }
        Scroll::Bottom => view.scroll_to_bottom(),
        Scroll::To { line } => {
            let rows = term.history().rows();
            view.set_offset(term, rows.saturating_sub(line as usize));
        }
        Scroll::Prompt { up } => view.scroll_to_prompt(term, up),
    }
}

fn select_in(viewer: &mut Viewer, term: &Term, select: Select) {
    let view = &mut viewer.view;
    match select {
        Select::Start { kind, point, right } => {
            let unit = match kind {
                SelectionKind::Simple => Unit::Cell,
                SelectionKind::Word => Unit::Word,
                SelectionKind::Line => Unit::Line,
            };
            view.select(
                term,
                unit,
                usize::from(point.line),
                usize::from(point.column),
                right,
            );
        }
        Select::Extend { point, right } => {
            view.extend(
                term,
                usize::from(point.line),
                usize::from(point.column),
                right,
            );
        }
        Select::All => view.select_all(term),
        Select::Clear => {
            view.clear_selection();
            if let Some((id, _)) = viewer.search.take() {
                viewer.answer(id, Answer::Gone);
            }
        }
    }
}

/// The emulator's settings from a client's options.
pub fn options(options: &caret_protocol::Options) -> caret_term::Options {
    caret_term::Options {
        scrollback: options.scrollback as usize,
        cursor: CursorStyle {
            shape: match options.cursor_shape {
                caret_protocol::CursorShape::Underline => CursorShape::Underline,
                caret_protocol::CursorShape::Beam => CursorShape::Beam,
                caret_protocol::CursorShape::Block | caret_protocol::CursorShape::HollowBlock => {
                    CursorShape::Block
                }
            },
            blinking: options.cursor_blinks,
        },
        copy_from_programs: options.copy_from_programs,
        version: format!("caret {}", crate::VERSION),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replies_stop_piling_up_for_a_program_that_does_not_read() {
        let mut unwritten = Unwritten::default();
        for _ in 0..1_000_000 {
            unwritten.reply(b"\x1b[24;80R");
        }
        assert!(unwritten.waiting().len() < MOST_BEHIND + 16);
        // What is typed still goes through past them.
        unwritten.input(b"typed");
        assert!(unwritten.waiting().ends_with(b"typed"));
    }

    #[test]
    fn input_stays_in_order_however_it_is_written() {
        let mut unwritten = Unwritten::default();
        let sent: Vec<u8> = (0..4 * COMPACTED).map(|at| at as u8).collect();
        unwritten.input(&sent);
        let mut received = Vec::new();
        for take in (1..).map(|round| 1 + round * 7919 % 65536) {
            if unwritten.is_empty() {
                break;
            }
            let waiting = unwritten.waiting();
            let take = take.min(waiting.len());
            received.extend_from_slice(&waiting[..take]);
            unwritten.wrote(take);
        }
        assert_eq!(received, sent);
        assert_eq!(unwritten.bytes, [] as [u8; 0]);
    }

    #[test]
    fn a_terminal_is_never_larger_than_memory_allows() {
        let asked = Size {
            columns: u16::MAX,
            lines: u16::MAX,
            cell_width: 8,
            cell_height: 16,
        };
        let size = fitted(asked);
        assert_eq!((size.columns, size.lines), (MOST_COLUMNS, MOST_LINES));
        let size = fitted(Size {
            columns: 0,
            lines: 0,
            ..asked
        });
        assert_eq!((size.columns, size.lines), (2, 1));
    }

    #[test]
    fn what_a_program_wrote_before_it_exited_is_all_read_however_long_that_takes() {
        // Filling a large screen for each of three bytes takes each read past a round's time.
        let script =
            r"i=0; while [ $i -lt 3000 ]; do printf '\033#8'; i=$((i+1)); done; printf end";
        let size = Size {
            columns: 400,
            lines: 100,
            cell_width: 8,
            cell_height: 16,
        };
        let mut program = std::process::Command::new("sh");
        program.args(["-c", script]);
        let pty = Pty::spawn(program, size).expect("sh starts");
        let ended = pty::wait_readable(&[pty.exited()], Some(Duration::from_secs(20)));
        assert_eq!(ended.expect("poll works"), [true]);
        let (events, _heard) = mpsc::channel();
        let (_orders, received) = mpsc::channel();
        let (_wake, woken) = UnixStream::pair().expect("a socket pair");
        let emulator = Emulator {
            id: TerminalId(1),
            size,
            options: caret_term::Options::default(),
            palette: palette::fallback(),
            events,
        };
        let mut worker = emulator.worker(Some(pty), received, woken);
        worker.exited();
        let shown: String = worker.term.screen().row(0).cells[..3]
            .iter()
            .map(|cell| cell.c)
            .collect();
        assert_eq!(shown, "end");
    }

    #[test]
    fn a_crashed_terminal_tells_the_server() {
        let (events, inputs) = mpsc::channel();
        let id = TerminalId(5);
        guarded(id, &events, || {});
        assert!(inputs.try_recv().is_err());
        std::thread::spawn(move || guarded(id, &events, || panic!("on purpose")))
            .join()
            .expect("the panic stays in the guard");
        assert!(matches!(
            inputs.try_recv(),
            Ok(Input::Terminal(TerminalId(5), Event::Crashed(_)))
        ));
    }

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
        let env = Launch(&command).environment();
        assert_eq!(env.get("EDITOR").map(String::as_str), Some("vi"));
        assert_eq!(env.get("TERM").map(String::as_str), Some("xterm-256color"));
        assert!(!env.contains_key("TMUX"));
        assert!(!env.contains_key("VTE_VERSION"));
        assert_eq!(Launch(&command).program(), "/bin/zsh");
    }

    #[test]
    fn the_renderer_caret_chose_is_not_passed_on() {
        let chosen = command(&[("GSK_RENDERER", "gl"), (CHOSE_RENDERER, "1")]);
        let env = Launch(&chosen).environment();
        assert!(!env.contains_key("GSK_RENDERER"));
        assert!(!env.contains_key(CHOSE_RENDERER));
        let own = command(&[("GSK_RENDERER", "vulkan")]);
        assert_eq!(
            Launch(&own)
                .environment()
                .get("GSK_RENDERER")
                .map(String::as_str),
            Some("vulkan")
        );
    }

    #[test]
    fn a_program_is_looked_for_in_the_path_it_will_run_with() {
        let default = Command::default();
        let command = Launch(&default);
        let path = HashMap::from([("PATH".to_owned(), "/usr/bin:/bin".to_owned())]);
        assert!(command.check("sh", &path).is_ok());
        assert!(command.check("/bin/sh", &HashMap::new()).is_ok());
        assert!(command.check("no-such-program", &path).is_err());
        let nowhere = HashMap::from([("PATH".to_owned(), "/nonexistent".to_owned())]);
        assert!(command.check("sh", &nowhere).is_err());
    }
}
