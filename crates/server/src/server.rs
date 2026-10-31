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

use caret_protocol::layout::{MOST_PANES, Node, Plan, WHOLE};
use caret_protocol::{
    Answer, Bytes, Command, CursorShape, Message, Options, Palette, Question, Request, Scroll,
    SessionEntry, SessionId, SessionInfo, Size, Tab, TerminalId,
};
use caret_term::Report;

use crate::listen::Outbox;
use crate::palette;
use crate::terminal::{self, Event, Look, Terminal};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ClientId(pub u64);

pub enum Input {
    Connected {
        client: ClientId,
        /// The protocol version agreed with it.
        version: u16,
        writer: Outbox,
    },
    Request(ClientId, Request),
    Disconnected(ClientId),
    Terminal(TerminalId, Event),
}

/// The first protocol version with split panes.
const PANES: u16 = 2;

/// Containers a terminal keeps track of, innermost last. Any program's output can claim to
/// enter one, and only a shell that says when commands run tells them apart.
const MOST_CONTAINERS: usize = 16;

struct Client {
    writer: Outbox,
    version: u16,
    palette: Palette,
    options: Options,
    sessions: BTreeSet<SessionId>,
}

#[derive(Default)]
struct Session {
    tabs: Vec<TabState>,
    clients: BTreeSet<ClientId>,
    /// The name the user gave it.
    name: Option<String>,
    color: Option<u8>,
    /// What the last window to leave it left: the tab it showed and its profile.
    remembered: Option<(Option<TerminalId>, Option<String>)>,
    /// The client that asked for it, which is about to attach to it or give it a tab.
    asked_by: Option<ClientId>,
}

struct TabState {
    /// The terminal the tab was opened with, which names it to the clients that know of
    /// panes, even once its pane is gone: terminals' ids are never used again.
    key: TerminalId,
    root: Node,
    /// The name the user gave the tab.
    name: Option<String>,
}

impl TabState {
    fn new(key: TerminalId) -> Self {
        Self {
            key,
            root: Node::Pane { terminal: key },
            name: None,
        }
    }

    fn has(&self, terminal: TerminalId) -> bool {
        self.key == terminal || self.root.contains(terminal)
    }

    /// What names the tab to a client of `version`: one from before panes sees a tab as
    /// its first pane.
    fn id(&self, version: u16) -> TerminalId {
        if version >= PANES {
            self.key
        } else {
            self.root.first()
        }
    }

    /// The terminals a client of `version` shows of the tab.
    fn shown(&self, version: u16) -> Vec<TerminalId> {
        if version >= PANES {
            self.root.panes()
        } else {
            vec![self.root.first()]
        }
    }

    /// Whether the tab is more than the pane it was opened with, which clients have to be
    /// told.
    fn divided(&self) -> bool {
        self.root != Node::Pane { terminal: self.key }
    }
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
            Input::Connected {
                client,
                version,
                writer,
            } => {
                self.clients.insert(
                    client,
                    Client {
                        writer,
                        version,
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
            client.writer.send(encoded(message));
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
                client.writer.send(bytes.clone());
            }
        }
    }

    /// Sends `message` about `terminal` to every client that shows it.
    fn broadcast_for(&self, terminal: TerminalId, message: &Message) {
        let Some(session) = self
            .terminals
            .get(&terminal)
            .and_then(|entry| self.sessions.get(&entry.session))
        else {
            return;
        };
        let first = session
            .tabs
            .iter()
            .find(|tab| tab.root.contains(terminal))
            .map(|tab| tab.root.first());
        let bytes = encoded(message);
        for client in &session.clients {
            if let Some(state) = self.clients.get(client)
                && (state.version >= PANES || first == Some(terminal))
            {
                state.writer.send(bytes.clone());
            }
        }
    }

    /// Sends each client attached to `session` but `except` what `message` makes for its
    /// version.
    fn broadcast_versions(
        &self,
        session: SessionId,
        except: Option<ClientId>,
        message: impl Fn(u16) -> Option<Message>,
    ) {
        let Some(session) = self.sessions.get(&session) else {
            return;
        };
        for client in &session.clients {
            if Some(*client) != except
                && let Some(state) = self.clients.get(client)
                && let Some(message) = message(state.version)
            {
                state.writer.send(encoded(&message));
            }
        }
    }

    fn version(&self, client: ClientId) -> u16 {
        self.clients
            .get(&client)
            .map_or(caret_protocol::NEWEST, |state| state.version)
    }

    /// The session and place of the tab `terminal` is a pane of, or the key of.
    fn find_tab(&self, terminal: TerminalId) -> Option<(SessionId, usize)> {
        self.sessions.iter().find_map(|(&id, session)| {
            let index = session.tabs.iter().position(|tab| tab.has(terminal))?;
            Some((id, index))
        })
    }

    fn tab_state(&self, (session, index): (SessionId, usize)) -> Option<&TabState> {
        self.sessions.get(&session)?.tabs.get(index)
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

    /// The tab as a client of `version` knows it, titled as its first pane is.
    fn tab(&self, tab: &TabState, version: u16) -> Tab {
        Tab {
            terminal: tab.id(version),
            title: self
                .terminals
                .get(&tab.root.first())
                .map(|entry| entry.title.clone())
                .unwrap_or_default(),
            name: tab.name.clone(),
        }
    }

    fn layout(session: SessionId, tab: &TabState) -> Message {
        Message::Layout {
            session,
            tab: tab.key,
            root: tab.root.clone(),
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
                let keys: Vec<TerminalId> = self
                    .sessions
                    .get(&session)
                    .map(|session| session.tabs.iter().map(|tab| tab.key).collect())
                    .unwrap_or_default();
                for key in keys {
                    self.close_tab(key);
                }
                self.forget_if_empty(session);
            }
            Request::CloseTab { terminal } => self.close_tab(terminal),
            Request::ClosePane { terminal } => self.close(terminal),
            Request::Divide { terminal, share } => {
                let Some(place) = self.find_tab(terminal) else {
                    return;
                };
                let Some(tab) = self
                    .sessions
                    .get_mut(&place.0)
                    .and_then(|session| session.tabs.get_mut(place.1))
                else {
                    return;
                };
                if tab.root.divide(terminal, share)
                    && let Some(tab) = self.tab_state(place)
                {
                    let layout = Self::layout(place.0, tab);
                    self.broadcast_versions(place.0, None, |version| {
                        (version >= PANES).then(|| layout.clone())
                    });
                }
            }
            Request::SwapPanes { terminal, other } => {
                self.rearrange(terminal, |root| root.swap(terminal, other));
            }
            Request::MovePane {
                terminal,
                beside,
                side,
            } => self.rearrange(terminal, |root| {
                let Some(mut rest) = root
                    .clone()
                    .without(terminal)
                    .filter(|rest| rest.contains(beside))
                else {
                    return false;
                };
                rest.beside(beside, terminal, side);
                *root = rest;
                true
            }),
            Request::PaneToTab { terminal } => self.pane_to_tab(terminal),
            Request::ShowSession { terminal, session } => {
                let Some(entry) = self.terminals.get(&terminal) else {
                    return;
                };
                let clients: Vec<ClientId> = match entry.active {
                    Some(active) => vec![active],
                    None => entry.views.keys().copied().collect(),
                };
                for client in clients {
                    if self.version(client) >= PANES {
                        self.send(client, &Message::ShowSession { session });
                    }
                }
            }
            Request::NameSession { session, name } => {
                let Some(state) = self.sessions.get_mut(&session) else {
                    return;
                };
                state.name.clone_from(&name);
                self.broadcast_versions(session, None, |version| {
                    (version >= PANES).then(|| Message::SessionNamed {
                        session,
                        name: name.clone(),
                    })
                });
            }
            Request::ColorSession { session, color } => {
                let Some(state) = self.sessions.get_mut(&session) else {
                    return;
                };
                state.color = color;
                self.broadcast_versions(session, None, |version| {
                    (version >= PANES).then_some(Message::SessionColored { session, color })
                });
            }
            Request::RememberSession {
                session,
                tab,
                profile,
            } => {
                if let Some(state) = self.sessions.get_mut(&session) {
                    state.remembered = Some((tab, profile));
                }
            }
            Request::MoveTab {
                terminal,
                session,
                index,
            } => self.move_tab(client, terminal, session, index as usize),
            Request::NameTab { terminal, name } => {
                let Some(place) = self.find_tab(terminal) else {
                    return;
                };
                let Some(tab) = self
                    .sessions
                    .get_mut(&place.0)
                    .and_then(|session| session.tabs.get_mut(place.1))
                else {
                    return;
                };
                tab.name.clone_from(&name);
                let (key, first) = (tab.key, tab.root.first());
                self.broadcast_versions(place.0, None, |version| {
                    Some(Message::TabNamed {
                        terminal: if version >= PANES { key } else { first },
                        name: name.clone(),
                    })
                });
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

    /// How many programs other than the shells they started with run in `sessions`.
    fn programs(&self, sessions: &[SessionId]) -> Answer {
        let count = sessions
            .iter()
            .filter_map(|session| self.sessions.get(session))
            .flat_map(|session| &session.tabs)
            .flat_map(|tab| tab.root.panes())
            .filter_map(|terminal| self.running(terminal))
            .filter(|running| running.busy().is_some())
            .count();
        Answer::Programs {
            count: u32::try_from(count).unwrap_or(u32::MAX),
        }
    }

    /// Every session, with its name and how many panes and clients it has.
    fn session_list(&self) -> Answer {
        Answer::SessionList {
            sessions: self
                .sessions
                .iter()
                .map(|(&session, state)| SessionEntry {
                    session,
                    name: state.name.clone(),
                    color: state.color,
                    tabs: state.tabs.iter().map(|tab| self.tab(tab, PANES)).collect(),
                    panes: state
                        .tabs
                        .iter()
                        .map(|tab| tab.root.panes().len() as u32)
                        .sum(),
                    clients: state.clients.len() as u32,
                })
                .collect(),
        }
    }

    fn ask(&mut self, client: ClientId, id: u32, question: Question) {
        let answer = match question {
            Question::Sessions => {
                let version = self.version(client);
                Answer::Sessions {
                    sessions: self
                        .sessions
                        .iter()
                        .map(|(&session, state)| SessionInfo {
                            session,
                            tabs: state
                                .tabs
                                .iter()
                                .map(|tab| self.tab(tab, version))
                                .collect(),
                            clients: state.clients.len() as u32,
                        })
                        .collect(),
                }
            }
            Question::SessionList => self.session_list(),
            Question::Programs { sessions } => self.programs(&sessions),
            Question::NewSession => self.new_session(client),
            // What the client sees is the terminal's thread's to answer.
            Question::SelectionText { terminal }
            | Question::LinkAt { terminal, .. }
            | Question::Find { terminal, .. } => match self
                .terminals
                .get(&terminal)
                .filter(|entry| entry.views.contains_key(&client))
                .and_then(|entry| entry.terminal.as_ref())
            {
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
            Question::Foreground { terminal } => self.foreground(terminal),
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
            Question::Split {
                terminal,
                vertical,
                command,
                size,
            } => {
                self.split(client, id, terminal, vertical, &command, size);
                return;
            }
            Question::Arrange { terminal, plan } => {
                self.arrange(client, id, terminal, &plan);
                return;
            }
        };
        self.send(client, &Message::Answer { id, answer });
    }

    fn new_session(&mut self, client: ClientId) -> Answer {
        let session = SessionId(self.next_session);
        self.next_session += 1;
        self.sessions.insert(
            session,
            Session {
                asked_by: Some(client),
                ..Session::default()
            },
        );
        Answer::Session { session }
    }

    fn foreground(&self, terminal: TerminalId) -> Answer {
        match self.running(terminal) {
            Some(running) => Answer::Command {
                command: Command {
                    argv: running.foreground_command(),
                    ..Command::default()
                },
            },
            None => Answer::Gone,
        }
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
        if let Some(state) = self.clients.get_mut(&client) {
            state.sessions.insert(session_id);
        }
        let version = self.version(client);
        let Some(session) = self.sessions.get(&session_id) else {
            return;
        };
        let shown: Vec<TerminalId> = session
            .tabs
            .iter()
            .flat_map(|tab| tab.shown(version))
            .collect();
        let tabs = session
            .tabs
            .iter()
            .map(|tab| self.tab(tab, version))
            .collect();
        let layouts: Vec<Message> = session
            .tabs
            .iter()
            .filter(|tab| version >= PANES && tab.divided())
            .map(|tab| Self::layout(session_id, tab))
            .collect();
        for &terminal in &shown {
            self.add_view(terminal, client, None);
        }
        self.send(
            client,
            &Message::Attached {
                session: session_id,
                tabs,
            },
        );
        for layout in &layouts {
            self.send(client, layout);
        }
        if let Some(name) = self
            .sessions
            .get(&session_id)
            .and_then(|session| session.name.clone())
            .filter(|_| version >= PANES)
        {
            self.send(
                client,
                &Message::SessionNamed {
                    session: session_id,
                    name: Some(name),
                },
            );
        }
        if let Some(color) = self
            .sessions
            .get(&session_id)
            .and_then(|session| session.color)
            .filter(|_| version >= PANES)
        {
            self.send(
                client,
                &Message::SessionColored {
                    session: session_id,
                    color: Some(color),
                },
            );
        }
        if let Some((tab, profile)) = self
            .sessions
            .get(&session_id)
            .and_then(|session| session.remembered.clone())
            .filter(|_| version >= PANES)
        {
            self.send(
                client,
                &Message::SessionRemembered {
                    session: session_id,
                    tab,
                    profile,
                },
            );
        }
        for terminal in shown {
            self.tell_state(client, terminal);
        }
    }

    /// Lets `client` see `terminal`, at `size` when it gave one.
    fn add_view(&mut self, terminal: TerminalId, client: ClientId, size: Option<Size>) {
        if let Some(running) = self.running(terminal) {
            self.show(running, client);
        }
        if let Some(entry) = self.terminals.get_mut(&terminal) {
            entry.views.insert(client, View { size });
        }
    }

    /// Stops `client` seeing `terminal`.
    fn drop_view(&mut self, terminal: TerminalId, client: ClientId) {
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

    /// What a client attaching late missed of a terminal: its title, which only a tab's
    /// first pane comes with, and whether it failed or ended.
    fn tell_state(&self, client: ClientId, terminal: TerminalId) {
        let Some(entry) = self.terminals.get(&terminal) else {
            return;
        };
        self.send(
            client,
            &Message::Title {
                terminal,
                title: entry.title.clone(),
            },
        );
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
        let panes: Vec<TerminalId> = session
            .tabs
            .iter()
            .flat_map(|tab| tab.root.panes())
            .collect();
        for terminal in panes {
            self.drop_view(terminal, client);
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
        // Asked for and never used, it would keep the server from ever ending.
        self.sessions.retain(|_, session| {
            session.asked_by != Some(client)
                || !session.tabs.is_empty()
                || !session.clients.is_empty()
        });
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
            self.answer(client, id, Answer::Gone);
            return;
        }
        let terminal = self.next_id();
        self.answer(client, id, Answer::Opened { terminal });
        let viewers: Vec<ClientId> = self
            .sessions
            .get(&session_id)
            .map(|session| session.clients.iter().copied().collect())
            .unwrap_or_default();
        let failure = self.spawn(terminal, client, session_id, command, size, &viewers);
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return;
        };
        let index = index.min(session.tabs.len());
        session.tabs.insert(index, TabState::new(terminal));
        let Some(tab) = self.tab_state((session_id, index)) else {
            return;
        };
        let tab = self.tab(tab, caret_protocol::NEWEST);
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

    /// Starts `command` in a pane after `at`'s, which the split leaves half of its room.
    fn split(
        &mut self,
        client: ClientId,
        id: u32,
        at: TerminalId,
        vertical: bool,
        command: &Command,
        size: Size,
    ) {
        let Some(place) = self.find_tab(at).filter(|&place| {
            self.tab_state(place)
                .is_some_and(|tab| tab.root.contains(at) && tab.root.panes().len() < MOST_PANES)
        }) else {
            self.answer(client, id, Answer::Gone);
            return;
        };
        let terminal = self.next_id();
        self.answer(client, id, Answer::Opened { terminal });
        let viewers = self.pane_viewers(place.0);
        let failure = self.spawn(terminal, client, place.0, command, size, &viewers);
        let Some(tab) = self
            .sessions
            .get_mut(&place.0)
            .and_then(|session| session.tabs.get_mut(place.1))
        else {
            return;
        };
        tab.root.split(at, terminal, vertical);
        let Some(tab) = self.tab_state(place) else {
            return;
        };
        let layout = Self::layout(place.0, tab);
        self.broadcast_versions(place.0, None, |version| {
            (version >= PANES).then(|| layout.clone())
        });
        // A tab's title comes with it; a pane's is told apart.
        if let Some(title) = self
            .terminals
            .get(&terminal)
            .map(|entry| entry.title.clone())
        {
            self.broadcast_for(terminal, &Message::Title { terminal, title });
        }
        if let Some(reason) = failure {
            self.broadcast_for(terminal, &Message::Failed { terminal, reason });
        }
    }

    /// The clients of `session` that are shown panes other than a tab's first.
    fn pane_viewers(&self, session: SessionId) -> Vec<ClientId> {
        self.sessions
            .get(&session)
            .map(|session| {
                session
                    .clients
                    .iter()
                    .copied()
                    .filter(|&viewer| self.version(viewer) >= PANES)
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Lays out the tab whose one pane is `terminal` as `plan` says, in one change of its
    /// layout so that the running program is resized once.
    fn arrange(&mut self, client: ClientId, id: u32, terminal: TerminalId, plan: &Plan) {
        let alone = Node::Pane { terminal };
        let Some(place) = self
            .find_tab(terminal)
            .filter(|&place| self.tab_state(place).is_some_and(|tab| tab.root == alone))
            .filter(|_| plan.kept() == 1 && plan.panes() <= MOST_PANES)
        else {
            self.answer(client, id, Answer::Gone);
            return;
        };
        self.answer(client, id, Answer::Opened { terminal });
        let viewers = self.pane_viewers(place.0);
        let mut started = Vec::new();
        let root = self.planned(plan, terminal, (client, place.0, &viewers), &mut started);
        let Some(tab) = self
            .sessions
            .get_mut(&place.0)
            .and_then(|session| session.tabs.get_mut(place.1))
        else {
            return;
        };
        tab.root = root;
        let Some(tab) = self.tab_state(place) else {
            return;
        };
        let layout = Self::layout(place.0, tab);
        self.broadcast_versions(place.0, None, |version| {
            (version >= PANES).then(|| layout.clone())
        });
        for (terminal, failure) in started {
            if let Some(title) = self
                .terminals
                .get(&terminal)
                .map(|entry| entry.title.clone())
            {
                self.broadcast_for(terminal, &Message::Title { terminal, title });
            }
            if let Some(reason) = failure {
                self.broadcast_for(terminal, &Message::Failed { terminal, reason });
            }
        }
    }

    /// The tree `plan` makes around `kept`, its programs started, each noted in `started`
    /// with why it failed, if it did.
    fn planned(
        &mut self,
        plan: &Plan,
        kept: TerminalId,
        place: (ClientId, SessionId, &[ClientId]),
        started: &mut Vec<(TerminalId, Option<String>)>,
    ) -> Node {
        match plan {
            Plan::Kept => Node::Pane { terminal: kept },
            Plan::Run { command, size } => {
                let terminal = self.next_id();
                let failure = self.spawn(terminal, place.0, place.1, command, *size, place.2);
                started.push((terminal, failure));
                Node::Pane { terminal }
            }
            Plan::Split {
                vertical,
                share,
                first,
                second,
            } => Node::Split {
                vertical: *vertical,
                share: (*share).clamp(1, WHOLE - 1),
                first: Box::new(self.planned(first, kept, place, started)),
                second: Box::new(self.planned(second, kept, place, started)),
            },
        }
    }

    fn answer(&self, client: ClientId, id: u32, answer: Answer) {
        self.send(client, &Message::Answer { id, answer });
    }

    fn next_id(&mut self) -> TerminalId {
        let terminal = TerminalId(self.next_terminal);
        self.next_terminal += 1;
        terminal
    }

    /// Starts `command` as `terminal` of `session_id` at `size`, with `client`'s options
    /// and colours, shown to `viewers`; why it could not start, if it could not.
    fn spawn(
        &mut self,
        terminal: TerminalId,
        client: ClientId,
        session_id: SessionId,
        command: &Command,
        size: Size,
        viewers: &[ClientId],
    ) -> Option<String> {
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
        let entry = Entry {
            session: session_id,
            terminal: running,
            failure: failure.clone(),
            title: title.clone(),
            default_title: title,
            hold: command.hold,
            exit_status: None,
            ended: None,
            active: Some(client),
            views: HashMap::new(),
            size,
            shell_directory: None,
            containers: Vec::new(),
            command_started: None,
        };
        self.terminals.insert(terminal, entry);
        for &viewer in viewers {
            self.add_view(terminal, viewer, (viewer == client).then_some(size));
        }
        failure
    }

    /// Ends every pane of the tab `terminal` names, and the tab.
    fn close_tab(&mut self, terminal: TerminalId) {
        let Some(place) = self.find_tab(terminal) else {
            return;
        };
        let panes = self
            .tab_state(place)
            .map(|tab| tab.root.panes())
            .unwrap_or_default();
        // Dropping them hangs up on their programs.
        for pane in panes {
            self.terminals.remove(&pane);
        }
        self.take_out(place, None);
    }

    /// Ends the terminal's program and its pane; the tab goes with its last pane.
    fn close(&mut self, terminal: TerminalId) {
        let Some(place) = self.find_tab(terminal) else {
            return;
        };
        let Some(tab) = self
            .sessions
            .get_mut(&place.0)
            .and_then(|session| session.tabs.get_mut(place.1))
        else {
            return;
        };
        if !tab.root.contains(terminal) {
            return;
        }
        let first = tab.root.first();
        let rest = tab.root.clone().without(terminal);
        // Dropping it hangs up on the program.
        self.terminals.remove(&terminal);
        let Some(rest) = rest else {
            self.take_out(place, None);
            return;
        };
        if let Some(tab) = self
            .sessions
            .get_mut(&place.0)
            .and_then(|session| session.tabs.get_mut(place.1))
        {
            tab.root = rest;
        }
        self.relaid(place, first);
    }

    /// Tells the tab's clients its panes changed, `first` having been its first pane.
    fn relaid(&mut self, place: (SessionId, usize), first: TerminalId) {
        let Some(tab) = self.tab_state(place) else {
            return;
        };
        let layout = Self::layout(place.0, tab);
        let after = tab.root.first();
        let opened = (first != after).then(|| self.tab(tab, 1));
        self.broadcast_versions(place.0, None, |version| {
            (version >= PANES).then(|| layout.clone())
        });
        // Clients from before panes see the tab as its first pane, which is another now.
        let Some(opened) = opened else {
            return;
        };
        let older: Vec<ClientId> = self
            .sessions
            .get(&place.0)
            .map(|session| {
                session
                    .clients
                    .iter()
                    .copied()
                    .filter(|&client| self.version(client) < PANES)
                    .collect()
            })
            .unwrap_or_default();
        for client in older {
            self.drop_view(first, client);
            self.add_view(after, client, None);
            // Opened before the other closes, so that the window is never without tabs.
            self.send(
                client,
                &Message::TabOpened {
                    session: place.0,
                    index: place.1 as u32,
                    tab: opened.clone(),
                },
            );
            self.send(
                client,
                &Message::TabClosed {
                    session: place.0,
                    terminal: first,
                },
            );
            self.tell_state(client, after);
        }
    }

    /// Changes the panes of `terminal`'s tab as `change` does, when it says it did.
    fn rearrange(&mut self, terminal: TerminalId, change: impl FnOnce(&mut Node) -> bool) {
        let Some(place) = self.find_tab(terminal) else {
            return;
        };
        let Some(tab) = self
            .sessions
            .get_mut(&place.0)
            .and_then(|session| session.tabs.get_mut(place.1))
        else {
            return;
        };
        let first = tab.root.first();
        if tab.root.contains(terminal) && change(&mut tab.root) {
            self.relaid(place, first);
        }
    }

    /// Takes the pane out of its tab into a tab of its own, after it. The tab keeps its
    /// key's pane, so that a tab's key is never another's pane: when that is the one
    /// taken out, the others go to the new tab before it instead, with its name.
    fn pane_to_tab(&mut self, terminal: TerminalId) {
        let Some((session_id, index)) = self.find_tab(terminal) else {
            return;
        };
        let key = self.next_id();
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return;
        };
        let Some(tab) = session.tabs.get_mut(index) else {
            return;
        };
        if !tab.root.contains(terminal) {
            return;
        }
        let first = tab.root.first();
        let Some(rest) = tab.root.clone().without(terminal) else {
            return;
        };
        let alone = Node::Pane { terminal };
        let (kept, index, added) = if tab.key == terminal {
            let added = TabState {
                key,
                root: rest,
                name: tab.name.take(),
            };
            tab.root = alone;
            (index + 1, index, added)
        } else {
            tab.root = rest;
            let added = TabState {
                key,
                root: alone,
                name: None,
            };
            (index, index + 1, added)
        };
        session.tabs.insert(index, added);
        self.relaid((session_id, kept), first);
        let Some(tab) = self.tab_state((session_id, index)) else {
            return;
        };
        let (newer, older) = (self.tab(tab, PANES), self.tab(tab, 1));
        let layout = Self::layout(session_id, tab);
        self.broadcast_versions(session_id, None, |version| {
            Some(Message::TabOpened {
                session: session_id,
                index: index as u32,
                tab: if version >= PANES {
                    newer.clone()
                } else {
                    older.clone()
                },
            })
        });
        self.broadcast_versions(session_id, None, |version| {
            (version >= PANES).then(|| layout.clone())
        });
        // Clients start the new tab's panes again, so they see them whole.
        let clients: Vec<ClientId> = self
            .sessions
            .get(&session_id)
            .map(|session| session.clients.iter().copied().collect())
            .unwrap_or_default();
        for client in clients {
            let shown = self
                .tab_state((session_id, index))
                .map(|tab| tab.shown(self.version(client)))
                .unwrap_or_default();
            for pane in shown {
                self.add_view(pane, client, None);
                self.tell_state(client, pane);
            }
        }
    }

    /// Takes the tab out of its session, telling every client of it but `mover`, which
    /// moved it elsewhere itself; the session goes with its last tab.
    fn take_out(&mut self, (session_id, index): (SessionId, usize), mover: Option<ClientId>) {
        let Some(tab) = self.tab_state((session_id, index)) else {
            return;
        };
        let (key, first) = (tab.key, tab.root.first());
        self.broadcast_versions(session_id, mover, |version| {
            Some(Message::TabClosed {
                session: session_id,
                terminal: if version >= PANES { key } else { first },
            })
        });
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return;
        };
        session.tabs.remove(index);
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

    /// Puts the tab `terminal` names at `index` of `session_id`. `client` moved it there
    /// itself, as a tab is dragged between windows; every other client hears of it.
    fn move_tab(
        &mut self,
        client: ClientId,
        terminal: TerminalId,
        session_id: SessionId,
        index: usize,
    ) {
        let Some(from) = self.find_tab(terminal) else {
            return;
        };
        if !self.sessions.contains_key(&session_id) {
            return;
        }
        if from.0 == session_id {
            let Some(session) = self.sessions.get_mut(&session_id) else {
                return;
            };
            let tab = session.tabs.remove(from.1);
            let index = index.min(session.tabs.len());
            let (key, first) = (tab.key, tab.root.first());
            session.tabs.insert(index, tab);
            self.broadcast_versions(session_id, None, |version| {
                Some(Message::TabMoved {
                    session: session_id,
                    terminal: if version >= PANES { key } else { first },
                    index: index as u32,
                })
            });
            return;
        }
        let old_clients = self
            .sessions
            .get(&from.0)
            .map(|session| session.clients.clone())
            .unwrap_or_default();
        let panes = self
            .tab_state(from)
            .map(|tab| tab.root.panes())
            .unwrap_or_default();
        let Some(tab) = self.tab_state(from).map(|tab| TabState {
            key: tab.key,
            root: tab.root.clone(),
            name: tab.name.clone(),
        }) else {
            return;
        };
        self.take_out(from, Some(client));
        for &pane in &panes {
            for &viewer in &old_clients {
                self.drop_view(pane, viewer);
            }
            if let Some(entry) = self.terminals.get_mut(&pane) {
                entry.session = session_id;
            }
        }
        let Some(session) = self.sessions.get_mut(&session_id) else {
            return;
        };
        let index = index.min(session.tabs.len());
        session.tabs.insert(index, tab);
        let new_clients: Vec<ClientId> = session.clients.iter().copied().collect();
        let place = (session_id, index);
        for &viewer in &new_clients {
            let shown = self
                .tab_state(place)
                .map(|tab| tab.shown(self.version(viewer)))
                .unwrap_or_default();
            for pane in shown {
                self.add_view(pane, viewer, None);
            }
        }
        let Some(tab) = self.tab_state(place) else {
            return;
        };
        let layout = tab.divided().then(|| Self::layout(session_id, tab));
        let (newer, older) = (self.tab(tab, PANES), self.tab(tab, 1));
        self.broadcast_versions(session_id, Some(client), |version| {
            Some(Message::TabOpened {
                session: session_id,
                index: index as u32,
                tab: if version >= PANES {
                    newer.clone()
                } else {
                    older.clone()
                },
            })
        });
        if let Some(layout) = layout {
            self.broadcast_versions(session_id, None, |version| {
                (version >= PANES).then(|| layout.clone())
            });
        }
        for viewer in new_clients {
            let shown = self
                .tab_state(place)
                .map(|tab| tab.shown(self.version(viewer)))
                .unwrap_or_default();
            for pane in shown {
                self.tell_state(viewer, pane);
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
            Event::NotStarted(reason) => {
                entry.failure = Some(reason.clone());
                self.broadcast_for(terminal, &Message::Failed { terminal, reason });
            }
            Event::Crashed(reason) => {
                entry.terminal = None;
                // Should what replaces it crash as well, nothing replaces that.
                if entry.failure.replace(reason.clone()).is_none() {
                    self.replace_crashed(terminal, reason.clone());
                }
                self.broadcast_for(terminal, &Message::Failed { terminal, reason });
            }
        }
    }

    /// Puts a terminal that says why in place of one that crashed, for its pane to show.
    fn replace_crashed(&mut self, terminal: TerminalId, reason: String) {
        let Some(entry) = self.terminals.get(&terminal) else {
            return;
        };
        let viewers: Vec<ClientId> = entry.views.keys().copied().collect();
        let client = entry.active.or_else(|| viewers.first().copied());
        let (options, palette) = client
            .and_then(|client| self.clients.get(&client))
            .map_or_else(
                || (default_options(), palette::fallback()),
                |state| (state.options.clone(), state.palette.clone()),
            );
        let showing = Terminal::showing(
            terminal,
            reason,
            entry.size,
            terminal::options(&options),
            &palette,
            &self.events,
        );
        let showing = match showing {
            Ok(showing) => showing,
            Err(error) => {
                tracing::error!("replacing terminal {}: {error}", terminal.0);
                return;
            }
        };
        for &viewer in &viewers {
            self.show(&showing, viewer);
            // What it had asked of the one that crashed will not come.
            showing.want_frame(viewer);
        }
        if let Some(entry) = self.terminals.get_mut(&terminal) {
            entry.terminal = Some(showing);
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
            Report::ContainerEntered { name, runtime } => {
                if entry.containers.len() == MOST_CONTAINERS {
                    entry.containers.remove(0);
                }
                entry.containers.push((name, runtime));
            }
            Report::ContainerLeft => {
                entry.containers.pop();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;
    use std::time::Duration;

    use caret_protocol::layout::Side;
    use caret_protocol::{Frame, GlyphText};

    use super::*;

    /// A server with one client, and what that client is sent.
    struct Harness {
        server: Server,
        inputs: mpsc::Receiver<Input>,
        sent: mpsc::Receiver<Vec<u8>>,
    }

    const CLIENT: ClientId = ClientId(1);

    const SIZE: Size = Size {
        columns: 40,
        lines: 5,
        cell_width: 8,
        cell_height: 16,
    };

    /// An outbox, and the messages sent to it.
    fn outbox() -> (Outbox, mpsc::Receiver<Vec<u8>>) {
        let (socket, _) = UnixStream::pair().expect("a socket pair");
        let (outbox, queued) = Outbox::new(socket);
        (outbox, queued.messages)
    }

    fn harness() -> Harness {
        let (events, inputs) = mpsc::channel();
        let mut server = Server::new(events);
        let (writer, sent) = outbox();
        server.handle(Input::Connected {
            client: CLIENT,
            version: caret_protocol::NEWEST,
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

        fn split(&mut self, at: TerminalId, argv: &[&str]) -> TerminalId {
            self.request(Request::Ask {
                id: 3,
                question: Question::Split {
                    terminal: at,
                    vertical: true,
                    command: Command {
                        argv: argv.iter().map(|&arg| arg.to_owned()).collect(),
                        ..Command::default()
                    },
                    size: SIZE,
                },
            });
            let Message::Answer {
                answer: Answer::Opened { terminal },
                ..
            } = self.next(|message| matches!(message, Message::Answer { id: 3, .. }))
            else {
                panic!("no pane opened");
            };
            terminal
        }

        /// The panes of the next layout the client is sent.
        fn panes(&mut self) -> Vec<TerminalId> {
            match self.next(|message| matches!(message, Message::Layout { .. })) {
                Message::Layout { root, .. } => root.panes(),
                _ => unreachable!(),
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
                    size: SIZE,
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
    fn a_named_program_tells_its_command_line() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        let terminal = harness.open(session, &["sleep", "30"]);
        // The program takes over the terminal's process group only once started.
        let mut argv = Vec::new();
        for _ in 0..50 {
            harness.request(Request::Ask {
                id: 6,
                question: Question::Foreground { terminal },
            });
            let answer = harness.next(|message| matches!(message, Message::Answer { id: 6, .. }));
            let Message::Answer {
                answer: Answer::Command { command },
                ..
            } = answer
            else {
                panic!("{answer:?}");
            };
            argv = command.argv;
            if argv.first().is_some_and(|program| program == "sleep") {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(argv, ["sleep", "30"]);
    }

    #[test]
    fn a_tab_of_one_pane_takes_a_whole_plan_at_once() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        let kept = harness.open(session, &["sleep", "30"]);
        let run = |text: &str| {
            Box::new(Plan::Run {
                command: Command {
                    argv: vec![
                        "sh".to_owned(),
                        "-c".to_owned(),
                        format!("echo {text}; sleep 30"),
                    ],
                    ..Command::default()
                },
                size: SIZE,
            })
        };
        let plan = Plan::Split {
            vertical: true,
            share: 3000,
            first: run("left"),
            second: Box::new(Plan::Split {
                vertical: false,
                share: 5000,
                first: Box::new(Plan::Kept),
                second: run("below"),
            }),
        };
        let arrange = |plan: Plan| Request::Ask {
            id: 5,
            question: Question::Arrange {
                terminal: kept,
                plan,
            },
        };
        harness.request(arrange(*run("nothing kept")));
        assert!(matches!(
            harness.next(|message| matches!(message, Message::Answer { id: 5, .. })),
            Message::Answer {
                answer: Answer::Gone,
                ..
            }
        ));

        harness.request(arrange(plan.clone()));
        let Message::Layout { tab, root, .. } =
            harness.next(|message| matches!(message, Message::Layout { .. }))
        else {
            unreachable!();
        };
        assert_eq!(tab, kept);
        let panes = root.panes();
        assert_eq!(panes.len(), 3);
        assert_eq!(panes[1], kept);
        assert_eq!(root.share(panes[1]), Some(3000));
        harness.frame_with(panes[0], "left");
        harness.frame_with(panes[2], "below");

        harness.request(arrange(plan));
        assert!(matches!(
            harness.next(|message| matches!(message, Message::Answer { id: 5, .. })),
            Message::Answer {
                answer: Answer::Gone,
                ..
            }
        ));
    }

    #[test]
    fn of_the_titles_a_program_sets_at_once_only_the_last_is_sent() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        harness.open(
            session,
            &[
                "sh",
                "-c",
                "printf '\\033]0;one\\007\\033]0;two\\007'; sleep 30",
            ],
        );
        let set = |message: &Message| matches!(message, Message::Title { title, .. } if title == "one" || title == "two");
        match harness.next(set) {
            Message::Title { title, .. } => assert_eq!(title, "two"),
            other => panic!("{other:?}"),
        }
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

        let (writer, sent) = outbox();
        harness.server.handle(Input::Connected {
            client: CLIENT,
            version: caret_protocol::NEWEST,
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
        let terminal = harness.open(session, &["/nonexistent/program"]);
        assert!(matches!(
            harness.next(|message| matches!(message, Message::Failed { .. })),
            Message::Failed { .. }
        ));
        harness.frame_with(terminal, "/nonexistent/program:Nosuch");
        harness.request(Request::CloseTab { terminal });
        harness.next(|message| matches!(message, Message::SessionClosed { .. }));
    }

    #[test]
    fn a_terminal_that_crashes_is_replaced_by_one_saying_so() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        let terminal = harness.open(session, &["cat"]);
        let crashed = Event::Crashed("The terminal crashed".to_owned());
        harness.server.handle(Input::Terminal(terminal, crashed));
        assert!(matches!(
            harness.next(|message| matches!(message, Message::Failed { .. })),
            Message::Failed { .. }
        ));
        harness.frame_with(terminal, "Theterminalcrashed");
        harness.request(Request::CloseTab { terminal });
        harness.next(|message| matches!(message, Message::SessionClosed { .. }));
    }

    #[test]
    fn a_terminal_that_crashes_again_is_not_replaced_again() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        let terminal = harness.open(session, &["cat"]);
        for _ in 0..2 {
            let crashed = Event::Crashed("The terminal crashed".to_owned());
            harness.server.handle(Input::Terminal(terminal, crashed));
        }
        assert!(harness.server.running(terminal).is_none());
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
        assert_eq!(harness.server.sessions[&second].tabs[0].key, terminal);
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

    #[test]
    fn a_tab_of_panes_closes_with_its_last() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        let first = harness.open(session, &["sh", "-c", "echo left; sleep 30"]);
        let second = harness.split(first, &["sh", "-c", "echo right; sleep 30"]);
        assert_eq!(harness.panes(), [first, second]);
        harness.frame_with(second, "right");

        harness.request(Request::ClosePane { terminal: first });
        assert_eq!(harness.panes(), [second]);
        // The tab keeps the name it was opened with.
        harness.request(Request::ClosePane { terminal: second });
        match harness.next(|message| matches!(message, Message::TabClosed { .. })) {
            Message::TabClosed { terminal, .. } => assert_eq!(terminal, first),
            other => panic!("{other:?}"),
        }
        harness.next(|message| matches!(message, Message::SessionClosed { .. }));
    }

    #[test]
    fn panes_trade_places_move_and_go_to_tabs_of_their_own() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        let first = harness.open(session, &["sh", "-c", "sleep 30"]);
        let second = harness.split(first, &["sh", "-c", "sleep 30"]);
        assert_eq!(harness.panes(), [first, second]);
        let third = harness.split(second, &["sh", "-c", "sleep 30"]);
        assert_eq!(harness.panes(), [first, second, third]);

        harness.request(Request::SwapPanes {
            terminal: first,
            other: third,
        });
        assert_eq!(harness.panes(), [third, second, first]);
        harness.request(Request::MovePane {
            terminal: third,
            beside: first,
            side: Side::Below,
        });
        assert_eq!(harness.panes(), [second, first, third]);

        let opened = |harness: &mut Harness| match harness
            .next(|message| matches!(message, Message::TabOpened { .. }))
        {
            Message::TabOpened { index, tab, .. } => (index, tab.terminal),
            _ => unreachable!(),
        };
        harness.request(Request::PaneToTab { terminal: second });
        assert_eq!(harness.panes(), [first, third]);
        let (index, key) = opened(&mut harness);
        assert_eq!(index, 1);
        match harness.next(|message| matches!(message, Message::Layout { .. })) {
            Message::Layout { tab, root, .. } => {
                assert_eq!((tab, root.panes()), (key, vec![second]));
            }
            _ => unreachable!(),
        }

        // The tab keeps the pane it is known by, and the others leave it.
        harness.request(Request::PaneToTab { terminal: first });
        assert_eq!(harness.panes(), [first]);
        let (index, key) = opened(&mut harness);
        assert_eq!(index, 0);
        match harness.next(|message| matches!(message, Message::Layout { .. })) {
            Message::Layout { tab, root, .. } => {
                assert_eq!((tab, root.panes()), (key, vec![third]));
            }
            _ => unreachable!(),
        }
        harness.request(Request::CloseSession { session });
    }

    #[test]
    fn a_named_session_is_listed_and_attached_with_its_name_and_colour() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        harness.open(session, &["sh", "-c", "sleep 30"]);
        harness.request(Request::NameSession {
            session,
            name: Some("build".to_owned()),
        });
        harness.request(Request::ColorSession {
            session,
            color: Some(3),
        });
        harness.request(Request::Ask {
            id: 4,
            question: Question::SessionList,
        });
        match harness.next(|message| matches!(message, Message::Answer { id: 4, .. })) {
            Message::Answer {
                answer: Answer::SessionList { sessions },
                ..
            } => {
                assert_eq!(sessions.len(), 1);
                assert_eq!(sessions[0].name.as_deref(), Some("build"));
                assert_eq!(sessions[0].color, Some(3));
                assert_eq!((sessions[0].tabs.len(), sessions[0].panes), (1, 1));
            }
            other => panic!("{other:?}"),
        }
        // Attaching again, the name comes after the tabs.
        harness.request(Request::Detach { session });
        harness.request(Request::Attach { session });
        harness.next(|message| matches!(message, Message::Attached { .. }));
        match harness.next(|message| matches!(message, Message::SessionNamed { .. })) {
            Message::SessionNamed { name, .. } => assert_eq!(name.as_deref(), Some("build")),
            _ => unreachable!(),
        }
        match harness.next(|message| matches!(message, Message::SessionColored { .. })) {
            Message::SessionColored { color, .. } => assert_eq!(color, Some(3)),
            _ => unreachable!(),
        }
        harness.request(Request::CloseSession { session });
    }

    #[test]
    fn the_next_window_on_a_session_hears_what_the_last_one_left() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        let tab = harness.open(session, &["sh", "-c", "sleep 30"]);
        harness.request(Request::RememberSession {
            session,
            tab: Some(tab),
            profile: Some("work".to_owned()),
        });
        harness.request(Request::Detach { session });
        harness.request(Request::Attach { session });
        match harness.next(|message| matches!(message, Message::SessionRemembered { .. })) {
            Message::SessionRemembered {
                tab: remembered,
                profile,
                ..
            } => {
                assert_eq!(remembered, Some(tab));
                assert_eq!(profile.as_deref(), Some("work"));
            }
            _ => unreachable!(),
        }
        harness.request(Request::CloseSession { session });
    }

    #[test]
    fn a_client_from_before_panes_sees_a_tab_as_its_first_pane() {
        const OLDER: ClientId = ClientId(2);
        let mut harness = harness();
        let (writer, older) = outbox();
        harness.server.handle(Input::Connected {
            client: OLDER,
            version: 1,
            writer,
        });
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        harness
            .server
            .handle(Input::Request(OLDER, Request::Attach { session }));
        let first = harness.open(session, &["sh", "-c", "sleep 30"]);
        let second = harness.split(first, &["sh", "-c", "sleep 30"]);
        assert_eq!(harness.panes(), [first, second]);
        harness.request(Request::ClosePane { terminal: first });
        assert_eq!(harness.panes(), [second]);

        let told: Vec<Message> = older
            .try_iter()
            .map(|bytes| {
                caret_protocol::receive::<Message>(&mut bytes.as_slice())
                    .unwrap()
                    .unwrap()
            })
            .collect();
        assert!(
            !told
                .iter()
                .any(|message| matches!(message, Message::Layout { .. }))
        );
        let tabs: Vec<(bool, TerminalId)> = told
            .iter()
            .filter_map(|message| match message {
                Message::TabOpened { tab, .. } => Some((true, tab.terminal)),
                Message::TabClosed { terminal, .. } => Some((false, *terminal)),
                _ => None,
            })
            .collect();
        assert_eq!(tabs, [(true, first), (true, second), (false, first)]);
        harness.request(Request::CloseSession { session });
    }

    /// A deterministic run of what clients might ask, sensible or not.
    struct Chaos {
        state: u64,
        terminals: Vec<TerminalId>,
        sessions: Vec<SessionId>,
    }

    impl Chaos {
        fn next(&mut self) -> u64 {
            self.state ^= self.state >> 12;
            self.state ^= self.state << 25;
            self.state ^= self.state >> 27;
            self.state.wrapping_mul(0x2545_f491_4f6c_dd1d)
        }

        fn below(&mut self, limit: usize) -> usize {
            (self.next() >> 33) as usize % limit
        }

        fn terminal(&mut self) -> TerminalId {
            let known = self.terminals.len();
            if known > 0 && self.below(8) > 0 {
                let at = self.below(known);
                self.terminals[at]
            } else {
                TerminalId(self.below(40) as u32)
            }
        }

        fn session(&mut self) -> SessionId {
            let known = self.sessions.len();
            if known > 0 && self.below(8) > 0 {
                let at = self.below(known);
                self.sessions[at]
            } else {
                SessionId(self.below(10) as u32)
            }
        }

        fn command(&mut self) -> Command {
            let program: &[&str] = match self.below(6) {
                0 => &["/no/such/program"],
                1 => &["sh", "-c", "exit 3"],
                2 => &[
                    "sh",
                    "-c",
                    "printf '\\033]0;title\\007\\033]9;note\\007\\033[6n'; cat",
                ],
                _ => &["cat"],
            };
            Command {
                argv: program.iter().map(|&arg| arg.to_owned()).collect(),
                hold: self.below(2) == 0,
                ..Command::default()
            }
        }

        fn size(&mut self) -> Size {
            let far = [0, 1, 2, 5, 40, 200];
            Size {
                columns: far[self.below(far.len())],
                lines: far[self.below(far.len())],
                cell_width: 8,
                cell_height: 16,
            }
        }

        fn plan(&mut self, depth: usize) -> Plan {
            match self.below(4) {
                0 | 1 if depth < 3 => Plan::Split {
                    vertical: self.below(2) == 0,
                    share: self.next() as u16,
                    first: Box::new(self.plan(depth + 1)),
                    second: Box::new(self.plan(depth + 1)),
                },
                2 => Plan::Kept,
                _ => Plan::Run {
                    command: self.command(),
                    size: self.size(),
                },
            }
        }

        fn question(&mut self) -> Question {
            let terminal = self.terminal();
            let session = self.session();
            match self.below(24) {
                0 => Question::Sessions,
                1 => Question::SessionList,
                2 => Question::Programs {
                    sessions: vec![session],
                },
                3 => Question::NewSession,
                4 => Question::SelectionText { terminal },
                5 => Question::LinkAt {
                    terminal,
                    point: caret_protocol::Point {
                        line: self.next() as u16,
                        column: self.next() as u16,
                    },
                },
                6 => Question::Find {
                    terminal,
                    text: "a".to_owned(),
                    upwards: self.below(2) == 0,
                },
                7 => Question::Busy { terminal },
                8 => Question::Foreground { terminal },
                9 => Question::Directory { terminal },
                10 => Question::NextCommand { terminal },
                11..=16 => Question::OpenTab {
                    session,
                    index: self.next() as u32,
                    command: self.command(),
                    size: self.size(),
                },
                17..=21 => Question::Split {
                    terminal,
                    vertical: self.below(2) == 0,
                    command: self.command(),
                    size: self.size(),
                },
                _ => Question::Arrange {
                    terminal,
                    plan: self.plan(0),
                },
            }
        }

        fn request(&mut self) -> Request {
            let terminal = self.terminal();
            let other = self.terminal();
            let session = self.session();
            if self.sessions.is_empty() {
                return Request::Ask {
                    id: 1,
                    question: Question::NewSession,
                };
            }
            match self.below(48) {
                0..=25 => Request::Ask {
                    id: self.next() as u32,
                    question: self.question(),
                },
                26..=30 => Request::Attach { session },
                31 => Request::Detach { session },
                32 => Request::CloseTab { terminal },
                33 => Request::ClosePane { terminal },
                34 => Request::Divide {
                    terminal,
                    share: self.next() as u16,
                },
                35 => Request::SwapPanes { terminal, other },
                36 => Request::MovePane {
                    terminal,
                    beside: other,
                    side: caret_protocol::layout::Side::Below,
                },
                37 => Request::PaneToTab { terminal },
                38 => Request::MoveTab {
                    terminal,
                    session,
                    index: self.next() as u32,
                },
                39 => Request::Resize {
                    terminal,
                    size: self.size(),
                },
                40 => Request::Frame { terminal },
                41 => Request::Input {
                    terminal,
                    bytes: Bytes(b"typed\n".to_vec()),
                    bottom: self.below(2) == 0,
                },
                42 => Request::Scroll {
                    terminal,
                    scroll: Scroll::Lines {
                        lines: self.next() as i32,
                    },
                },
                43 => Request::Focus {
                    terminal,
                    focused: self.below(2) == 0,
                },
                44 if self.sessions.len() > 2 => Request::CloseSession { session },
                45 => Request::Select {
                    terminal,
                    select: caret_protocol::Select::Start {
                        kind: caret_protocol::SelectionKind::Word,
                        point: caret_protocol::Point {
                            line: self.next() as u16,
                            column: self.next() as u16,
                        },
                        right: false,
                    },
                },
                46 => Request::Hover {
                    terminal,
                    point: Some(caret_protocol::Point {
                        line: self.next() as u16,
                        column: self.next() as u16,
                    }),
                },
                _ => Request::Annotate {
                    terminal,
                    text: "note".to_owned(),
                },
            }
        }
    }

    /// What the rest of the server takes for granted about its state.
    fn check_state(server: &Server, seed: u64) {
        let mut seen = BTreeSet::new();
        for (id, session) in &server.sessions {
            for tab in &session.tabs {
                for pane in tab.root.panes() {
                    assert!(seen.insert(pane), "seed {seed}: {pane:?} is in two tabs");
                    let entry = server.terminals.get(&pane);
                    assert!(entry.is_some(), "seed {seed}: {pane:?} has no terminal");
                    assert_eq!(entry.map(|entry| entry.session), Some(*id), "seed {seed}");
                }
                assert!(tab.root.panes().len() <= MOST_PANES, "seed {seed}");
            }
            for client in &session.clients {
                assert!(
                    server
                        .clients
                        .get(client)
                        .is_some_and(|state| state.sessions.contains(id)),
                    "seed {seed}: {client:?} is not told it is in {id:?}"
                );
            }
        }
        for terminal in server.terminals.keys() {
            assert!(
                seen.contains(terminal),
                "seed {seed}: {terminal:?} is in no tab"
            );
        }
        for state in server.clients.values() {
            for session in &state.sessions {
                assert!(server.sessions.contains_key(session), "seed {seed}");
            }
        }
    }

    #[test]
    fn a_session_its_client_never_used_goes_with_the_client() {
        let mut harness = harness();
        harness.new_session();
        assert_eq!(harness.server.sessions.len(), 1);
        harness.server.handle(Input::Disconnected(CLIENT));
        assert!(harness.server.sessions.is_empty());
    }

    #[test]
    fn a_session_with_a_tab_stays_when_the_client_that_asked_for_it_goes() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        harness.open(session, &["cat"]);
        harness.server.handle(Input::Disconnected(CLIENT));
        assert_eq!(harness.server.sessions.len(), 1);
    }

    #[test]
    fn a_program_that_closes_its_terminal_and_opens_it_again_is_still_shown() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        let script = "exec 0<&- 1>&- 2>&-; sleep 0.4; echo back > /dev/tty; sleep 20";
        let terminal = harness.open(session, &["sh", "-c", script]);
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            harness.request(Request::Frame { terminal });
            let Message::Frame { frame, .. } =
                harness.next(|message| matches!(message, Message::Frame { .. }))
            else {
                panic!("not a frame");
            };
            let shown = frame.rows.iter().any(|row| {
                row.cells.iter().any(
                    |cell| matches!(&cell.glyph, Some(glyph) if glyph.text == GlyphText::Char('b')),
                )
            });
            if shown {
                break;
            }
            assert!(Instant::now() < deadline, "what it wrote never showed");
        }
    }

    #[test]
    fn what_is_typed_before_a_program_has_started_reaches_it() {
        let mut harness = harness();
        let session = harness.new_session();
        harness.request(Request::Attach { session });
        let terminal = harness.open(session, &["sh", "-c", "read line; exit $line"]);
        harness.request(Request::Input {
            terminal,
            bytes: Bytes(b"0\n".to_vec()),
            bottom: false,
        });
        harness.next(|message| matches!(message, Message::TabClosed { .. }));
    }

    #[test]
    fn random_clients_leave_the_server_whole() {
        for seed in 1..=8u64 {
            let mut chaos = Chaos {
                state: seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1,
                terminals: Vec::new(),
                sessions: Vec::new(),
            };
            let mut harness = harness();
            let (writer, _older_sent) = outbox();
            let older = ClientId(2);
            harness.server.handle(Input::Connected {
                client: older,
                version: 1,
                writer,
            });
            for step in 0..400 {
                let request = chaos.request();
                let client = if chaos.below(5) == 0 { older } else { CLIENT };
                harness.server.handle(Input::Request(client, request));
                while let Ok(input) = harness.inputs.try_recv() {
                    harness.server.handle(input);
                }
                while harness.sent.try_recv().is_ok() {}
                chaos.terminals = harness.server.terminals.keys().copied().collect();
                chaos.terminals.sort();
                chaos.sessions = harness.server.sessions.keys().copied().collect();
                check_state(&harness.server, seed);
                if step % 100 == 99 {
                    harness.server.handle(Input::Disconnected(older));
                    let (writer, _) = outbox();
                    harness.server.handle(Input::Connected {
                        client: older,
                        version: 1,
                        writer,
                    });
                }
            }
            for session in chaos.sessions.clone() {
                harness.request(Request::CloseSession { session });
            }
            check_state(&harness.server, seed);
        }
    }
}
