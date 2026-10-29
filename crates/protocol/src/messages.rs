//! Requests a client sends and what the server sends back.
//!
//! A server keeps every session and its terminals; a client attaches to sessions to
//! show them, and asks for each frame of a terminal it shows when it is ready to draw
//! one, so a busy program never floods it.

use crate::screen::{CursorShape, Frame, Point, Rgb};
use crate::wire::{Bytes, DecodeError, Wire, record, variants};

macro_rules! id {
    ($(#[$doc:meta])* $name:ident) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(pub u32);

        impl Wire for $name {
            fn put(&self, out: &mut Vec<u8>) {
                self.0.put(out);
            }

            fn take(input: &mut &[u8]) -> Result<Self, DecodeError> {
                u32::take(input).map(Self)
            }
        }
    };
}

id! {
    /// A group of tabs a client shows as one window.
    SessionId
}
id! {
    /// One program in a pseudoterminal.
    TerminalId
}

/// The grid in cells, and a cell in pixels, as programs are told.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Size {
    pub columns: u16,
    pub lines: u16,
    pub cell_width: u16,
    pub cell_height: u16,
}
record!(Size {
    columns,
    lines,
    cell_width,
    cell_height
});

/// What to run, where, and with what environment.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Command {
    /// A program and its arguments; the user's shell when empty.
    pub argv: Vec<String>,
    pub directory: Option<Bytes>,
    /// The environment of the command line that asked for the program; the server's own
    /// when empty.
    pub env: Vec<(String, String)>,
    /// A program named on the command line: if it fails, its tab stays with its output.
    pub hold: bool,
}
record!(Command {
    argv,
    directory,
    env,
    hold
});

/// The colours a client shows, which cells are resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub foreground: Rgb,
    pub background: Rgb,
    /// The 16 ANSI colours.
    pub ansi: Vec<Rgb>,
}
record!(Palette {
    foreground,
    background,
    ansi
});

/// How a client wants its terminals to behave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    pub scrollback: u32,
    pub cursor_shape: CursorShape,
    pub cursor_blinks: bool,
    /// Programs may put text on the clipboard.
    pub copy_from_programs: bool,
    pub scroll_on_output: bool,
}
record!(Options {
    scrollback,
    cursor_shape,
    cursor_blinks,
    copy_from_programs,
    scroll_on_output
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scroll {
    /// Towards the history for positive ones.
    Lines {
        lines: i32,
    },
    Page {
        up: bool,
    },
    Bottom,
    /// To lines of history from the top, as a scroll bar counts.
    To {
        line: u32,
    },
    /// To the previous or next prompt the shell marked; past the last, to the bottom.
    Prompt {
        up: bool,
    },
}
variants!(Scroll {
    0 => Lines { lines },
    1 => Page { up },
    2 => Bottom,
    3 => To { line },
    4 => Prompt { up },
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionKind {
    Simple,
    Word,
    Line,
}
variants!(SelectionKind {
    0 => Simple,
    1 => Word,
    2 => Line,
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Select {
    /// From a point, on the right half of its cell or the left.
    Start {
        kind: SelectionKind,
        point: Point,
        right: bool,
    },
    /// Moves the end of the selection there is, or starts one there.
    Extend {
        point: Point,
        right: bool,
    },
    All,
    Clear,
}
variants!(Select {
    0 => Start { kind, point, right },
    1 => Extend { point, right },
    2 => All,
    3 => Clear,
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Question {
    Sessions,
    NewSession,
    SelectionText {
        terminal: TerminalId,
    },
    LinkAt {
        terminal: TerminalId,
        point: Point,
    },
    /// The program in the foreground if it is not the one started.
    Busy {
        terminal: TerminalId,
    },
    Directory {
        terminal: TerminalId,
    },
    /// What a new tab from this one runs: the shell, where this one's is, in the same
    /// container.
    NextCommand {
        terminal: TerminalId,
    },
    /// Selects the next place `text` appears, upwards or back down, and scrolls to it.
    Find {
        terminal: TerminalId,
        text: String,
        upwards: bool,
    },
    /// Starts `command` in a new tab of the session: answered before the session's
    /// clients hear of the tab, so the one that asked knows it as its own.
    OpenTab {
        session: SessionId,
        index: u32,
        command: Command,
        size: Size,
    },
}
variants!(Question {
    0 => Sessions,
    1 => NewSession,
    2 => SelectionText { terminal },
    3 => LinkAt { terminal, point },
    4 => Busy { terminal },
    5 => Directory { terminal },
    6 => NextCommand { terminal },
    7 => Find { terminal, text, upwards },
    8 => OpenTab { session, index, command, size },
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Answer {
    Sessions {
        sessions: Vec<SessionInfo>,
    },
    Session {
        session: SessionId,
    },
    Text {
        text: Option<String>,
    },
    Link {
        uri: Option<String>,
    },
    Busy {
        program: Option<String>,
    },
    Directory {
        path: Option<Bytes>,
    },
    Command {
        command: Command,
    },
    Found {
        found: bool,
    },
    Opened {
        terminal: TerminalId,
    },
    /// The terminal or session asked about is gone.
    Gone,
}
variants!(Answer {
    0 => Sessions { sessions },
    1 => Session { session },
    2 => Text { text },
    3 => Link { uri },
    4 => Busy { program },
    5 => Directory { path },
    6 => Command { command },
    7 => Found { found },
    8 => Gone,
    9 => Opened { terminal },
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Tab {
    pub terminal: TerminalId,
    /// The program's title.
    pub title: String,
    /// The name the user gave the tab.
    pub name: Option<String>,
}
record!(Tab {
    terminal,
    title,
    name
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    pub session: SessionId,
    pub tabs: Vec<Tab>,
    /// Clients attached to it now.
    pub clients: u32,
}
record!(SessionInfo {
    session,
    tabs,
    clients
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Request {
    /// The client's colours and options, for every terminal it shows; sent first, and
    /// again when they change.
    Look {
        palette: Palette,
        options: Options,
    },
    Ask {
        id: u32,
        question: Question,
    },
    /// Starts hearing about a session: its tabs come in `Attached`.
    Attach {
        session: SessionId,
    },
    /// Stops hearing about it; it keeps running.
    Detach {
        session: SessionId,
    },
    /// Ends every terminal of a session.
    CloseSession {
        session: SessionId,
    },
    CloseTab {
        terminal: TerminalId,
    },
    /// Puts the tab at `index` of `session`, its own or another.
    MoveTab {
        terminal: TerminalId,
        session: SessionId,
        index: u32,
    },
    NameTab {
        terminal: TerminalId,
        name: Option<String>,
    },
    /// The client is ready to draw the terminal again: the next change comes as a frame.
    Frame {
        terminal: TerminalId,
    },
    /// Bytes for the program; what the user typed or pasted brings the view to the bottom
    /// when `bottom` says.
    Input {
        terminal: TerminalId,
        bytes: Bytes,
        bottom: bool,
    },
    Resize {
        terminal: TerminalId,
        size: Size,
    },
    Scroll {
        terminal: TerminalId,
        scroll: Scroll,
    },
    Select {
        terminal: TerminalId,
        select: Select,
    },
    /// The cell under the pointer, whose address the next frames underline.
    Hover {
        terminal: TerminalId,
        point: Option<Point>,
    },
    Focus {
        terminal: TerminalId,
        focused: bool,
    },
    /// Text shown in the terminal as if the program wrote it, in bold with the cursor
    /// hidden after it: how a program that ended is described.
    Annotate {
        terminal: TerminalId,
        text: String,
    },
}
variants!(Request {
    0 => Look { palette, options },
    1 => Ask { id, question },
    2 => Attach { session },
    3 => Detach { session },
    4 => CloseSession { session },
    5 => CloseTab { terminal },
    6 => MoveTab { terminal, session, index },
    7 => NameTab { terminal, name },
    8 => Frame { terminal },
    9 => Input { terminal, bytes, bottom },
    10 => Resize { terminal, size },
    11 => Scroll { terminal, scroll },
    12 => Select { terminal, select },
    13 => Hover { terminal, point },
    14 => Focus { terminal, focused },
    15 => Annotate { terminal, text },
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Message {
    Answer {
        id: u32,
        answer: Answer,
    },
    Attached {
        session: SessionId,
        tabs: Vec<Tab>,
    },
    TabOpened {
        session: SessionId,
        index: u32,
        tab: Tab,
    },
    /// The tab closed, or went to another session.
    TabClosed {
        session: SessionId,
        terminal: TerminalId,
    },
    TabMoved {
        session: SessionId,
        terminal: TerminalId,
        index: u32,
    },
    TabNamed {
        terminal: TerminalId,
        name: Option<String>,
    },
    /// Its last tab closed.
    SessionClosed {
        session: SessionId,
    },
    Frame {
        terminal: TerminalId,
        frame: Frame,
    },
    Title {
        terminal: TerminalId,
        title: String,
    },
    Bell {
        terminal: TerminalId,
    },
    Clipboard {
        terminal: TerminalId,
        primary: bool,
        text: String,
    },
    Notification {
        terminal: TerminalId,
        title: String,
        body: String,
    },
    CommandStarted {
        terminal: TerminalId,
    },
    /// How long it ran, and its exit status if the shell said.
    CommandFinished {
        terminal: TerminalId,
        seconds: u64,
        status: Option<i32>,
    },
    /// The program could not be started.
    Failed {
        terminal: TerminalId,
        reason: String,
    },
    /// A held program failed: its tab stays, with its output.
    Ended {
        terminal: TerminalId,
        code: Option<i32>,
        signal: Option<i32>,
    },
}
variants!(Message {
    0 => Answer { id, answer },
    1 => Attached { session, tabs },
    2 => TabOpened { session, index, tab },
    3 => TabClosed { session, terminal },
    4 => TabMoved { session, terminal, index },
    5 => TabNamed { terminal, name },
    6 => SessionClosed { session },
    7 => Frame { terminal, frame },
    8 => Title { terminal, title },
    9 => Bell { terminal },
    10 => Clipboard { terminal, primary, text },
    11 => Notification { terminal, title, body },
    12 => CommandStarted { terminal },
    13 => CommandFinished { terminal, seconds, status },
    14 => Failed { terminal, reason },
    15 => Ended { terminal, code, signal },
});
