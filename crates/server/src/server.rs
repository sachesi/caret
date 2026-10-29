//! The server's loop: every session, terminal and client, and what each request and each
//! event of a terminal does to them. Everything else, the parsers, the clients' sockets
//! and searches, runs on threads that send it [`Input`].

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Instant;

use alacritty_terminal::event::{Event, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll as GridScroll};
use alacritty_terminal::index::{self, Column, Line, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::search::RegexSearch;
use alacritty_terminal::term::{self, ClipboardType, TermMode, viewport_to_point};
use alacritty_terminal::vte::ansi::{Color, CursorStyle, NamedColor, Processor};
use caret_protocol::{
    Answer, Bytes, Command, CursorShape, Hovered, Message, Options, Palette, Point, Question,
    Request, Row, Scroll, Select, SelectionKind, SessionId, SessionInfo, Size, Tab, TerminalId,
};

use crate::capture::{self, capture};
use crate::links::{self, Span};
use crate::palette;
use crate::search;
use crate::shell::{self, Report};
use crate::terminal::{self, Terminal};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(pub u64);

pub enum Input {
    Connected {
        client: ClientId,
        writer: mpsc::Sender<Vec<u8>>,
    },
    Request(ClientId, Request),
    Disconnected(ClientId),
    Terminal(TerminalId, Event),
    Report(TerminalId, Report),
    /// A search ended: whether it found something, or none when a later one or a cleared
    /// selection cancelled it.
    Found {
        client: ClientId,
        id: u32,
        terminal: TerminalId,
        found: Option<bool>,
    },
}

struct Client {
    writer: mpsc::Sender<Vec<u8>>,
    palette: Palette,
    options: Options,
    sessions: BTreeSet<SessionId>,
}

#[derive(Default)]
struct Session {
    tabs: Vec<TerminalId>,
    clients: BTreeSet<ClientId>,
}

/// A client's view of a terminal it shows.
#[derive(Default)]
struct View {
    /// It asked for a frame and has not had it yet.
    wants_frame: bool,
    /// Something changed since its last frame.
    dirty: bool,
    sent: Vec<Row>,
    hover: Option<Point>,
    size: Option<Size>,
}

struct Entry {
    session: SessionId,
    /// None when the program could not be started.
    terminal: Option<Terminal>,
    failure: Option<String>,
    title: String,
    default_title: String,
    name: Option<String>,
    hold: bool,
    exit_status: Option<ExitStatus>,
    /// A held program failed; its tab stays.
    ended: Option<ExitStatus>,
    /// The client that last had input, whose size, colours and options the terminal takes.
    active: Option<ClientId>,
    views: HashMap<ClientId, View>,
    size: Size,
    /// Where the shell said it is, which is right where `/proc` is not: in a container or
    /// on another machine the shell was started from.
    shell_directory: Option<PathBuf>,
    /// The toolbox or distrobox containers the shell entered, innermost last, by name and
    /// tool.
    containers: Vec<(String, String)>,
    command_started: Option<Instant>,
    /// Counts searches, so that one a later search replaced stops.
    searches: Arc<AtomicU64>,
}

pub struct Server {
    events: mpsc::Sender<Input>,
    sessions: BTreeMap<SessionId, Session>,
    terminals: HashMap<TerminalId, Entry>,
    clients: HashMap<ClientId, Client>,
    next_session: u32,
    next_terminal: u32,
}

fn default_options() -> Options {
    Options {
        scrollback: 10_000,
        cursor_shape: CursorShape::Block,
        cursor_blinks: true,
        copy_from_programs: true,
        scroll_on_output: false,
    }
}

fn config(options: &Options) -> term::Config {
    term::Config {
        scrolling_history: options.scrollback as usize,
        default_cursor_style: CursorStyle {
            shape: capture::cursor_shape(options.cursor_shape),
            blinking: options.cursor_blinks,
        },
        osc52: if options.copy_from_programs {
            term::Osc52::OnlyCopy
        } else {
            term::Osc52::Disabled
        },
        ..term::Config::default()
    }
}

fn window_size(size: Size) -> WindowSize {
    WindowSize {
        num_lines: size.lines.max(1),
        num_cols: size.columns.max(2),
        cell_width: size.cell_width,
        cell_height: size.cell_height,
    }
}

fn encoded(message: &Message) -> Vec<u8> {
    let mut bytes = Vec::new();
    // Writing into a vector fails only for a message past 4 GiB, which no frame is.
    let _ = caret_protocol::send(&mut bytes, message);
    bytes
}

fn grid_point(point: Point) -> index::Point<usize> {
    index::Point::new(usize::from(point.line), Column(usize::from(point.column)))
}

impl Server {
    pub fn new(events: mpsc::Sender<Input>) -> Self {
        Self {
            events,
            sessions: BTreeMap::new(),
            terminals: HashMap::new(),
            clients: HashMap::new(),
            next_session: 1,
            next_terminal: 1,
        }
    }

    /// Ends once it holds nothing and no client is connected: systemd or the next window
    /// starts it again.
    pub fn run(mut self, inputs: &mpsc::Receiver<Input>) {
        while let Ok(input) = inputs.recv() {
            self.handle(input);
            if self.clients.is_empty() && self.sessions.is_empty() {
                tracing::info!("nothing left to keep, ending");
                break;
            }
        }
    }

    fn handle(&mut self, input: Input) {
        match input {
            Input::Connected { client, writer } => {
                self.clients.insert(
                    client,
                    Client {
                        writer,
                        palette: palette::fallback(),
                        options: default_options(),
                        sessions: BTreeSet::new(),
                    },
                );
            }
            Input::Request(client, request) => {
                if self.clients.contains_key(&client) {
                    self.request(client, request);
                }
            }
            Input::Disconnected(client) => self.disconnected(client),
            Input::Terminal(terminal, event) => self.event(terminal, event),
            Input::Report(terminal, report) => self.report(terminal, report),
            Input::Found {
                client,
                id,
                terminal,
                found,
            } => {
                let answer = match found {
                    Some(found) => {
                        self.dirty(terminal);
                        Answer::Found { found }
                    }
                    None => Answer::Gone,
                };
                self.send(client, &Message::Answer { id, answer });
            }
        }
    }

    fn send(&self, client: ClientId, message: &Message) {
        if let Some(client) = self.clients.get(&client) {
            // Fails once its writer has ended, and its reader then says it is gone.
            let _ = client.writer.send(encoded(message));
        }
    }

    /// Sends `message` to every client attached to `session`.
    fn broadcast(&self, session: SessionId, message: &Message) {
        let Some(session) = self.sessions.get(&session) else {
            return;
        };
        let bytes = encoded(message);
        for client in &session.clients {
            if let Some(client) = self.clients.get(client) {
                let _ = client.writer.send(bytes.clone());
            }
        }
    }

    fn broadcast_for(&self, terminal: TerminalId, message: &Message) {
        if let Some(entry) = self.terminals.get(&terminal) {
            self.broadcast(entry.session, message);
        }
    }

    fn palette_of(&self, entry: &Entry) -> Palette {
        entry
            .active
            .into_iter()
            .chain(entry.views.keys().copied())
            .find_map(|client| self.clients.get(&client))
            .map_or_else(palette::fallback, |client| client.palette.clone())
    }

    fn options_of(&self, entry: &Entry) -> Options {
        entry
            .active
            .and_then(|client| self.clients.get(&client))
            .map_or_else(default_options, |client| client.options.clone())
    }

    fn tab(entry: &Entry, terminal: TerminalId) -> Tab {
        Tab {
            terminal,
            title: entry.title.clone(),
            name: entry.name.clone(),
        }
    }

    /// Every client showing `terminal` gets a frame when it next asks, or now if it has.
    fn dirty(&mut self, terminal: TerminalId) {
        if let Some(entry) = self.terminals.get_mut(&terminal) {
            for view in entry.views.values_mut() {
                view.dirty = true;
            }
        }
        self.flush(terminal);
    }

    fn flush(&mut self, terminal: TerminalId) {
        let Some(entry) = self.terminals.get(&terminal) else {
            return;
        };
        let due: Vec<ClientId> = entry
            .views
            .iter()
            .filter(|(_, view)| view.wants_frame && view.dirty)
            .map(|(&client, _)| client)
            .collect();
        for client in due {
            self.send_frame(terminal, client);
        }
    }

    fn send_frame(&mut self, terminal: TerminalId, client: ClientId) {
        let Some(entry) = self.terminals.get(&terminal) else {
            return;
        };
        let palette = self.palette_of(entry);
        let Some(entry) = self.terminals.get_mut(&terminal) else {
            return;
        };
        let Some(running) = entry.terminal.as_ref() else {
            return;
        };
        let Some(view) = entry.views.get_mut(&client) else {
            return;
        };
        // Rearmed before the copy: output after it wakes the loop again.
        running.rearm();
        let term = running.lock();
        // A program that reports the mouse gets the click, so the address is not marked.
        let link = view
            .hover
            .filter(|_| !term.mode().intersects(TermMode::MOUSE_MODE))
            .and_then(|point| links::link_in(&term, grid_point(point)));
        let screen = capture(&term, &palette, link.as_ref().map(|link| &link.span));
        drop(term);
        let hovered = link.map(|link| Hovered {
            marked: matches!(link.span, Span::Program(_)),
            uri: link.uri,
        });
        let frame = screen.changes(&mut view.sent, hovered);
        view.wants_frame = false;
        view.dirty = false;
        self.send(client, &Message::Frame { terminal, frame });
    }

    #[expect(clippy::too_many_lines, reason = "one arm for each request")]
    fn request(&mut self, client: ClientId, request: Request) {
        match request {
            Request::Look { palette, options } => {
                let config = config(&options);
                if let Some(state) = self.clients.get_mut(&client) {
                    state.palette = palette;
                    state.options = options;
                }
                let shown: Vec<TerminalId> = self
                    .terminals
                    .iter()
                    .filter(|(_, entry)| entry.views.contains_key(&client))
                    .map(|(&terminal, _)| terminal)
                    .collect();
                for terminal in shown {
                    if let Some(entry) = self.terminals.get(&terminal)
                        && entry.active == Some(client)
                        && let Some(running) = &entry.terminal
                    {
                        running.lock().set_options(config.clone());
                    }
                    self.dirty(terminal);
                }
            }
            Request::Ask { id, question } => self.ask(client, id, question),
            Request::Attach { session } => self.attach(client, session),
            Request::Detach { session } => self.detach(client, session),
            Request::CloseSession { session } => {
                let tabs = self
                    .sessions
                    .get(&session)
                    .map(|session| session.tabs.clone())
                    .unwrap_or_default();
                for terminal in tabs {
                    self.close(terminal);
                }
                self.forget_if_empty(session);
            }
            Request::CloseTab { terminal } => self.close(terminal),
            Request::MoveTab {
                terminal,
                session,
                index,
            } => self.move_tab(client, terminal, session, index as usize),
            Request::NameTab { terminal, name } => {
                if let Some(entry) = self.terminals.get_mut(&terminal) {
                    entry.name.clone_from(&name);
                    self.broadcast_for(terminal, &Message::TabNamed { terminal, name });
                }
            }
            Request::Frame { terminal } => {
                if let Some(view) = self
                    .terminals
                    .get_mut(&terminal)
                    .and_then(|entry| entry.views.get_mut(&client))
                {
                    view.wants_frame = true;
                }
                self.flush(terminal);
            }
            Request::Input {
                terminal,
                bytes,
                bottom,
            } => {
                self.activate(terminal, client);
                let Some(running) = self
                    .terminals
                    .get(&terminal)
                    .and_then(|entry| entry.terminal.as_ref())
                else {
                    return;
                };
                if bottom {
                    let mut term = running.lock();
                    if term.grid().display_offset() != 0 {
                        term.scroll_display(GridScroll::Bottom);
                        drop(term);
                        self.dirty(terminal);
                    }
                }
                if let Some(running) = self
                    .terminals
                    .get(&terminal)
                    .and_then(|entry| entry.terminal.as_ref())
                {
                    running.write(bytes.0);
                }
            }
            Request::Resize { terminal, size } => {
                let Some(entry) = self.terminals.get_mut(&terminal) else {
                    return;
                };
                if let Some(view) = entry.views.get_mut(&client) {
                    view.size = Some(size);
                }
                if entry.active.is_none_or(|active| active == client) {
                    self.resize(terminal, size);
                }
            }
            Request::Scroll { terminal, scroll } => {
                self.scroll(terminal, scroll);
                self.dirty(terminal);
            }
            Request::Select { terminal, select } => {
                self.select(terminal, select);
                self.dirty(terminal);
            }
            Request::Hover { terminal, point } => {
                if let Some(view) = self
                    .terminals
                    .get_mut(&terminal)
                    .and_then(|entry| entry.views.get_mut(&client))
                    && view.hover != point
                {
                    view.hover = point;
                    view.dirty = true;
                }
                self.flush(terminal);
            }
            Request::Focus { terminal, focused } => {
                if focused {
                    self.activate(terminal, client);
                }
                if let Some(running) = self
                    .terminals
                    .get(&terminal)
                    .and_then(|entry| entry.terminal.as_ref())
                {
                    let mut term = running.lock();
                    term.is_focused = focused;
                    let report = term.mode().contains(TermMode::FOCUS_IN_OUT);
                    drop(term);
                    if report {
                        running.write(if focused { &b"\x1b[I"[..] } else { b"\x1b[O" });
                    }
                }
                self.dirty(terminal);
            }
            Request::Annotate { terminal, text } => {
                if let Some(running) = self
                    .terminals
                    .get(&terminal)
                    .and_then(|entry| entry.terminal.as_ref())
                {
                    let mut term = running.lock();
                    term.scroll_display(GridScroll::Bottom);
                    let newline = if term.grid().cursor.point.column.0 > 0 {
                        "\r\n"
                    } else {
                        ""
                    };
                    let text: String = text.chars().filter(|c| !c.is_control()).collect();
                    // Bold, then the cursor hidden: nothing is left to type into.
                    let text = format!("{newline}\x1b[1m{text}\x1b[0m\x1b[?25l");
                    let mut parser: Processor = Processor::new();
                    parser.advance(&mut *term, text.as_bytes());
                }
                self.dirty(terminal);
            }
        }
    }

    fn ask(&mut self, client: ClientId, id: u32, question: Question) {
        let running = |terminal: TerminalId| {
            self.terminals
                .get(&terminal)
                .and_then(|entry| entry.terminal.as_ref())
        };
        let answer = match question {
            Question::Sessions => Answer::Sessions {
                sessions: self
                    .sessions
                    .iter()
                    .map(|(&session, state)| SessionInfo {
                        session,
                        tabs: state
                            .tabs
                            .iter()
                            .filter_map(|&terminal| {
                                Some(Self::tab(self.terminals.get(&terminal)?, terminal))
                            })
                            .collect(),
                        clients: state.clients.len() as u32,
                    })
                    .collect(),
            },
            Question::NewSession => {
                let session = SessionId(self.next_session);
                self.next_session += 1;
                self.sessions.insert(session, Session::default());
                Answer::Session { session }
            }
            Question::SelectionText { terminal } => match running(terminal) {
                Some(running) => Answer::Text {
                    text: running
                        .lock()
                        .selection_to_string()
                        .filter(|text| !text.is_empty()),
                },
                None => Answer::Gone,
            },
            Question::LinkAt { terminal, point } => match running(terminal) {
                Some(running) => Answer::Link {
                    uri: links::link_in(&running.lock(), grid_point(point)).map(|link| link.uri),
                },
                None => Answer::Gone,
            },
            Question::Busy { terminal } => match running(terminal) {
                Some(running) => Answer::Busy {
                    program: running.busy(),
                },
                None => Answer::Gone,
            },
            Question::Directory { terminal } => match self.terminals.get(&terminal) {
                Some(entry) => Answer::Directory {
                    path: Self::directory(entry)
                        .map(|path| Bytes(path.as_os_str().as_bytes().to_vec())),
                },
                None => Answer::Gone,
            },
            Question::NextCommand { terminal } => match self.terminals.get(&terminal) {
                Some(entry) => {
                    let argv = match entry.containers.last() {
                        Some((name, runtime))
                            if ["toolbox", "distrobox"].contains(&runtime.as_str()) =>
                        {
                            vec![runtime.clone(), "enter".to_owned(), name.clone()]
                        }
                        _ => Vec::new(),
                    };
                    Answer::Command {
                        command: Command {
                            argv,
                            directory: Self::directory(entry)
                                .map(|path| Bytes(path.as_os_str().as_bytes().to_vec())),
                            ..Command::default()
                        },
                    }
                }
                None => Answer::Gone,
            },
            Question::Find {
                terminal,
                text,
                upwards,
            } => {
                self.find(client, id, terminal, &text, upwards);
                return;
            }
            Question::OpenTab {
                session,
                index,
                command,
                size,
            } => {
                self.open_tab(client, id, session, index as usize, &command, size);
                return;
            }
        };
        self.send(client, &Message::Answer { id, answer });
    }

    fn directory(entry: &Entry) -> Option<PathBuf> {
        entry
            .shell_directory
            .clone()
            .filter(|directory| directory.is_dir())
            .or_else(|| entry.terminal.as_ref()?.directory())
    }

    fn find(&mut self, client: ClientId, id: u32, terminal: TerminalId, text: &str, upwards: bool) {
        let Some(entry) = self.terminals.get(&terminal) else {
            self.send(
                client,
                &Message::Answer {
                    id,
                    answer: Answer::Gone,
                },
            );
            return;
        };
        let Some(running) = entry.terminal.as_ref() else {
            self.send(
                client,
                &Message::Answer {
                    id,
                    answer: Answer::Gone,
                },
            );
            return;
        };
        let searches = entry.searches.clone();
        let search = searches.fetch_add(1, Ordering::SeqCst) + 1;
        let regex = match RegexSearch::new(&search::literal(text)) {
            Ok(regex) if !text.is_empty() => regex,
            _ => {
                running.lock().selection = None;
                self.dirty(terminal);
                self.send(
                    client,
                    &Message::Answer {
                        id,
                        answer: Answer::Found { found: false },
                    },
                );
                return;
            }
        };
        let term = running.term.clone();
        let events = self.events.clone();
        let spawned = std::thread::Builder::new()
            .name("search".to_owned())
            .spawn(move || {
                let found = search::search_history(&term, regex, upwards, || {
                    searches.load(Ordering::SeqCst) != search
                });
                let _ = events.send(Input::Found {
                    client,
                    id,
                    terminal,
                    found,
                });
            });
        if let Err(error) = spawned {
            tracing::warn!("starting a search: {error}");
            self.send(
                client,
                &Message::Answer {
                    id,
                    answer: Answer::Gone,
                },
            );
        }
    }

    fn attach(&mut self, client: ClientId, session_id: SessionId) {
        let Some(session) = self.sessions.get_mut(&session_id) else {
            self.send(
                client,
                &Message::SessionClosed {
                    session: session_id,
                },
            );
            return;
        };
        session.clients.insert(client);
        let tabs = session.tabs.clone();
        if let Some(state) = self.clients.get_mut(&client) {
            state.sessions.insert(session_id);
        }
        let mut shown = Vec::new();
        for &terminal in &tabs {
            if let Some(entry) = self.terminals.get_mut(&terminal) {
                entry.views.insert(
                    client,
                    View {
                        dirty: true,
                        ..View::default()
                    },
                );
                shown.push(Self::tab(entry, terminal));
            }
        }
        self.send(
            client,
            &Message::Attached {
                session: session_id,
                tabs: shown,
            },
        );
        for terminal in tabs {
            self.tell_state(client, terminal);
        }
    }

    /// What a client attaching late missed of a tab that failed or ended.
    fn tell_state(&self, client: ClientId, terminal: TerminalId) {
        let Some(entry) = self.terminals.get(&terminal) else {
            return;
        };
        if let Some(reason) = &entry.failure {
            self.send(
                client,
                &Message::Failed {
                    terminal,
                    reason: reason.clone(),
                },
            );
        }
        if let Some(status) = entry.ended {
            self.send(
                client,
                &Message::Ended {
                    terminal,
                    code: status.code(),
                    signal: status.signal(),
                },
            );
        }
    }

    fn detach(&mut self, client: ClientId, session_id: SessionId) {
        if let Some(state) = self.clients.get_mut(&client) {
            state.sessions.remove(&session_id);
        }
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return;
        };
        session.clients.remove(&client);
        for terminal in session.tabs.clone() {
            if let Some(entry) = self.terminals.get_mut(&terminal) {
                entry.views.remove(&client);
                if entry.active == Some(client) {
                    entry.active = None;
                }
            }
        }
        self.forget_if_empty(session_id);
    }

    fn disconnected(&mut self, client: ClientId) {
        let sessions = self
            .clients
            .get(&client)
            .map(|state| state.sessions.clone())
            .unwrap_or_default();
        for session in sessions {
            self.detach(client, session);
        }
        self.clients.remove(&client);
    }

    /// A session with no tabs that no client shows is of no use to anyone.
    fn forget_if_empty(&mut self, session: SessionId) {
        if self
            .sessions
            .get(&session)
            .is_some_and(|state| state.tabs.is_empty() && state.clients.is_empty())
        {
            self.sessions.remove(&session);
        }
    }

    fn open_tab(
        &mut self,
        client: ClientId,
        id: u32,
        session_id: SessionId,
        index: usize,
        command: &Command,
        size: Size,
    ) {
        if !self.sessions.contains_key(&session_id) {
            self.send(
                client,
                &Message::Answer {
                    id,
                    answer: Answer::Gone,
                },
            );
            return;
        }
        let terminal = TerminalId(self.next_terminal);
        self.next_terminal += 1;
        self.send(
            client,
            &Message::Answer {
                id,
                answer: Answer::Opened { terminal },
            },
        );
        let options = self
            .clients
            .get(&client)
            .map_or_else(default_options, |state| state.options.clone());
        let spawned = Terminal::spawn(
            terminal,
            command,
            window_size(size),
            config(&options),
            &self.events,
        );
        let title = terminal::program_name(command);
        let (running, failure) = match spawned {
            Ok(running) => (Some(running), None),
            Err(error) => (None, Some(error.to_string())),
        };
        let session = self
            .sessions
            .get_mut(&session_id)
            .expect("the session was looked up above");
        let index = index.min(session.tabs.len());
        session.tabs.insert(index, terminal);
        let views = session
            .clients
            .iter()
            .map(|&viewer| {
                let size = (viewer == client).then_some(size);
                (
                    viewer,
                    View {
                        dirty: true,
                        size,
                        ..View::default()
                    },
                )
            })
            .collect();
        let entry = Entry {
            session: session_id,
            terminal: running,
            failure: failure.clone(),
            title: title.clone(),
            default_title: title,
            name: None,
            hold: command.hold,
            exit_status: None,
            ended: None,
            active: Some(client),
            views,
            size,
            shell_directory: None,
            containers: Vec::new(),
            command_started: None,
            searches: Arc::default(),
        };
        if let Some(running) = &entry.terminal {
            running.lock().is_focused = true;
        }
        let tab = Self::tab(&entry, terminal);
        self.terminals.insert(terminal, entry);
        self.broadcast(
            session_id,
            &Message::TabOpened {
                session: session_id,
                index: index as u32,
                tab,
            },
        );
        if let Some(reason) = failure {
            self.broadcast(session_id, &Message::Failed { terminal, reason });
        }
    }

    /// Ends the terminal's program, and its tab; the session goes with its last tab.
    fn close(&mut self, terminal: TerminalId) {
        let Some(entry) = self.terminals.remove(&terminal) else {
            return;
        };
        let session_id = entry.session;
        // Hangs up on the program.
        drop(entry);
        self.take_out(session_id, terminal, None);
    }

    /// Takes the tab out of its session, telling every client of it but `mover`, which
    /// moved it elsewhere itself; the session goes with its last tab.
    fn take_out(&mut self, session_id: SessionId, terminal: TerminalId, mover: Option<ClientId>) {
        let Some(session) = self.sessions.get(&session_id) else {
            return;
        };
        let closed = encoded(&Message::TabClosed {
            session: session_id,
            terminal,
        });
        for client in &session.clients {
            if Some(*client) != mover
                && let Some(state) = self.clients.get(client)
            {
                let _ = state.writer.send(closed.clone());
            }
        }
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return;
        };
        session.tabs.retain(|&tab| tab != terminal);
        if session.tabs.is_empty() {
            self.broadcast(
                session_id,
                &Message::SessionClosed {
                    session: session_id,
                },
            );
            if let Some(session) = self.sessions.remove(&session_id) {
                for client in session.clients {
                    if let Some(state) = self.clients.get_mut(&client) {
                        state.sessions.remove(&session_id);
                    }
                }
            }
        }
    }

    /// Puts a tab at `index` of `session_id`. `client` moved it there itself, as a tab is
    /// dragged between windows; every other client hears of it.
    fn move_tab(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
        session_id: SessionId,
        index: usize,
    ) {
        let Some(from) = self.terminals.get(&terminal).map(|entry| entry.session) else {
            return;
        };
        if !self.sessions.contains_key(&session_id) {
            return;
        }
        if from == session_id {
            let Some(session) = self.sessions.get_mut(&session_id) else {
                return;
            };
            session.tabs.retain(|&tab| tab != terminal);
            let index = index.min(session.tabs.len());
            session.tabs.insert(index, terminal);
            self.broadcast(
                session_id,
                &Message::TabMoved {
                    session: session_id,
                    terminal,
                    index: index as u32,
                },
            );
            return;
        }
        let old_clients = self
            .sessions
            .get(&from)
            .map(|session| session.clients.clone())
            .unwrap_or_default();
        self.take_out(from, terminal, Some(client));
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return;
        };
        let index = index.min(session.tabs.len());
        session.tabs.insert(index, terminal);
        let new_clients = session.clients.clone();
        let Some(entry) = self.terminals.get_mut(&terminal) else {
            return;
        };
        entry.session = session_id;
        entry.views.retain(|viewer, _| new_clients.contains(viewer));
        for &viewer in new_clients.difference(&old_clients) {
            entry.views.entry(viewer).or_insert_with(|| View {
                dirty: true,
                ..View::default()
            });
        }
        let tab = Self::tab(entry, terminal);
        let opened = encoded(&Message::TabOpened {
            session: session_id,
            index: index as u32,
            tab,
        });
        for viewer in new_clients {
            if viewer != client
                && let Some(state) = self.clients.get(&viewer)
            {
                let _ = state.writer.send(opened.clone());
            }
        }
        self.dirty(terminal);
    }

    /// The terminal takes `client`'s size, colours and options from now on.
    fn activate(&mut self, terminal: TerminalId, client: ClientId) {
        let Some(entry) = self.terminals.get_mut(&terminal) else {
            return;
        };
        if entry.active == Some(client) {
            return;
        }
        entry.active = Some(client);
        let size = entry.views.get(&client).and_then(|view| view.size);
        if let (Some(running), Some(state)) = (&entry.terminal, self.clients.get(&client)) {
            running.lock().set_options(config(&state.options));
        }
        if let Some(size) = size.filter(|&size| size != entry.size) {
            self.resize(terminal, size);
        }
        self.dirty(terminal);
    }

    fn resize(&mut self, terminal: TerminalId, size: Size) {
        let Some(entry) = self.terminals.get_mut(&terminal) else {
            return;
        };
        entry.size = size;
        if let Some(running) = &entry.terminal {
            running.resize(window_size(size));
        }
        self.dirty(terminal);
    }

    fn scroll(&mut self, terminal: TerminalId, scroll: Scroll) {
        let Some(running) = self
            .terminals
            .get(&terminal)
            .and_then(|entry| entry.terminal.as_ref())
        else {
            return;
        };
        let mut term = running.lock();
        let offset = term.grid().display_offset() as i32;
        let scroll = match scroll {
            Scroll::Lines { lines } => GridScroll::Delta(lines),
            Scroll::Page { up: true } => GridScroll::PageUp,
            Scroll::Page { up: false } => GridScroll::PageDown,
            Scroll::Bottom => GridScroll::Bottom,
            Scroll::To { line } => {
                let wanted = term.history_size().saturating_sub(line as usize) as i32;
                GridScroll::Delta(wanted - offset)
            }
            Scroll::Prompt { up } => {
                let grid = term.grid();
                let columns = grid.columns();
                let marked = |line: i32| {
                    (0..columns).any(|column| {
                        grid[Line(line)][Column(column)]
                            .hyperlink()
                            .is_some_and(|link| link.uri() == shell::PROMPT)
                    })
                };
                let starts = |line: i32| {
                    marked(line) && (line == term.topmost_line().0 || !marked(line - 1))
                };
                let top = -offset;
                let found = if up {
                    (term.topmost_line().0..top)
                        .rev()
                        .find(|&line| starts(line))
                } else {
                    (top + 1..=term.bottommost_line().0).find(|&line| starts(line))
                };
                match found {
                    Some(line) => GridScroll::Delta(-line - offset),
                    None if up => return,
                    None => GridScroll::Bottom,
                }
            }
        };
        term.scroll_display(scroll);
    }

    fn select(&mut self, terminal: TerminalId, select: Select) {
        let Some(entry) = self.terminals.get(&terminal) else {
            return;
        };
        if matches!(select, Select::Clear) {
            entry.searches.fetch_add(1, Ordering::SeqCst);
        }
        let Some(running) = entry.terminal.as_ref() else {
            return;
        };
        let mut term = running.lock();
        let side = |right: bool| if right { Side::Right } else { Side::Left };
        match select {
            Select::Start { kind, point, right } => {
                let kind = match kind {
                    SelectionKind::Simple => SelectionType::Simple,
                    SelectionKind::Word => SelectionType::Semantic,
                    SelectionKind::Line => SelectionType::Lines,
                };
                let point = viewport_to_point(term.grid().display_offset(), grid_point(point));
                term.selection = Some(Selection::new(kind, point, side(right)));
            }
            Select::Extend { point, right } => {
                let point = viewport_to_point(term.grid().display_offset(), grid_point(point));
                match term.selection.as_mut() {
                    Some(selection) => selection.update(point, side(right)),
                    None => {
                        term.selection =
                            Some(Selection::new(SelectionType::Simple, point, side(right)));
                    }
                }
            }
            Select::All => {
                let top = term.topmost_line();
                let blank = |line: Line| {
                    let row = &term.grid()[line];
                    (0..term.columns()).all(|column| row[Column(column)].c == ' ')
                };
                let mut bottom = term.bottommost_line();
                while bottom > top && blank(bottom) {
                    bottom -= 1;
                }
                let mut selection = Selection::new(
                    SelectionType::Simple,
                    index::Point::new(top, Column(0)),
                    Side::Left,
                );
                selection.update(index::Point::new(bottom, term.last_column()), Side::Right);
                term.selection = Some(selection);
            }
            Select::Clear => term.selection = None,
        }
    }

    fn event(&mut self, terminal: TerminalId, event: Event) {
        let Some(entry) = self.terminals.get(&terminal) else {
            return;
        };
        let scroll = self.options_of(entry).scroll_on_output;
        let palette = matches!(event, Event::ColorRequest(..)).then(|| self.palette_of(entry));
        let Some(entry) = self.terminals.get_mut(&terminal) else {
            return;
        };
        match event {
            Event::Wakeup => {
                if scroll && let Some(running) = &entry.terminal {
                    let mut term = running.lock();
                    if term.grid().display_offset() != 0 {
                        term.scroll_display(GridScroll::Bottom);
                    }
                }
                self.dirty(terminal);
            }
            Event::Title(title) => {
                entry.title.clone_from(&title);
                self.broadcast_for(terminal, &Message::Title { terminal, title });
            }
            Event::ResetTitle => {
                entry.title.clone_from(&entry.default_title);
                let title = entry.title.clone();
                self.broadcast_for(terminal, &Message::Title { terminal, title });
            }
            Event::ClipboardStore(kind, text) => {
                let message = Message::Clipboard {
                    terminal,
                    primary: kind == ClipboardType::Selection,
                    text,
                };
                match entry.active {
                    Some(client) => self.send(client, &message),
                    None => self.broadcast_for(terminal, &message),
                }
            }
            Event::ColorRequest(index, format) => {
                let color = match index {
                    0..=255 => Color::Indexed(index as u8),
                    256 => Color::Named(NamedColor::Foreground),
                    257 => Color::Named(NamedColor::Background),
                    _ => Color::Named(NamedColor::Cursor),
                };
                if let (Some(running), Some(palette)) = (&entry.terminal, palette) {
                    let term = running.lock();
                    let resolved = palette::resolve(&palette, color, term.colors());
                    drop(term);
                    running.write(format(palette::ansi_rgb(resolved)).into_bytes());
                }
            }
            Event::PtyWrite(text) => {
                if let Some(running) = &entry.terminal {
                    running.write(text.into_bytes());
                }
            }
            Event::TextAreaSizeRequest(format) => {
                if let Some(running) = &entry.terminal {
                    running.write(format(window_size(entry.size)).into_bytes());
                }
            }
            Event::Bell => self.broadcast_for(terminal, &Message::Bell { terminal }),
            Event::ChildExit(status) => entry.exit_status = Some(status),
            // After the program's last output has been read.
            Event::Exit => self.program_ended(terminal),
            // OSC 52 reads are refused by the terminal's configuration.
            _ => {}
        }
    }

    /// Closes the tab, unless a held program failed: then what it wrote stays, and its
    /// clients describe how it ended.
    fn program_ended(&mut self, terminal: TerminalId) {
        let Some(entry) = self.terminals.get_mut(&terminal) else {
            return;
        };
        match entry.exit_status.filter(|status| !status.success()) {
            Some(status) if entry.hold => {
                entry.ended = Some(status);
                self.broadcast_for(
                    terminal,
                    &Message::Ended {
                        terminal,
                        code: status.code(),
                        signal: status.signal(),
                    },
                );
            }
            _ => self.close(terminal),
        }
    }

    fn report(&mut self, terminal: TerminalId, report: Report) {
        let Some(entry) = self.terminals.get_mut(&terminal) else {
            return;
        };
        // While a command runs, what reaches the terminal is its output, which may be a
        // file or another machine's: only the shell says where it is.
        let from_command = entry.command_started.is_some();
        match report {
            Report::Directory(_) | Report::ContainerEntered { .. } | Report::ContainerLeft
                if from_command => {}
            Report::Directory(directory) => entry.shell_directory = Some(directory),
            Report::Notification { title, body } => self.broadcast_for(
                terminal,
                &Message::Notification {
                    terminal,
                    title,
                    body,
                },
            ),
            Report::CommandStarted => {
                entry.command_started = Some(Instant::now());
                self.broadcast_for(terminal, &Message::CommandStarted { terminal });
            }
            Report::CommandFinished(status) => {
                if let Some(started) = entry.command_started.take() {
                    let seconds = started.elapsed().as_secs();
                    self.broadcast_for(
                        terminal,
                        &Message::CommandFinished {
                            terminal,
                            seconds,
                            status,
                        },
                    );
                }
            }
            Report::ContainerEntered { name, runtime } => entry.containers.push((name, runtime)),
            Report::ContainerLeft => {
                entry.containers.pop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use caret_protocol::{Frame, GlyphText};

    use super::*;

    /// A server with one client, and what that client is sent.
    struct Harness {
        server: Server,
        inputs: mpsc::Receiver<Input>,
        sent: mpsc::Receiver<Vec<u8>>,
    }

    const CLIENT: ClientId = ClientId(1);

    fn harness() -> Harness {
        let (events, inputs) = mpsc::channel();
        let mut server = Server::new(events);
        let (writer, sent) = mpsc::channel();
        server.handle(Input::Connected {
            client: CLIENT,
            writer,
        });
        Harness {
            server,
            inputs,
            sent,
        }
    }

    impl Harness {
        fn request(&mut self, request: Request) {
            self.server.handle(Input::Request(CLIENT, request));
        }

        /// The next message for the client, letting the terminals' events through while
        /// it waits.
        fn next(&mut self, wanted: impl Fn(&Message) -> bool) -> Message {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                while let Ok(bytes) = self.sent.try_recv() {
                    let message = caret_protocol::receive::<Message>(&mut bytes.as_slice())
                        .unwrap()
                        .unwrap();
                    if wanted(&message) {
                        return message;
                    }
                }
                assert!(Instant::now() < deadline, "no message came");
                if let Ok(input) = self.inputs.recv_timeout(Duration::from_millis(20)) {
                    self.server.handle(input);
                }
            }
        }

        fn new_session(&mut self) -> SessionId {
            self.request(Request::Ask {
                id: 1,
                question: Question::NewSession,
            });
            match self.next(|message| matches!(message, Message::Answer { .. })) {
                Message::Answer {
                    answer: Answer::Session { session },
                    ..
                } => session,
                other => panic!("{other:?}"),
            }
        }

        fn open(&mut self, session: SessionId, argv: &[&str]) -> TerminalId {
            self.request(Request::Ask {
                id: 2,
                question: Question::OpenTab {
                    session,
                    index: 0,
                    command: Command {
                        argv: argv.iter().map(|&arg| arg.to_owned()).collect(),
                        hold: true,
                        ..Command::default()
                    },
                    size: Size {
                        columns: 40,
                        lines: 5,
                        cell_width: 8,
                        cell_height: 16,
                    },
                },
            });
            let Message::Answer {
                answer: Answer::Opened { terminal },
                ..
            } = self.next(|message| matches!(message, Message::Answer { id: 2, .. }))
            else {
                panic!("no terminal opened");
            };
            match self.next(|message| matches!(message, Message::TabOpened { .. })) {
                Message::TabOpened { tab, .. } => assert_eq!(tab.terminal, terminal),
                other => panic!("{other:?}"),
            }
            terminal
        }

        /// Frames until one shows `text` somewhere.
        fn frame_with(&mut self, terminal: TerminalId, text: &str) -> Frame {
            let mut screen: Vec<String> = Vec::new();
            loop {
                self.request(Request::Frame { terminal });
                let Message::Frame { frame, .. } =
                    self.next(|message| matches!(message, Message::Frame { .. }))
                else {
                    unreachable!()
                };
                screen.resize(usize::from(frame.lines), String::new());
                for row in &frame.rows {
                    screen[usize::from(row.line)] = row
                        .cells
                        .iter()
                        .filter_map(|cell| match &cell.glyph.as_ref()?.text {
                            GlyphText::Char(c) => Some(*c),
                            GlyphText::Cluster(_) => None,
                        })
                        .collect();
                }
                if screen.iter().any(|line| line.contains(text)) {
                    return frame;
                }
            }
        }
    }

    #[test]
    fn a_tab_shows_its_program_and_closes_with_it() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        let terminal = harness.open(session, &["sh", "-c", "echo hello; read line"]);
        harness.frame_with(terminal, "hello");

        harness.request(Request::Input {
            terminal,
            bytes: Bytes(b"x\n".to_vec()),
            bottom: true,
        });
        harness.next(|message| matches!(message, Message::TabClosed { .. }));
        harness.next(|message| matches!(message, Message::SessionClosed { .. }));
        assert!(harness.server.sessions.is_empty());
    }

    #[test]
    fn a_detached_session_keeps_running_and_shows_again_when_attached() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        let terminal = harness.open(
            session,
            &["sh", "-c", "echo first; sleep 1; echo second; sleep 30"],
        );
        harness.frame_with(terminal, "first");
        harness.server.handle(Input::Disconnected(CLIENT));
        assert!(harness.server.sessions.contains_key(&session));

        let (writer, sent) = mpsc::channel();
        harness.server.handle(Input::Connected {
            client: CLIENT,
            writer,
        });
        harness.sent = sent;
        harness.request(Request::Attach { session });
        match harness.next(|message| matches!(message, Message::Attached { .. })) {
            Message::Attached { tabs, .. } => assert_eq!(tabs[0].terminal, terminal),
            other => panic!("{other:?}"),
        }
        harness.frame_with(terminal, "second");
        harness.request(Request::CloseSession { session });
        harness.next(|message| matches!(message, Message::SessionClosed { .. }));
    }

    #[test]
    fn a_held_program_that_fails_keeps_its_tab() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        let terminal = harness.open(session, &["sh", "-c", "echo oops; exit 3"]);
        match harness.next(|message| matches!(message, Message::Ended { .. })) {
            Message::Ended { code, .. } => assert_eq!(code, Some(3)),
            other => panic!("{other:?}"),
        }
        harness.request(Request::Annotate {
            terminal,
            text: "sh exited with status 3".to_owned(),
        });
        harness.frame_with(terminal, "exitedwithstatus3");
        harness.request(Request::CloseTab { terminal });
        harness.next(|message| matches!(message, Message::SessionClosed { .. }));
    }

    #[test]
    fn a_program_that_cannot_start_is_reported() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        harness.open(session, &["/nonexistent/program"]);
        assert!(matches!(
            harness.next(|message| matches!(message, Message::Failed { .. })),
            Message::Failed { .. }
        ));
    }

    #[test]
    fn a_tab_moved_to_another_session_leaves_the_first_once_empty() {
        let mut harness = harness();
        let first = harness.new_session();
        harness.request(Request::Attach { session: first });
        let terminal = harness.open(first, &["sh", "-c", "echo moved; sleep 30"]);
        let second = harness.new_session();
        harness.request(Request::Attach { session: second });
        harness.request(Request::MoveTab {
            terminal,
            session: second,
            index: 0,
        });
        harness.next(
            |message| matches!(message, Message::SessionClosed { session } if *session == first),
        );
        assert_eq!(harness.server.sessions[&second].tabs, [terminal]);
        harness.frame_with(terminal, "moved");
        harness.request(Request::CloseSession { session: second });
    }

    #[test]
    fn found_text_is_selected() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        let terminal = harness.open(session, &["sh", "-c", "echo a needle here; sleep 30"]);
        harness.frame_with(terminal, "needle");
        harness.request(Request::Ask {
            id: 9,
            question: Question::Find {
                terminal,
                text: "needle".to_owned(),
                upwards: true,
            },
        });
        match harness.next(|message| matches!(message, Message::Answer { id: 9, .. })) {
            Message::Answer { answer, .. } => assert_eq!(answer, Answer::Found { found: true }),
            other => panic!("{other:?}"),
        }
        harness.request(Request::Ask {
            id: 10,
            question: Question::SelectionText { terminal },
        });
        match harness.next(|message| matches!(message, Message::Answer { id: 10, .. })) {
            Message::Answer { answer, .. } => assert_eq!(
                answer,
                Answer::Text {
                    text: Some("needle".to_owned())
                }
            ),
            other => panic!("{other:?}"),
        }
        harness.request(Request::CloseSession { session });
    }
}
