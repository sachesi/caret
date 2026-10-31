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
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::mpsc;
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

/// How long a program may hold the screen back while it draws.
const SYNCHRONIZED: Duration = Duration::from_millis(150);

/// Lines a search goes through between reads of the program's output.
const SEARCH_STEP: usize = 2000;

/// How long a program has to end after it is hung up on, before it is killed.
const PATIENCE: Duration = Duration::from_secs(2);

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
    child: u32,
    /// A second handle on the controlling side of the pseudoterminal, to ask which process
    /// group is in the foreground.
    controller: File,
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
        let launch = Launch(command);
        let mut env = launch.environment();
        // How `caret attach` run in the terminal finds the window it is in.
        env.insert("CARET_TERMINAL".to_owned(), id.0.to_string());
        let program = launch.program();
        launch.check(&program, &env)?;
        let mut process = std::process::Command::new(&program);
        process
            .args(command.argv.iter().skip(1))
            .env_clear()
            .envs(&env);
        if let Some(directory) = launch.directory() {
            process.current_dir(directory);
        }
        let size = Size {
            columns: size.columns.max(2),
            lines: size.lines.max(1),
            ..size
        };
        let pty = Pty::spawn(process, size)?;
        let child = pty.id();
        let controller = pty.controller.try_clone()?;
        let (wake, woken) = UnixStream::pair()?;
        woken.set_nonblocking(true)?;
        let (orders, received) = mpsc::channel();

        let mut term = Term::new(usize::from(size.columns), usize::from(size.lines), options);
        term.set_palette_base(palette::base(palette));
        term.set_cell_size(size.cell_width, size.cell_height);
        let worker = Worker {
            id,
            term,
            pty: Some(pty),
            reading: true,
            unwritten: Vec::new(),
            buffer: vec![0; 64 * 1024],
            orders: received,
            woken,
            events: events.clone(),
            viewers: HashMap::new(),
            synchronized: None,
            focused: true,
        };
        std::thread::Builder::new()
            .name(format!("terminal {}", id.0))
            .spawn(move || worker.run())?;
        Ok(Self {
            orders,
            wake,
            child,
            controller,
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
    pub fn show(&self, client: ClientId, writer: Outbox, look: Look) {
        self.order(Order::Show {
            client,
            writer,
            look,
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

    /// The name of the program in the foreground when it is not the one the terminal
    /// started, the shell usually: something that closing would interrupt.
    pub fn busy(&self) -> Option<String> {
        let group = pty::foreground(self.controller.as_fd())?;
        if group == self.child {
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
    unwritten: Vec<u8>,
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
            } else {
                self.synchronized
                    .map(|since| SYNCHRONIZED.saturating_sub(since.elapsed()))
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
            if readable.get(2).copied().unwrap_or(false) || exited {
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

    #[expect(clippy::too_many_lines, reason = "one arm for each order")]
    fn order(&mut self, order: Order) {
        match order {
            Order::Input(bytes) => {
                self.unwritten.extend_from_slice(&bytes);
                self.write();
            }
            Order::Resize(size) => {
                let (columns, lines) = (
                    usize::from(size.columns.max(2)),
                    usize::from(size.lines.max(1)),
                );
                self.term.resize(columns, lines);
                self.term.set_cell_size(size.cell_width, size.cell_height);
                if let Some(pty) = &self.pty {
                    pty.resize(Size {
                        columns: columns as u16,
                        lines: lines as u16,
                        ..size
                    });
                }
                self.touch_all();
            }
            Order::Follow(options, palette) => {
                self.term.set_options(options);
                self.term.set_palette_base(palette::base(&palette));
                self.touch_all();
            }
            Order::Show {
                client,
                writer,
                look,
            } => {
                let mut viewer = Viewer {
                    writer,
                    view: View::default(),
                    base: caret_term::palette::Base::default(),
                    scroll_on_output: false,
                    wants_frame: false,
                    dirty: true,
                    sent: Sent::default(),
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
                        self.unwritten.extend_from_slice(if focused {
                            b"\x1b[I"
                        } else {
                            b"\x1b[O"
                        });
                        self.write();
                    }
                }
            }
            Order::Annotate(text) => {
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

    /// Reads what the program wrote, as much as there is up to the budget.
    fn read(&mut self) {
        let Some(pty) = &mut self.pty else {
            return;
        };
        let mut read = 0;
        while read < READ_BUDGET {
            match pty.controller.read(&mut self.buffer) {
                Ok(0) => {
                    self.reading = false;
                    break;
                }
                Ok(count) => {
                    self.term.advance(&self.buffer[..count]);
                    read += count;
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                // EIO once no process has the program's side open.
                Err(_) => {
                    self.reading = false;
                    break;
                }
            }
        }
        if read > 0 {
            self.output();
        }
    }

    /// Writes what the program has not read yet, as far as it takes it.
    fn write(&mut self) {
        let Some(pty) = &mut self.pty else {
            self.unwritten.clear();
            return;
        };
        while !self.unwritten.is_empty() {
            match pty.controller.write(&self.unwritten) {
                Ok(written) => {
                    self.unwritten.drain(..written);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(_) => {
                    self.unwritten.clear();
                    break;
                }
            }
        }
    }

    /// What the emulator made of new output: replies to write, events to pass on, and
    /// what the clients see.
    fn output(&mut self) {
        let replies = self.term.take_replies();
        if !replies.is_empty() {
            self.unwritten.extend_from_slice(&replies);
            self.write();
        }
        let events = self.term.take_events();
        // Of what was read at once, only the last title and one bell reach the clients: a
        // program setting its title in a loop would otherwise send them more than they
        // can take in.
        let last_title = events
            .iter()
            .rposition(|event| matches!(event, caret_term::Event::Title(_)));
        let mut rung = false;
        for (index, event) in events.into_iter().enumerate() {
            let event = match event {
                caret_term::Event::Title(title) if Some(index) == last_title => Event::Title(title),
                caret_term::Event::Bell if !rung => {
                    rung = true;
                    Event::Bell
                }
                caret_term::Event::Title(_) | caret_term::Event::Bell => continue,
                caret_term::Event::Clipboard { primary, text } => {
                    Event::Clipboard { primary, text }
                }
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
        self.read();
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
            let frame = capture(term, &viewer.view, &palette, viewer.focused, link.as_ref())
                .changes(&mut viewer.sent, hovered);
            viewer.send(&Message::Frame {
                terminal: self.id,
                frame,
            });
            viewer.wants_frame = false;
            viewer.dirty = false;
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
