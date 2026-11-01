//! A session drawn in the terminal Caret runs in, with escape sequences: its tabs' panes
//! side by side as the server lays them out, a line of tabs under them, and the keys
//! going to the focused pane. Ctrl+Shift+Space starts control mode where the terminal
//! tells it apart, Ctrl+] elsewhere; its keys move between panes and tabs, move, resize,
//! split, close and zoom panes, and detach. The mouse focuses panes, picks tabs, drags
//! dividers and, with Alt or in control mode, moves panes.

mod canvas;
mod host;
mod status;

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::panic;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::time::Duration;

use caret_protocol::encode::{self, Key, Modifiers, MouseAction, MouseButton, Press};
use caret_protocol::layout::Node;
use caret_protocol::{
    Answer, Bytes, Cell, Command, CursorShape, Frame, GlyphText, Message, Mode, Point, Question,
    Request, Rgb, Scroll, SessionId, Size, Tab, TerminalId,
};
use rustix::termios::{self, OptionalActions};

use gettextrs::gettext;

use crate::app::sessions::session_color;
use crate::config::Config;
use crate::i18n::gettext_f;
use crate::panes::{self, Direction, Divider, Rect};
use canvas::{Canvas, Colors, Pen};
use host::{Host, Input, Mouse};

/// The most events taken in before a frame is drawn, so that a busy server cannot keep
/// the screen from being written.
const EVENTS_PER_FRAME: usize = 256;
/// The most of a tab's or session's name the bar shows, in characters.
const TAB_TITLE: usize = 20;
const BAR_PEN: Pen = Pen {
    fg: None,
    bg: None,
    style: canvas::REVERSE,
};
const KEY_PEN: Pen = Pen {
    fg: None,
    bg: None,
    style: canvas::REVERSE | caret_protocol::BOLD,
};

/// A key and what it does, as the bar shows them.
fn bar_hint(keys: &str, action: &str) -> [status::Span; 2] {
    [
        (format!(" {keys} "), KEY_PEN),
        (format!("{action} "), BAR_PEN),
    ]
}

/// Shows `session` in this terminal until it is detached or ends.
///
/// # Errors
///
/// When standard input is not a terminal, or the server cannot be reached.
pub fn run(session: SessionId) -> Result<(), String> {
    let stdin = io::stdin();
    let saved =
        termios::tcgetattr(&stdin).map_err(|error| format!("reading the terminal: {error}"))?;
    let (mut stream, _) = caret_protocol::client::connect(crate::APP_ID, caret_protocol::NEWEST)
        .map_err(|error| error.to_string())?;
    let mut raw = saved.clone();
    raw.make_raw();
    termios::tcsetattr(&stdin, OptionalActions::Now, &raw)
        .map_err(|error| format!("setting the terminal up: {error}"))?;
    let mut host = host::probe();
    host.console = std::env::var("TERM").is_ok_and(|term| term.starts_with("linux"));
    let restore = Restore {
        saved,
        enhanced: host.enhanced,
        console: host.console,
        done: Arc::default(),
    };
    restore.before_a_panics_message();
    let _restore = restore;
    // The other screen, the cursor hidden until a pane shows its own, mouse reports while
    // a button is held, and keys written apart where the terminal can.
    // Told when its colours go from dark to light or back.
    print!("\x1b[?1049h\x1b[?25l\x1b[?1002h\x1b[?1006h\x1b[?2031h");
    if host.enhanced {
        print!("\x1b[>1u");
    }
    show(&mut stream, session, host)
}

/// Gives the user back the terminal as it was, however the client ends. Otherwise a panic
/// leaves a shell with no echo, on the other screen, reporting the mouse.
#[derive(Clone)]
struct Restore {
    saved: termios::Termios,
    enhanced: bool,
    console: bool,
    done: Arc<AtomicBool>,
}

impl Restore {
    /// A message written on the other screen would go with it the moment the client left.
    fn before_a_panics_message(&self) {
        let restore = self.clone();
        let message = panic::take_hook();
        panic::set_hook(Box::new(move |info| {
            if std::thread::current().name() == Some("main") {
                restore.put_back();
            }
            message(info);
        }));
    }

    fn put_back(&self) {
        if self.done.swap(true, Ordering::Relaxed) {
            return;
        }
        let mut text = String::new();
        text.push_str("\x1b[?2031l");
        if self.enhanced {
            text.push_str("\x1b[<u");
        }
        // A console has no other screen to go back from.
        text.push_str(if self.console {
            "\x1b[?0c\x1b[0m\x1b[2J\x1b[H"
        } else {
            "\x1b[0 q"
        });
        text.push_str("\x1b[?1006l\x1b[?1002l\x1b[0m\x1b[?25h\x1b[?1049l");
        // Not `print!`, which panics when the terminal has gone.
        let mut out = io::stdout();
        let _ = out.write_all(text.as_bytes());
        let _ = out.flush();
        let _ = termios::tcsetattr(io::stdin(), OptionalActions::Now, &self.saved);
    }
}

impl Drop for Restore {
    fn drop(&mut self) {
        self.put_back();
    }
}

/// Ends the client in order on SIGTERM and SIGHUP. Called before any thread that should
/// leave them to the one this starts, which inherits the mask.
#[expect(unsafe_code, reason = "signal masks are only reached through libc")]
fn close_on_signals(events: mpsc::Sender<Event>) {
    // SAFETY: an all-zero sigset_t is a valid value for sigemptyset to fill in.
    let mut signals: libc::sigset_t = unsafe { std::mem::zeroed() };
    // SAFETY: each call takes the live set above, and the old mask may be null.
    unsafe {
        libc::sigemptyset(&raw mut signals);
        libc::sigaddset(&raw mut signals, libc::SIGTERM);
        libc::sigaddset(&raw mut signals, libc::SIGHUP);
        libc::pthread_sigmask(libc::SIG_BLOCK, &raw const signals, std::ptr::null_mut());
    }
    std::thread::spawn(move || {
        let mut signal = 0;
        // SAFETY: both pointers are to live values for the length of the call.
        if unsafe { libc::sigwait(&raw const signals, &raw mut signal) } == 0 {
            let _ = events.send(Event::Stopped);
        }
    });
}

enum Event {
    Message(Message),
    Typed(Vec<u8>),
    Closed,
    /// A signal asked the client to end.
    Stopped,
}

struct Screen {
    columns: u16,
    lines: u16,
    rows: Vec<Vec<Cell>>,
    background: Rgb,
    cursor: Option<Point>,
    cursor_shape: CursorShape,
    cursor_blinks: bool,
    mode: Mode,
}

impl Screen {
    fn apply(&mut self, frame: Frame) {
        if (frame.columns, frame.lines) != (self.columns, self.lines) {
            self.columns = frame.columns;
            self.lines = frame.lines;
            self.rows = vec![Vec::new(); usize::from(frame.lines)];
        }
        for row in frame.rows {
            if let Some(line) = self.rows.get_mut(usize::from(row.line)) {
                *line = row.cells;
            }
        }
        self.background = frame.background;
        self.cursor = frame
            .cursor
            .map(|cursor| cursor.point)
            .filter(|point| point.line < frame.lines);
        if let Some(cursor) = frame.cursor {
            self.cursor_shape = cursor.shape;
        }
        self.cursor_blinks = frame.cursor_blinks;
        self.mode = frame.mode;
    }
}

struct TextTab {
    key: TerminalId,
    root: Node,
    focused: TerminalId,
    title: String,
    name: Option<String>,
}

struct Client {
    writer: UnixStream,
    session: SessionId,
    session_name: Option<String>,
    /// The colour picked for the session.
    session_color: Option<u8>,
    config: Config,
    tabs: Vec<TextTab>,
    selected: usize,
    screens: HashMap<TerminalId, Screen>,
    titles: HashMap<TerminalId, String>,
    /// Why terminals failed, to say in a pane that never shows anything else.
    failures: HashMap<TerminalId, String>,
    /// Its size in cells, and in pixels where the terminal says.
    size: (u16, u16, u16, u16),
    sizes: HashMap<TerminalId, Size>,
    /// The terminal it runs in writes keys apart: Ctrl+Shift+Space starts control mode.
    enhanced: bool,
    control: bool,
    zoomed: bool,
    asked: u32,
    /// The pane split off, to give the keys once the server lays it out.
    opened: Option<TerminalId>,
    /// The pane taken out to a tab of its own, to show once the tab is there.
    moved: Option<TerminalId>,
    drag: Option<Drag>,
    /// What the terminal shows, and the frame being made from what is known now.
    shown: Canvas,
    next: Canvas,
    shown_cursor: Option<TextCursor>,
    colors: Colors,
    /// What the terminal this runs in said of itself, to tell the server its colours.
    host: Host,
}

/// The cursor of the focused pane: its line and column from one, and how it looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TextCursor {
    line: i32,
    column: i32,
    shape: CursorShape,
    blinks: bool,
}

impl TextCursor {
    /// Sets how it looks: a console has its own sequence and no blinking to choose.
    fn write_look(self, colors: Colors, out: &mut String) {
        let _ = match colors {
            Colors::Console => {
                let size = if self.shape == CursorShape::Underline {
                    2
                } else {
                    6
                };
                write!(out, "\x1b[?{size}c")
            }
            Colors::True => {
                let steady = match self.shape {
                    CursorShape::Block | CursorShape::HollowBlock => 2,
                    CursorShape::Underline => 4,
                    CursorShape::Beam => 6,
                };
                write!(out, "\x1b[{} q", steady - i32::from(self.blinks))
            }
        };
    }
}

/// What a held mouse button moves.
#[derive(Clone)]
enum Drag {
    Dividers(Vec<Divider>),
    Pane(TerminalId),
    /// The program asked for the mouse.
    Program(TerminalId),
}

fn show(stream: &mut UnixStream, session: SessionId, host: Host) -> Result<(), String> {
    let writer = stream.try_clone().map_err(|error| error.to_string())?;
    let (events, received) = mpsc::channel();
    close_on_signals(events.clone());
    let mut reader = stream.try_clone().map_err(|error| error.to_string())?;
    let from_server = events.clone();
    std::thread::spawn(move || {
        while let Ok(Some(message)) = caret_protocol::receive::<Message>(&mut reader) {
            if from_server.send(Event::Message(message)).is_err() {
                return;
            }
        }
        let _ = from_server.send(Event::Closed);
    });
    std::thread::spawn(move || {
        let mut typed = [0; 4096];
        while let Ok(read) = io::stdin().read(&mut typed) {
            if read == 0 || events.send(Event::Typed(typed[..read].to_vec())).is_err() {
                return;
            }
        }
    });
    let mut client = Client {
        writer,
        session,
        tabs: Vec::new(),
        selected: 0,
        screens: HashMap::new(),
        titles: HashMap::new(),
        failures: HashMap::new(),
        size: window_size(),
        sizes: HashMap::new(),
        enhanced: host.enhanced,
        session_name: None,
        session_color: None,
        config: Config::load(None),
        control: false,
        zoomed: false,
        asked: 0,
        opened: None,
        moved: None,
        drag: None,
        shown: Canvas::default(),
        next: Canvas::default(),
        shown_cursor: None,
        colors: if host.console {
            Colors::Console
        } else {
            Colors::True
        },
        host,
    };
    let typed = std::mem::take(&mut client.host.typed);
    let looks = look(&client.host);
    client.send(&looks);
    client.send(&Request::Attach { session });
    if !typed.is_empty() {
        client.typed(&typed);
    }
    loop {
        let first = match received.recv_timeout(Duration::from_millis(250)) {
            Ok(event) => event,
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let size = window_size();
                if size != client.size {
                    client.size = size;
                    client.resize();
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        };
        // All that has come goes into one frame, however many events it took.
        let waiting = std::iter::from_fn(|| received.try_recv().ok()).take(EVENTS_PER_FRAME);
        for event in std::iter::once(first).chain(waiting) {
            if !client.event(event)? {
                return Ok(());
            }
        }
        client.draw();
    }
}

/// The colours of the terminal it runs in, the configured ones of its style where it does
/// not tell, and the configured options.
fn look(host: &Host) -> Request {
    let config = Config::load(None);
    let dark = host.background.is_none_or(|background| {
        u32::from(background.r) * 299
            + u32::from(background.g) * 587
            + u32::from(background.b) * 114
            < 128_000
    });
    let scheme = if dark {
        &config.dark_scheme
    } else {
        &config.light_scheme
    };
    let mut palette = crate::palette::Palette::named(scheme, dark).colors();
    if host.console {
        palette.ansi = canvas::CONSOLE_COLORS.to_vec();
        palette.foreground = canvas::CONSOLE_COLORS[7];
        palette.background = canvas::CONSOLE_COLORS[0];
    }
    palette.foreground = host.foreground.unwrap_or(palette.foreground);
    palette.background = host.background.unwrap_or(palette.background);
    for (color, told) in palette.ansi.iter_mut().zip(host.ansi) {
        *color = told.unwrap_or(*color);
    }
    Request::Look {
        palette,
        options: crate::app::options(&config),
    }
}

/// The terminal's size in cells and pixels.
fn window_size() -> (u16, u16, u16, u16) {
    termios::tcgetwinsize(io::stdout()).map_or((80, 24, 0, 0), |size| {
        (size.ws_col, size.ws_row, size.ws_xpixel, size.ws_ypixel)
    })
}

impl Client {
    fn send(&mut self, request: &Request) {
        let _ = caret_protocol::send(&mut self.writer, request);
    }

    fn tab(&self) -> Option<&TextTab> {
        self.tabs.get(self.selected)
    }

    /// The panes' room: all but the line of tabs.
    fn area(&self) -> Rect {
        let (columns, lines, ..) = self.size;
        Rect {
            x: 0,
            y: 0,
            width: i32::from(columns),
            height: i32::from(lines.saturating_sub(1)),
        }
    }

    fn placed(&self) -> panes::Placed {
        match self.tab() {
            Some(tab) if self.zoomed => panes::place(
                &Node::Pane {
                    terminal: tab.focused,
                },
                self.area(),
                1,
            ),
            Some(tab) => panes::place(&tab.root, self.area(), 1),
            None => panes::Placed::default(),
        }
    }

    /// Tells the server the size of each pane shown that changed, and asks for frames.
    fn resize(&mut self) {
        let (columns, lines, x_pixels, y_pixels) = self.size;
        let cell_width = x_pixels.checked_div(columns).unwrap_or(0);
        let cell_height = y_pixels.checked_div(lines).unwrap_or(0);
        for (terminal, rect) in self.placed().panes {
            let size = Size {
                columns: u16::try_from(rect.width.max(1)).unwrap_or(1),
                lines: u16::try_from(rect.height.max(1)).unwrap_or(1),
                cell_width,
                cell_height,
            };
            if self.sizes.get(&terminal) != Some(&size) {
                self.sizes.insert(terminal, size);
                self.send(&Request::Resize { terminal, size });
            }
            self.send(&Request::Frame { terminal });
        }
    }

    /// Takes in what the server says; false once the session is gone.
    fn message(&mut self, message: Message) -> bool {
        match message {
            Message::Attached { session, tabs } if session == self.session => {
                for (index, tab) in tabs.into_iter().enumerate() {
                    self.opened(index, tab);
                }
                self.show_tab(0);
            }
            Message::TabOpened {
                session,
                index,
                tab,
            } if session == self.session => {
                self.opened(index as usize, tab);
                self.resize();
            }
            Message::TabClosed { session, terminal } if session == self.session => {
                if let Some(index) = self.tabs.iter().position(|tab| tab.key == terminal) {
                    self.tabs.remove(index);
                    self.show_tab(self.selected.min(self.tabs.len().saturating_sub(1)));
                }
            }
            Message::TabMoved {
                session,
                terminal,
                index,
            } if session == self.session => {
                if let Some(from) = self.tabs.iter().position(|tab| tab.key == terminal) {
                    let tab = self.tabs.remove(from);
                    let index = (index as usize).min(self.tabs.len());
                    self.tabs.insert(index, tab);
                }
            }
            Message::TabNamed { terminal, name } => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.key == terminal) {
                    tab.name = name;
                }
            }
            Message::Layout { session, tab, root } if session == self.session => {
                self.laid_out(tab, root);
            }
            Message::SessionClosed { session } if session == self.session => return false,
            Message::DetachSession { session } if session == self.session => {
                self.send(&Request::Detach { session });
                return false;
            }
            Message::Title { terminal, title } => {
                if let Some(tab) = self.tabs.iter_mut().find(|tab| tab.key == terminal) {
                    tab.title.clone_from(&title);
                }
                self.titles.insert(terminal, title);
            }
            Message::Frame { terminal, frame } => {
                let screen = self.screens.entry(terminal).or_insert_with(|| Screen {
                    columns: 0,
                    lines: 0,
                    rows: Vec::new(),
                    background: Rgb::default(),
                    cursor: None,
                    cursor_shape: CursorShape::default(),
                    cursor_blinks: false,
                    mode: Mode::default(),
                });
                screen.apply(frame);
                if self.placed().rect(terminal).is_some() {
                    self.send(&Request::Frame { terminal });
                }
            }
            Message::Failed { terminal, reason } => {
                self.failures.insert(terminal, reason);
            }
            Message::SessionNamed { session, name } if session == self.session => {
                self.session_name = name;
            }
            Message::SessionColored { session, color } if session == self.session => {
                self.session_color = color;
            }
            Message::Answer {
                answer: Answer::Opened { terminal },
                ..
            } => self.opened = Some(terminal),
            Message::Clipboard { text, .. } => {
                // Passed on to the terminal this runs in, as the program asked of its own.
                print!("\x1b]52;c;{}\x07", base64(text.as_bytes()));
            }
            _ => {}
        }
        true
    }

    /// Takes in how the panes of tab `tab` are laid out now.
    fn laid_out(&mut self, tab: TerminalId, root: Node) {
        if let Some(state) = self.tabs.iter_mut().find(|state| state.key == tab) {
            if let Some(opened) = self.opened.filter(|&pane| root.contains(pane)) {
                state.focused = opened;
                self.opened = None;
            } else if !root.contains(state.focused) {
                state.focused = root.first();
            }
            state.root = root;
        }
        self.zoomed = false;
        let moved = self.moved.map(|terminal| Node::Pane { terminal });
        if let Some(index) = self
            .tabs
            .iter()
            .position(|state| Some(&state.root) == moved.as_ref())
        {
            self.moved = None;
            self.show_tab(index);
        }
        self.resize();
    }

    fn opened(&mut self, index: usize, tab: Tab) {
        let index = index.min(self.tabs.len());
        if index <= self.selected && !self.tabs.is_empty() {
            self.selected += 1;
        }
        self.tabs.insert(
            index,
            TextTab {
                key: tab.terminal,
                root: Node::Pane {
                    terminal: tab.terminal,
                },
                focused: tab.terminal,
                title: tab.title,
                name: tab.name,
            },
        );
    }

    fn show_tab(&mut self, index: usize) {
        if index < self.tabs.len() {
            self.selected = index;
            self.zoomed = false;
            if let Some(terminal) = self.tab().map(|tab| tab.focused) {
                self.send(&Request::Focus {
                    terminal,
                    focused: true,
                });
            }
            self.resize();
        }
    }

    /// Passes typed bytes on, or acts on them in control mode; false to detach.
    fn typed(&mut self, bytes: &[u8]) -> bool {
        let mut rest = bytes;
        while !rest.is_empty() {
            let (input, used) = host::next(rest, self.enhanced);
            rest = &rest[used.max(1).min(rest.len())..];
            match input {
                Input::Control => self.control = !self.control,
                Input::SchemeChanged => host::ask_again(),
                Input::Color(answer) => self.host.take_color(answer),
                Input::Answered => {
                    let looks = look(&self.host);
                    self.send(&looks);
                }
                Input::Mouse(mouse) => self.mouse(mouse),
                Input::Key(key, modifiers) if self.control => {
                    let bytes = encode::key(key, modifiers, Mode::default()).unwrap_or_default();
                    if !self.control_keys(&bytes) {
                        return false;
                    }
                }
                Input::Bytes(bytes) if self.control => {
                    if !self.control_keys(bytes) {
                        return false;
                    }
                }
                Input::Key(key, modifiers) => self.key(key, modifiers),
                Input::Bytes(bytes) => {
                    if let Some(terminal) = self.focused() {
                        self.input(terminal, bytes.to_vec(), true);
                    }
                }
            }
        }
        true
    }

    fn focused(&self) -> Option<TerminalId> {
        self.tab().map(|tab| tab.focused)
    }

    fn mode(&self, terminal: TerminalId) -> Mode {
        self.screens
            .get(&terminal)
            .map_or(Mode::default(), |screen| screen.mode)
    }

    fn input(&mut self, terminal: TerminalId, bytes: Vec<u8>, bottom: bool) {
        if !bytes.is_empty() {
            self.send(&Request::Input {
                terminal,
                bytes: Bytes(bytes),
                bottom,
            });
        }
    }

    /// A key the terminal wrote apart, encoded as the focused pane's program wants it.
    fn key(&mut self, key: Key, modifiers: Modifiers) {
        let Some(terminal) = self.focused() else {
            return;
        };
        let text = match key {
            Key::Char(c) if !modifiers.ctrl && !modifiers.alt => Some(c.to_string()),
            _ => None,
        };
        let mode = self.mode(terminal);
        if let Some(bytes) = encode::key_event(key, modifiers, Press::Down, text.as_deref(), mode) {
            self.input(terminal, bytes, true);
        }
    }

    /// Does what keys say in control mode, an escape sequence whole or else each byte;
    /// false to detach. A key it does not know leaves the mode, and the rest are dropped.
    fn control_keys(&mut self, bytes: &[u8]) -> bool {
        if bytes.first() == Some(&0x1b) {
            match arrow(bytes) {
                Some((direction, 1)) => self.focus_towards(direction),
                Some((direction, 2)) => self.swap_towards(direction),
                Some((direction, 5)) => self.resize_towards(direction),
                _ => self.control = false,
            }
            return true;
        }
        for &byte in bytes {
            match byte {
                b'h' => self.focus_towards(Direction::Left),
                b'j' => self.focus_towards(Direction::Down),
                b'k' => self.focus_towards(Direction::Up),
                b'l' => self.focus_towards(Direction::Right),
                b'H' => self.swap_towards(Direction::Left),
                b'J' => self.swap_towards(Direction::Down),
                b'K' => self.swap_towards(Direction::Up),
                b'L' => self.swap_towards(Direction::Right),
                b'n' => self.step_tab(1),
                b'p' => self.step_tab(self.tabs.len().saturating_sub(1)),
                b'v' => self.split(true),
                b's' => self.split(false),
                b'x' => {
                    if let Some(terminal) = self.focused() {
                        self.send(&Request::ClosePane { terminal });
                    }
                }
                b'z' => {
                    self.zoomed = !self.zoomed;
                    self.resize();
                }
                b't' => {
                    if let Some(terminal) = self.focused() {
                        self.pane_to_tab(terminal);
                    }
                }
                b'd' => return false,
                _ => {
                    self.control = false;
                    return true;
                }
            }
        }
        true
    }

    fn pane_to_tab(&mut self, terminal: TerminalId) {
        if self.tab().is_some_and(|tab| tab.root.panes().len() > 1) {
            self.moved = Some(terminal);
            self.send(&Request::PaneToTab { terminal });
        }
    }

    fn swap_towards(&mut self, direction: Direction) {
        let Some(terminal) = self.focused() else {
            return;
        };
        if let Some(other) = panes::neighbour(&self.placed(), terminal, direction) {
            self.send(&Request::SwapPanes { terminal, other });
        }
    }

    fn resize_towards(&mut self, direction: Direction) {
        let Some(terminal) = self.focused() else {
            return;
        };
        if let Some((divider, share)) =
            panes::resized(&self.placed(), terminal, direction, (1.0, 1.0), 1)
        {
            self.send(&Request::Divide {
                terminal: divider.before,
                share,
            });
        }
    }

    fn focus(&mut self, terminal: TerminalId) {
        if let Some(tab) = self.tabs.get_mut(self.selected)
            && tab.focused != terminal
        {
            tab.focused = terminal;
            self.send(&Request::Focus {
                terminal,
                focused: true,
            });
        }
    }

    fn mouse(&mut self, mouse: Mouse) {
        let point = (f64::from(mouse.column) + 0.5, f64::from(mouse.line) + 0.5);
        let placed = self.placed();
        let left = mouse.button == MouseButton::Left;
        let on_tabs = mouse.line + 1 >= self.size.1;
        match mouse.action {
            MouseAction::Press
                if matches!(mouse.button, MouseButton::WheelUp | MouseButton::WheelDown) =>
            {
                if let Some(terminal) = placed.pane_at(point) {
                    self.wheel(terminal, mouse);
                }
            }
            MouseAction::Press if on_tabs => {
                if let Some(index) = self.tab_at(mouse.column).filter(|_| left && !self.control) {
                    self.show_tab(index);
                }
            }
            MouseAction::Press => {
                let dividers = placed.dividers_at(point, 0.0);
                if left && !dividers.is_empty() {
                    self.drag = Some(Drag::Dividers(dividers));
                    return;
                }
                let Some(terminal) = placed.pane_at(point) else {
                    return;
                };
                self.focus(terminal);
                if left && (self.control || mouse.modifiers.alt) {
                    self.drag = Some(Drag::Pane(terminal));
                } else if self.report(terminal, mouse) {
                    self.drag = Some(Drag::Program(terminal));
                }
            }
            MouseAction::Motion => match self.drag.clone() {
                Some(Drag::Dividers(dividers)) => {
                    for divider in dividers {
                        self.send(&Request::Divide {
                            terminal: divider.before,
                            share: divider.share_under(point, 1),
                        });
                    }
                }
                Some(Drag::Program(terminal)) => {
                    self.report(terminal, mouse);
                }
                _ => {}
            },
            MouseAction::Release => match self.drag.take() {
                Some(Drag::Pane(terminal)) if on_tabs => self.pane_to_tab(terminal),
                Some(Drag::Pane(terminal)) => {
                    if let Some(target) = placed.pane_at(point).filter(|&target| target != terminal)
                        && let Some(rect) = placed.rect(target)
                    {
                        self.send(&match panes::side_near(rect, point) {
                            Some(side) => Request::MovePane {
                                terminal,
                                beside: target,
                                side,
                            },
                            None => Request::SwapPanes {
                                terminal,
                                other: target,
                            },
                        });
                    }
                }
                Some(Drag::Program(terminal)) => {
                    self.report(terminal, mouse);
                }
                _ => {}
            },
        }
    }

    /// Tells the pane's program of a mouse event, if it asked for them: whether it did.
    fn report(&mut self, terminal: TerminalId, mouse: Mouse) -> bool {
        let Some(rect) = self.placed().rect(terminal) else {
            return false;
        };
        let column = (i32::from(mouse.column) - rect.x).clamp(0, rect.width - 1);
        let line = (i32::from(mouse.line) - rect.y).clamp(0, rect.height - 1);
        let bytes = encode::mouse(
            mouse.button,
            mouse.action,
            mouse.modifiers,
            usize::try_from(column).unwrap_or(0),
            usize::try_from(line).unwrap_or(0),
            self.mode(terminal),
        );
        let reported = bytes.is_some();
        if let Some(bytes) = bytes {
            self.input(terminal, bytes, false);
        }
        reported
    }

    /// Scrolls the pane under the wheel, as its program or the view takes it.
    fn wheel(&mut self, terminal: TerminalId, mouse: Mouse) {
        const LINES: usize = 3;
        if self.report(terminal, mouse) {
            return;
        }
        let up = mouse.button == MouseButton::WheelUp;
        let mode = self.mode(terminal);
        if mode.contains(Mode::ALT_SCREEN | Mode::ALTERNATE_SCROLL) {
            // Programs on the alternate screen have no history to scroll: they get arrows.
            let key = if up { Key::Up } else { Key::Down };
            if let Some(bytes) = encode::key(key, Modifiers::default(), mode) {
                self.input(terminal, bytes.repeat(LINES), false);
            }
        } else {
            let lines = LINES as i32;
            self.send(&Request::Scroll {
                terminal,
                scroll: Scroll::Lines {
                    lines: if up { lines } else { -lines },
                },
            });
        }
    }

    /// Each tab's part of the bar.
    fn tab_labels(&self) -> Vec<String> {
        self.tabs
            .iter()
            .enumerate()
            .map(|(index, tab)| {
                let title = tab.name.as_deref().unwrap_or(&tab.title);
                let title = status::shortened(title, TAB_TITLE);
                format!(" {} {title} ", index + 1)
            })
            .collect()
    }

    /// The session's part of the bar.
    fn session_label(&self) -> String {
        let number = gettext_f(
            "Session {number}",
            &[("number", &self.session.0.to_string())],
        );
        match &self.session_name {
            Some(name) => format!(" {number} · {} ", status::shortened(name, TAB_TITLE)),
            None => format!(" {number} "),
        }
    }

    /// The tab whose label is at `column` of the bar.
    fn tab_at(&self, column: u16) -> Option<usize> {
        let mut end = self.session_label().chars().count();
        if usize::from(column) < end {
            return None;
        }
        self.tab_labels().iter().position(|label| {
            end += label.chars().count();
            usize::from(column) < end
        })
    }

    /// What marks the session and the focused pane.
    fn session_rgb(&self) -> Rgb {
        let color = if self.config.session_colors {
            session_color(self.session, self.session_color)
        } else {
            crate::chrome::ACCENT
        };
        let [r, g, b, _] = color.to_be_bytes();
        Rgb { r, g, b }
    }

    /// Control mode's bar: its keys go on to lines above the last rather than being cut,
    /// over the panes and without resizing them.
    fn control_bar(&self) -> Vec<Vec<status::Span>> {
        let badge = self.badge_pen();
        let hint = bar_hint;
        status::wrapped(
            vec![(format!(" {} ", gettext("CONTROL")), badge)],
            vec![
                hint(&gettext("arrows"), &gettext("focus")),
                hint(&gettext("shift"), &gettext("swap")),
                hint(&gettext("ctrl"), &gettext("resize")),
                hint("n p", &gettext("tab")),
                hint("v s", &gettext("split")),
                hint("x", &gettext("close")),
                hint("z", &gettext("zoom")),
                hint("t", &gettext("to tab")),
                hint("d", &gettext("detach")),
                hint("Esc", &gettext("leave")),
            ],
            usize::from(self.size.0),
        )
    }

    fn badge_pen(&self) -> Pen {
        Pen {
            fg: Some(Rgb {
                r: 0xff,
                g: 0xff,
                b: 0xff,
            }),
            bg: Some(self.session_rgb()),
            style: caret_protocol::BOLD,
        }
    }

    /// The first screen line the bar takes.
    fn bar_top(&self) -> i32 {
        let lines = i32::from(self.size.1);
        if self.control {
            lines - i32::try_from(self.control_bar().len()).unwrap_or(lines)
        } else {
            lines - 1
        }
    }

    fn compose_bar(&self, canvas: &mut Canvas) {
        let columns = usize::from(self.size.0);
        if self.control {
            for (y, row) in (self.bar_top()..).zip(&self.control_bar()) {
                status::fill(canvas, y, BAR_PEN);
                status::put(canvas, 0, y, row);
            }
            return;
        }
        let (bar, key, badge, hint) = (BAR_PEN, KEY_PEN, self.badge_pen(), bar_hint);
        let y = self.bar_top();
        status::fill(canvas, y, bar);
        let mut left = vec![(self.session_label(), badge)];
        for (index, label) in self.tab_labels().into_iter().enumerate() {
            let selected = Pen {
                style: caret_protocol::BOLD,
                ..Pen::default()
            };
            left.push((
                label,
                if index == self.selected {
                    selected
                } else {
                    bar
                },
            ));
        }
        status::put(canvas, 0, y, &left);
        let control = if self.enhanced {
            "Ctrl+Shift+Space"
        } else {
            "Ctrl+]"
        };
        let mut right = Vec::new();
        if self.zoomed {
            right.push((format!(" {} ", gettext("zoomed")), key));
        }
        right.extend(hint(control, &gettext("control")));
        let room = right
            .iter()
            .map(|(text, _): &status::Span| text.chars().count());
        let x = columns.saturating_sub(room.sum());
        status::put(canvas, i32::try_from(x).unwrap_or(0), y, &right);
    }

    /// Shows the tab `steps` after the one shown, round from the last to the first.
    fn step_tab(&mut self, steps: usize) {
        if !self.tabs.is_empty() {
            self.show_tab((self.selected + steps) % self.tabs.len());
        }
    }

    fn focus_towards(&mut self, direction: Direction) {
        let placed = self.placed();
        let Some(tab) = self.tabs.get_mut(self.selected) else {
            return;
        };
        if let Some(next) = panes::neighbour(&placed, tab.focused, direction) {
            tab.focused = next;
            self.send(&Request::Focus {
                terminal: next,
                focused: true,
            });
        }
    }

    fn split(&mut self, vertical: bool) {
        let Some(terminal) = self.tab().map(|tab| tab.focused) else {
            return;
        };
        let size = self.sizes.get(&terminal).copied().unwrap_or(Size {
            columns: 80,
            lines: 24,
            cell_width: 0,
            cell_height: 0,
        });
        self.asked += 1;
        let question = Question::Split {
            terminal,
            vertical,
            command: Command {
                env: std::env::vars().collect(),
                ..Command::default()
            },
            size,
        };
        let id = self.asked;
        self.send(&Request::Ask { id, question });
    }

    /// Puts the panes of the tab shown, the lines between them and the line of tabs on
    /// the canvas of the next frame.
    fn compose(&mut self, placed: &panes::Placed) {
        let (columns, lines, ..) = self.size;
        let mut next = std::mem::take(&mut self.next);
        next.reset(usize::from(columns), usize::from(lines));
        for &(terminal, rect) in &placed.panes {
            let Some(screen) = self.screens.get(&terminal) else {
                let reason = self.failures.get(&terminal).map_or("", String::as_str);
                for (column, c) in (0..rect.width).zip(reason.chars()) {
                    let text = Some(GlyphText::Char(c));
                    next.put(rect.x + column, rect.y, text, Pen::default(), false);
                }
                continue;
            };
            for line in 0..rect.height {
                let row = screen
                    .rows
                    .get(usize::try_from(line).unwrap_or(usize::MAX))
                    .map_or(&[][..], Vec::as_slice);
                next.put_row((rect.x, rect.y + line), row, rect.width, screen.background);
            }
        }
        let dim = Pen {
            style: canvas::DIM,
            ..Pen::default()
        };
        let mut stretch = |vertical: bool, gap: Rect, pen: Pen| {
            let c = if vertical { '│' } else { '─' };
            for line in 0..gap.height {
                for column in 0..gap.width {
                    let text = Some(GlyphText::Char(c));
                    next.put(gap.x + column, gap.y + line, text, pen, false);
                }
            }
        };
        for divider in &placed.dividers {
            stretch(divider.vertical, divider.gap, dim);
        }
        if let Some(rect) = self.tab().and_then(|tab| placed.rect(tab.focused)) {
            let strong = Pen {
                fg: self.config.active_pane_color.then(|| self.session_rgb()),
                style: caret_protocol::BOLD,
                ..Pen::default()
            };
            // In its own gap, as a line drawn across another would cut it.
            for edge in placed.edges(rect) {
                stretch(edge.vertical, edge.reaching(1, edge.gap), strong);
            }
        }
        self.compose_bar(&mut next);
        self.next = next;
    }

    /// Where the focused pane has its cursor, if it shows one.
    fn cursor(&self, placed: &panes::Placed) -> Option<TextCursor> {
        let tab = self.tab()?;
        let rect = placed.rect(tab.focused)?;
        let screen = self.screens.get(&tab.focused)?;
        let cursor = screen.cursor?;
        // Control mode's bar can cover the line the cursor is on.
        if rect.y + i32::from(cursor.line) >= self.bar_top() {
            return None;
        }
        Some(TextCursor {
            line: rect.y + i32::from(cursor.line) + 1,
            column: rect.x + i32::from(cursor.column) + 1,
            shape: screen.cursor_shape,
            blinks: screen.cursor_blinks,
        })
    }

    /// Writes what changed on the screen since the last time.
    fn draw(&mut self) {
        let placed = self.placed();
        self.compose(&placed);
        let cursor = self.cursor(&placed);
        let mut out = String::new();
        if self.shown.size() != self.next.size() {
            let (columns, lines) = self.next.size();
            self.shown.reset(columns, lines);
            out.push_str("\x1b[0m\x1b[2J");
        }
        self.next.write_changes(&self.shown, self.colors, &mut out);
        if out.is_empty() && cursor == self.shown_cursor {
            std::mem::swap(&mut self.next, &mut self.shown);
            return;
        }
        // The cursor stays hidden while it runs over the cells written.
        let mut frame = String::with_capacity(out.len() + 32);
        if !out.is_empty() {
            frame.push_str("\x1b[?2026h\x1b[?25l");
            frame.push_str(&out);
        }
        match cursor {
            Some(cursor) => {
                let looks = (cursor.shape, cursor.blinks);
                if self.shown_cursor.map(|shown| (shown.shape, shown.blinks)) != Some(looks) {
                    cursor.write_look(self.colors, &mut frame);
                }
                let _ = write!(frame, "\x1b[{};{}H\x1b[?25h", cursor.line, cursor.column);
            }
            None => frame.push_str("\x1b[?25l"),
        }
        if !out.is_empty() {
            frame.push_str("\x1b[?2026l");
        }
        let mut stdout = io::stdout();
        let _ = stdout.write_all(frame.as_bytes());
        let _ = stdout.flush();
        std::mem::swap(&mut self.next, &mut self.shown);
        self.shown_cursor = cursor;
    }

    /// Takes in one event; false once the client should end.
    fn event(&mut self, event: Event) -> Result<bool, String> {
        match event {
            Event::Message(message) => Ok(self.message(message)),
            Event::Typed(bytes) => {
                let going_on = self.typed(&bytes);
                if !going_on {
                    self.send(&Request::Detach {
                        session: self.session,
                    });
                }
                Ok(going_on)
            }
            Event::Closed => Err("the server closed the connection".to_owned()),
            Event::Stopped => Ok(false),
        }
    }
}

/// The direction and xterm modifier parameter of an arrow key's sequence: 1 for none, 2
/// with Shift, 5 with Ctrl.
fn arrow(sequence: &[u8]) -> Option<(Direction, u8)> {
    let (&last, inner) = sequence.strip_prefix(b"\x1b")?.split_last()?;
    let direction = match last {
        b'A' => Direction::Up,
        b'B' => Direction::Down,
        b'C' => Direction::Right,
        b'D' => Direction::Left,
        _ => return None,
    };
    let modifier = match inner {
        b"[" | b"O" => 1,
        _ => std::str::from_utf8(inner.strip_prefix(b"[1;")?)
            .ok()?
            .parse()
            .ok()?,
    };
    Some((direction, modifier))
}

fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut text = String::new();
    for chunk in bytes.chunks(3) {
        let n = chunk
            .iter()
            .enumerate()
            .fold(0u32, |n, (i, &byte)| n | u32::from(byte) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                text.push(char::from(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]));
            } else {
                text.push('=');
            }
        }
    }
    text
}
