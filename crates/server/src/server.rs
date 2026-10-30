//! The server's loop: every session, terminal and client, and what each request and each
//! event of a terminal does to them. Everything else, the terminals and the clients'
//! sockets, runs on threads that send it [`Input`].

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::mpsc;
use std::time::Instant;

use caret_protocol::{
    Answer, Bytes, Command, CursorShape, Message, Options, Palette, Question, Request, Scroll,
    SessionId, SessionInfo, Size, Tab, TerminalId,
};
use caret_term::Report;

use crate::palette;
use crate::terminal::{self, Event, Look, Terminal};

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

/// A client's view of a terminal it shows; the rest of it is the terminal's thread's.
#[derive(Default)]
struct View {
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

pub fn encoded(message: &Message) -> Vec<u8> {
    let mut bytes = Vec::new();
    // Writing into a vector fails only for a message past 4 GiB, which no frame is.
    let _ = caret_protocol::send(&mut bytes, message);
    bytes
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

    /// How `client` shows terminals.
    fn look_of(&self, client: ClientId) -> Look {
        self.clients.get(&client).map_or_else(
            || Look {
                palette: palette::fallback(),
                scroll_on_output: false,
            },
            |state| Look {
                palette: state.palette.clone(),
                scroll_on_output: state.options.scroll_on_output,
            },
        )
    }

    /// Lets `client` see `terminal`.
    fn show(&self, terminal: &Terminal, client: ClientId) {
        if let Some(state) = self.clients.get(&client) {
            terminal.show(client, state.writer.clone(), self.look_of(client));
        }
    }

    fn running(&self, terminal: TerminalId) -> Option<&Terminal> {
        self.terminals
            .get(&terminal)
            .and_then(|entry| entry.terminal.as_ref())
    }

    fn tab(entry: &Entry, terminal: TerminalId) -> Tab {
        Tab {
            terminal,
            title: entry.title.clone(),
            name: entry.name.clone(),
        }
    }

    #[expect(clippy::too_many_lines, reason = "one arm for each request")]
    fn request(&mut self, client: ClientId, request: Request) {
        match request {
            Request::Look { palette, options } => {
                if let Some(state) = self.clients.get_mut(&client) {
                    state.palette = palette.clone();
                    state.options = options.clone();
                }
                let look = self.look_of(client);
                for entry in self.terminals.values() {
                    let Some(running) = &entry.terminal else {
                        continue;
                    };
                    if entry.views.contains_key(&client) {
                        running.look(client, look.clone());
                    }
                    if entry.active == Some(client) {
                        running.follow(terminal::options(&options), palette.clone());
                    }
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
                if let Some(running) = self.running(terminal) {
                    running.want_frame(client);
                }
            }
            Request::Input {
                terminal,
                bytes,
                bottom,
            } => {
                self.activate(terminal, client);
                if let Some(running) = self.running(terminal) {
                    if bottom {
                        running.scroll(client, Scroll::Bottom);
                    }
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
                if let Some(running) = self.running(terminal) {
                    running.scroll(client, scroll);
                }
            }
            Request::Select { terminal, select } => {
                if let Some(running) = self.running(terminal) {
                    running.select(client, select);
                }
            }
            Request::Hover { terminal, point } => {
                if let Some(running) = self.running(terminal) {
                    running.hover(client, point);
                }
            }
            Request::Focus { terminal, focused } => {
                if focused {
                    self.activate(terminal, client);
                }
                if let Some(running) = self.running(terminal) {
                    running.focus(client, focused);
                }
            }
            Request::Annotate { terminal, text } => {
                if let Some(running) = self.running(terminal) {
                    running.annotate(text);
                }
            }
        }
    }

    fn ask(&mut self, client: ClientId, id: u32, question: Question) {
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
            // What the client sees is the terminal's thread's to answer.
            Question::SelectionText { terminal }
            | Question::LinkAt { terminal, .. }
            | Question::Find { terminal, .. } => match self.running(terminal) {
                Some(running) => {
                    running.ask(client, id, question);
                    return;
                }
                None => Answer::Gone,
            },
            Question::Busy { terminal } => match self.running(terminal) {
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
            if let Some(entry) = self.terminals.get(&terminal) {
                if let Some(running) = &entry.terminal {
                    self.show(running, client);
                }
                shown.push(Self::tab(entry, terminal));
            }
            if let Some(entry) = self.terminals.get_mut(&terminal) {
                entry.views.insert(client, View::default());
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
                if let Some(running) = &entry.terminal {
                    running.hide(client);
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
        let (options, palette) = self.clients.get(&client).map_or_else(
            || (default_options(), palette::fallback()),
            |state| (state.options.clone(), state.palette.clone()),
        );
        let spawned = Terminal::spawn(
            terminal,
            command,
            size,
            terminal::options(&options),
            &palette,
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
        let viewers = session.clients.clone();
        let views = viewers
            .iter()
            .map(|&viewer| {
                let size = (viewer == client).then_some(size);
                (viewer, View { size })
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
        };
        if let Some(running) = &entry.terminal {
            for viewer in viewers {
                self.show(running, viewer);
            }
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
            entry.views.entry(viewer).or_default();
        }
        if let Some(running) = &entry.terminal {
            for &viewer in old_clients.difference(&new_clients) {
                running.hide(viewer);
            }
        }
        if let Some(running) = self.running(terminal) {
            for &viewer in new_clients.difference(&old_clients) {
                self.show(running, viewer);
            }
        }
        let Some(entry) = self.terminals.get(&terminal) else {
            return;
        };
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
            running.follow(terminal::options(&state.options), state.palette.clone());
        }
        if let Some(size) = size.filter(|&size| size != entry.size) {
            self.resize(terminal, size);
        }
    }

    fn resize(&mut self, terminal: TerminalId, size: Size) {
        let Some(entry) = self.terminals.get_mut(&terminal) else {
            return;
        };
        entry.size = size;
        if let Some(running) = &entry.terminal {
            running.resize(size);
        }
    }

    fn event(&mut self, terminal: TerminalId, event: Event) {
        let Some(entry) = self.terminals.get_mut(&terminal) else {
            return;
        };
        match event {
            Event::Title(title) => {
                entry.title = if title.is_empty() {
                    entry.default_title.clone()
                } else {
                    title
                };
                let title = entry.title.clone();
                self.broadcast_for(terminal, &Message::Title { terminal, title });
            }
            Event::Clipboard { primary, text } => {
                let message = Message::Clipboard {
                    terminal,
                    primary,
                    text,
                };
                match entry.active {
                    Some(client) => self.send(client, &message),
                    None => self.broadcast_for(terminal, &message),
                }
            }
            Event::Bell => self.broadcast_for(terminal, &Message::Bell { terminal }),
            Event::Report(report) => self.report(terminal, report),
            Event::Exited(status) => {
                entry.exit_status = Some(status);
                self.program_ended(terminal);
            }
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
