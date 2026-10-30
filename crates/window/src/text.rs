//! A session drawn in the terminal Caret runs in, with escape sequences: its tabs' panes
//! side by side as the server lays them out, a line of tabs under them, and the keys
//! going to the focused pane. Ctrl+] starts control mode, whose keys move between panes
//! and tabs, split, close and zoom panes, and detach.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc;
use std::time::Duration;

use caret_protocol::layout::Node;
use caret_protocol::{
    Answer, BOLD, Bytes, Cell, Command, Frame, GlyphText, ITALIC, Message, Point, Question,
    Request, Rgb, SessionId, Size, Tab, TerminalId,
};
use rustix::termios::{self, OptionalActions};

use crate::panes::{self, Direction, Rect};

/// The byte Ctrl+] sends, which starts control mode.
const CONTROL: u8 = 0x1d;

const HINTS: &str = "arrows focus · n p tab · v s split · x close · z zoom · d detach · Esc";

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
    // The other screen, the cursor hidden until a pane shows its own.
    print!("\x1b[?1049h\x1b[?25l");
    let ended = show(&mut stream, session);
    print!("\x1b[0m\x1b[?25h\x1b[?1049l");
    let _ = io::stdout().flush();
    let _ = termios::tcsetattr(&stdin, OptionalActions::Now, &saved);
    ended
}

enum Event {
    Message(Message),
    Typed(Vec<u8>),
    Closed,
}

struct Screen {
    columns: u16,
    lines: u16,
    rows: Vec<Vec<Cell>>,
    background: Rgb,
    cursor: Option<Point>,
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
        self.cursor = frame.cursor.map(|cursor| cursor.point);
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
    tabs: Vec<TextTab>,
    selected: usize,
    screens: HashMap<TerminalId, Screen>,
    titles: HashMap<TerminalId, String>,
    /// Its size in cells, and in pixels where the terminal says.
    size: (u16, u16, u16, u16),
    sizes: HashMap<TerminalId, Size>,
    control: bool,
    zoomed: bool,
    asked: u32,
    /// The pane split off, to give the keys once the server lays it out.
    opened: Option<TerminalId>,
}

fn show(stream: &mut UnixStream, session: SessionId) -> Result<(), String> {
    let writer = stream.try_clone().map_err(|error| error.to_string())?;
    let (events, received) = mpsc::channel();
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
        size: window_size(),
        sizes: HashMap::new(),
        control: false,
        zoomed: false,
        asked: 0,
        opened: None,
    };
    client.send(&Request::Attach { session });
    loop {
        match received.recv_timeout(Duration::from_millis(250)) {
            Ok(Event::Message(message)) => {
                if !client.message(message) {
                    return Ok(());
                }
            }
            Ok(Event::Typed(bytes)) => {
                if !client.typed(&bytes) {
                    client.send(&Request::Detach { session });
                    return Ok(());
                }
            }
            Ok(Event::Closed) => return Err("the server closed the connection".to_owned()),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                let size = window_size();
                if size != client.size {
                    client.size = size;
                    client.resize();
                }
                continue;
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Ok(()),
        }
        client.draw();
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
                self.resize();
            }
            Message::SessionClosed { session } if session == self.session => return false,
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
                });
                screen.apply(frame);
                if self.placed().rect(terminal).is_some() {
                    self.send(&Request::Frame { terminal });
                }
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
            if self.control {
                let (used, go_on) = self.control_key(rest);
                rest = &rest[used.max(1).min(rest.len())..];
                if !go_on {
                    return false;
                }
                continue;
            }
            let until = rest.iter().position(|&byte| byte == CONTROL);
            let (typed, after) = rest.split_at(until.unwrap_or(rest.len()));
            if !typed.is_empty()
                && let Some(terminal) = self.tab().map(|tab| tab.focused)
            {
                self.send(&Request::Input {
                    terminal,
                    bytes: Bytes(typed.to_vec()),
                    bottom: true,
                });
            }
            if until.is_some() {
                self.control = true;
                rest = &after[1..];
            } else {
                rest = &[];
            }
        }
        true
    }

    /// Does what the key at the start of `bytes` says in control mode: how many bytes it
    /// took, and false to detach.
    fn control_key(&mut self, bytes: &[u8]) -> (usize, bool) {
        let arrow = match bytes {
            [0x1b, b'[' | b'O', b'A', ..] | [b'k', ..] => Some(Direction::Up),
            [0x1b, b'[' | b'O', b'B', ..] | [b'j', ..] => Some(Direction::Down),
            [0x1b, b'[' | b'O', b'C', ..] | [b'l', ..] => Some(Direction::Right),
            [0x1b, b'[' | b'O', b'D', ..] | [b'h', ..] => Some(Direction::Left),
            _ => None,
        };
        if let Some(direction) = arrow {
            self.focus_towards(direction);
            return (if bytes[0] == 0x1b { 3 } else { 1 }, true);
        }
        match bytes[0] {
            b'n' => self.step_tab(1),
            b'p' => self.step_tab(self.tabs.len().saturating_sub(1)),
            b'v' => self.split(true),
            b's' => self.split(false),
            b'x' => {
                if let Some(terminal) = self.tab().map(|tab| tab.focused) {
                    self.send(&Request::ClosePane { terminal });
                }
            }
            b'z' => {
                self.zoomed = !self.zoomed;
                self.resize();
            }
            b'd' => return (1, false),
            // A key it does not know leaves the mode; an escape sequence goes whole.
            0x1b => {
                self.control = false;
                let length = bytes
                    .iter()
                    .skip(2)
                    .position(|byte| byte.is_ascii_alphabetic() || *byte == b'~')
                    .map_or(1, |end| end + 3);
                return (if bytes.len() > 1 { length } else { 1 }, true);
            }
            _ => self.control = false,
        }
        (1, true)
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

    /// Draws the panes of the tab shown, the lines between them and the line of tabs.
    fn draw(&mut self) {
        let placed = self.placed();
        let (columns, lines, ..) = self.size;
        let mut out = String::from("\x1b[?2026h\x1b[0m\x1b[2J");
        for &(terminal, rect) in &placed.panes {
            let Some(screen) = self.screens.get(&terminal) else {
                continue;
            };
            for line in 0..rect.height {
                let _ = write!(out, "\x1b[{};{}H", rect.y + line + 1, rect.x + 1);
                let row = screen
                    .rows
                    .get(usize::try_from(line).unwrap_or(usize::MAX))
                    .map_or(&[][..], Vec::as_slice);
                draw_row(&mut out, row, rect.width, screen.background);
            }
        }
        out.push_str("\x1b[0m\x1b[2m");
        for divider in &placed.dividers {
            let gap = divider.gap;
            for line in 0..gap.height {
                for column in 0..gap.width {
                    let _ = write!(out, "\x1b[{};{}H", gap.y + line + 1, gap.x + column + 1);
                    out.push(if divider.vertical { '│' } else { '─' });
                }
            }
        }
        let _ = write!(out, "\x1b[0m\x1b[{lines};1H\x1b[7m");
        let status = if self.control {
            format!(" {HINTS} ")
        } else {
            let mut tabs = String::new();
            for (index, tab) in self.tabs.iter().enumerate() {
                let title = tab.name.as_deref().unwrap_or(&tab.title);
                let mark = if index == self.selected { '*' } else { ' ' };
                let _ = write!(tabs, " {}{mark}{title} ", index + 1);
            }
            tabs + "  Ctrl+] control"
        };
        let status: String = status.chars().take(usize::from(columns)).collect();
        let _ = write!(out, "{status:<width$}\x1b[0m", width = usize::from(columns));
        if let Some(tab) = self.tab()
            && let (Some(rect), Some(screen)) =
                (placed.rect(tab.focused), self.screens.get(&tab.focused))
            && let Some(cursor) = screen.cursor
        {
            let _ = write!(
                out,
                "\x1b[{};{}H\x1b[?25h",
                rect.y + i32::from(cursor.line) + 1,
                rect.x + i32::from(cursor.column) + 1
            );
        } else {
            out.push_str("\x1b[?25l");
        }
        out.push_str("\x1b[?2026l");
        let mut stdout = io::stdout();
        let _ = stdout.write_all(out.as_bytes());
        let _ = stdout.flush();
    }
}

/// A line of cells, `width` wide, blank where it has no cell.
fn draw_row(out: &mut String, row: &[Cell], width: i32, background: Rgb) {
    let mut column = 0;
    let mut cells = row.iter().peekable();
    while column < width {
        let cell = cells
            .peek()
            .filter(|cell| i32::from(cell.column) == column)
            .copied();
        let Some(cell) = cell else {
            let _ = write!(
                out,
                "\x1b[0;48;2;{};{};{}m ",
                background.r, background.g, background.b
            );
            column += 1;
            continue;
        };
        cells.next();
        let style = cell.glyph.as_ref().map_or(0, |glyph| glyph.style);
        let (fg, bg) = (cell.fg, cell.bg);
        let _ = write!(
            out,
            "\x1b[0{}{};38;2;{};{};{};48;2;{};{};{}m",
            if style & BOLD != 0 { ";1" } else { "" },
            if style & ITALIC != 0 { ";3" } else { "" },
            fg.r,
            fg.g,
            fg.b,
            bg.r,
            bg.g,
            bg.b,
        );
        match cell.glyph.as_ref().map(|glyph| &glyph.text) {
            Some(GlyphText::Char(c)) => out.push(*c),
            Some(GlyphText::Cluster(text)) => out.push_str(text),
            None => out.push(' '),
        }
        let wide = cell.glyph.as_ref().is_some_and(|glyph| glyph.wide);
        column += if wide { 2 } else { 1 };
    }
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
