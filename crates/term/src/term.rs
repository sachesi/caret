//! A terminal: its screens, cursor, modes and history, as the program's output changes
//! them, and the replies and events that output asks for.

mod csi;
mod osc;
#[cfg(test)]
mod tests;

use std::borrow::Cow;
use std::collections::HashMap;
use std::ops::RangeInclusive;
use std::path::PathBuf;

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::grid::{Grid, MAX_CLUSTER_BYTES, Row, Text};
use crate::history::{History, Line};
use crate::palette::Palette;
use crate::parser::{Params, Parser, Perform};
use crate::style::{Cell, Flags, LinkId, Style};

/// How a terminal behaves where programs leave it to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Options {
    /// Rows of history kept, at the terminal's width.
    pub scrollback: usize,
    pub cursor: CursorStyle,
    /// Programs may put text on the clipboard.
    pub copy_from_programs: bool,
    /// The name and version told to programs that ask.
    pub version: String,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            scrollback: 10_000,
            cursor: CursorStyle::default(),
            copy_from_programs: true,
            version: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Beam,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CursorStyle {
    pub shape: CursorShape,
    pub blinking: bool,
}

impl Default for CursorStyle {
    fn default() -> Self {
        Self {
            shape: CursorShape::Block,
            blinking: true,
        }
    }
}

/// Something the output asked for beyond the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The title changed; empty when a program reset it.
    Title(String),
    Bell,
    /// Text to put on the clipboard, or the primary selection.
    Clipboard {
        primary: bool,
        text: String,
    },
    Report(Report),
}

/// What the shell tells beside its output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Directory(PathBuf),
    Notification {
        title: String,
        body: String,
    },
    CommandStarted,
    /// With the command's exit status, when the shell gave it.
    CommandFinished(Option<i32>),
    /// A toolbox or distrobox container the shell entered, by name and by the tool that
    /// runs it.
    ContainerEntered {
        name: String,
        runtime: String,
    },
    ContainerLeft,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MouseMode {
    #[default]
    Off,
    /// Presses and releases.
    Click,
    /// Moves too, while a button is held.
    Drag,
    /// Every move.
    Motion,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MouseEncoding {
    #[default]
    Default,
    Utf8,
    Sgr,
}

/// The modes programs set and reset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a separate mode a program sets"
)]
pub struct Modes {
    pub app_cursor: bool,
    pub app_keypad: bool,
    pub line_feed_new_line: bool,
    pub insert: bool,
    pub autowrap: bool,
    pub origin: bool,
    pub show_cursor: bool,
    pub mouse: MouseMode,
    pub mouse_encoding: MouseEncoding,
    pub focus: bool,
    pub alternate_scroll: bool,
    pub bracketed_paste: bool,
    /// The program is drawing: the screen is shown once it ends.
    pub synchronized: bool,
    /// Characters are grouped into grapheme clusters, each as wide as Unicode says.
    pub graphemes: bool,
}

impl Default for Modes {
    fn default() -> Self {
        Self {
            app_cursor: false,
            app_keypad: false,
            line_feed_new_line: false,
            insert: false,
            autowrap: true,
            origin: false,
            show_cursor: true,
            mouse: MouseMode::Off,
            mouse_encoding: MouseEncoding::Default,
            focus: false,
            alternate_scroll: true,
            bracketed_paste: false,
            synchronized: false,
            graphemes: false,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum Charset {
    #[default]
    Ascii,
    /// DEC's special graphics: line drawing in place of lower-case letters.
    Graphics,
    /// ASCII with a pound sign for `#`.
    British,
}

impl Charset {
    fn map(self, c: char) -> char {
        match self {
            Charset::British if c == '#' => '£',
            Charset::Ascii | Charset::British => c,
            Charset::Graphics => match c {
                '_' => ' ',
                '`' => '◆',
                'a' => '▒',
                'b' => '␉',
                'c' => '␌',
                'd' => '␍',
                'e' => '␊',
                'f' => '°',
                'g' => '±',
                'h' => '␤',
                'i' => '␋',
                'j' => '┘',
                'k' => '┐',
                'l' => '┌',
                'm' => '└',
                'n' => '┼',
                'o' => '⎺',
                'p' => '⎻',
                'q' => '─',
                'r' => '⎼',
                's' => '⎽',
                't' => '├',
                'u' => '┤',
                'v' => '┴',
                'w' => '┬',
                'x' => '│',
                'y' => '≤',
                'z' => '≥',
                '{' => 'π',
                '|' => '≠',
                '}' => '£',
                '~' => '·',
                c => c,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Cursor {
    line: usize,
    column: usize,
    /// The last character went in the last column: the next one wraps first.
    pending: bool,
    style: Style,
    charsets: [Charset; 4],
    /// Which of the four is in use.
    shifted: usize,
    /// One for the next character only.
    single_shift: Option<usize>,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Saved {
    cursor: Cursor,
    origin: bool,
}

/// The links programs gave text, numbered for the cells to refer to.
#[derive(Debug, Clone, Default)]
struct Links {
    /// Index 0 is no link.
    uris: Vec<Box<str>>,
    /// Links by what a program named them, if it did, and address: its separate pieces of
    /// text share one, and a program listing the same addresses again takes no more numbers.
    named: HashMap<(Box<str>, Box<str>), LinkId>,
}

impl Links {
    fn add(&mut self, id: &str, uri: &str) -> LinkId {
        if let Some(&link) = self.named.get(&(id.into(), uri.into())) {
            return link;
        }
        if self.uris.is_empty() {
            self.uris.push("".into());
        }
        // Out of numbers: the text shows without its link.
        let Ok(link) = LinkId::try_from(self.uris.len()) else {
            return 0;
        };
        self.uris.push(uri.into());
        self.named.insert((id.into(), uri.into()), link);
        link
    }
}

/// Keyboard enhancement flags a program pushed, for each screen.
const MAX_KEYBOARD_STACK: usize = 16;
/// Titles a program pushed to restore later.
const MAX_TITLES: usize = 16;
/// Bytes of a title or of a notification's text kept, which clients have to lay out and the
/// server to send on each change. Commands can be far longer, for copies to the clipboard.
const MAX_TEXT_BYTES: usize = 4096;
/// Bytes of a link's address: every one is kept for as long as the terminal lives, so a
/// longer one is shown as text without a link.
const MAX_LINK_BYTES: usize = 4096;

pub struct Term {
    parser: Parser,
    main: Grid,
    alt: Grid,
    alt_active: bool,
    history: History,
    cursor: Cursor,
    /// The cursor DECSC saved, for each screen.
    saved: [Saved; 2],
    /// The scrolling region's first and last line.
    top: usize,
    bottom: usize,
    tabs: Vec<bool>,
    modes: Modes,
    /// The cursor a program chose, or None for the user's.
    cursor_style: Option<CursorStyle>,
    /// The keyboard enhancement flags in force, and those pushed, for each screen.
    keyboard: [u8; 2],
    keyboard_stack: [Vec<u8>; 2],
    title: String,
    titles: Vec<String>,
    palette: Palette,
    links: Links,
    options: Options,
    /// A cell's size in pixels, for programs that ask for the window's.
    cell_size: (u16, u16),
    /// The last character shown and where, for REP and for marks that join it; the line is
    /// past the screen once the cell is forgotten.
    last: Option<(char, usize, usize)>,
    dcs: Option<osc::Dcs>,
    replies: Vec<u8>,
    events: Vec<Event>,
    /// The palette changed, so every row's colours did.
    recolored: bool,
}

impl Term {
    #[must_use]
    pub fn new(columns: usize, lines: usize, options: Options) -> Self {
        let columns = columns.max(1);
        let lines = lines.max(1);
        Self {
            parser: Parser::new(),
            main: Grid::new(columns, lines),
            alt: Grid::new(columns, lines),
            alt_active: false,
            history: History::new(columns, options.scrollback),
            cursor: Cursor::default(),
            saved: [Saved::default(); 2],
            top: 0,
            bottom: lines - 1,
            tabs: default_tabs(columns),
            modes: Modes::default(),
            cursor_style: None,
            keyboard: [0; 2],
            keyboard_stack: [Vec::new(), Vec::new()],
            title: String::new(),
            titles: Vec::new(),
            palette: Palette::default(),
            links: Links::default(),
            options,
            cell_size: (0, 0),
            last: None,
            dcs: None,
            replies: Vec::new(),
            events: Vec::new(),
            recolored: false,
        }
    }

    /// Takes in a program's output.
    pub fn advance(&mut self, bytes: &[u8]) {
        let mut parser = std::mem::take(&mut self.parser);
        parser.advance(self, bytes);
        self.parser = parser;
    }

    /// What to write back to the program: replies to its questions.
    pub fn take_replies(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.replies)
    }

    pub fn take_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    #[must_use]
    pub fn columns(&self) -> usize {
        self.main.columns()
    }

    #[must_use]
    pub fn lines(&self) -> usize {
        self.main.lines()
    }

    /// The screen shown: the alternate one while a program uses it.
    #[must_use]
    pub fn screen(&self) -> &Grid {
        if self.alt_active {
            &self.alt
        } else {
            &self.main
        }
    }

    /// Forgets which rows changed, once a frame was made of them.
    pub fn clean(&mut self) {
        self.main.clean();
        self.alt.clean();
        self.recolored = false;
    }

    /// Whether every row's colours changed, with the palette.
    #[must_use]
    pub fn recolored(&self) -> bool {
        self.recolored
    }

    #[must_use]
    pub fn history(&self) -> &History {
        &self.history
    }

    /// The serial number of the line each row of the screen shown is in, and the cell of
    /// that line the row starts with. Lines go on from the history's.
    #[must_use]
    pub fn screen_places(&self) -> Vec<(u64, usize)> {
        let open = self.history.is_open() && !self.alt_active;
        let next = self.history.serial(self.history.len());
        let mut places = Vec::with_capacity(self.lines());
        let mut place = if open {
            // Without history, the start of the line is gone.
            let width = self.history.len().checked_sub(1);
            (
                next - 1,
                width.map_or(0, |last| self.history.line(last).width()),
            )
        } else {
            (next, 0)
        };
        let rows = self.screen().rows();
        for index in 0..rows.len() {
            if index > 0 {
                let above = &rows[index - 1];
                place = if above.wrapped {
                    (place.0, place.1 + above.line_cells())
                } else {
                    (place.0 + 1, 0)
                };
            }
            places.push(place);
        }
        places
    }

    /// The serial numbers of the lines there are: the history's and the screen's, or only
    /// the screen's while the alternate one is shown.
    #[must_use]
    pub fn serials(&self) -> RangeInclusive<u64> {
        let places = self.screen_places();
        let first = places.first().map_or(0, |place| place.0);
        let last = places.last().map_or(0, |place| place.0);
        if self.alt_active || self.history.is_empty() {
            first..=last
        } else {
            self.history.serial(0)..=last
        }
    }

    /// The line with serial number `serial`, from the history or the screen.
    #[must_use]
    pub fn line(&self, serial: u64) -> Option<Cow<'_, Line>> {
        let next = self.history.serial(self.history.len());
        let open = self.history.is_open() && !self.alt_active;
        if serial < next - u64::from(open) {
            let index = self.history.index_of(serial)?;
            return Some(Cow::Borrowed(self.history.line(index)));
        }
        let places = self.screen_places();
        let mut rows = places
            .iter()
            .zip(self.screen().rows())
            .filter(|((line, _), _)| *line == serial)
            .map(|(_, row)| row)
            .peekable();
        rows.peek()?;
        let mut line = match self.history.index_of(serial) {
            Some(index) => self.history.line(index).clone(),
            None => Line::default(),
        };
        for row in rows {
            line.append(row);
        }
        Some(Cow::Owned(line))
    }

    /// The serial number of the highest line output wrote to since this was last called,
    /// or one below it.
    pub fn take_written(&mut self) -> Option<u64> {
        let main = self.main.take_written();
        let alt = self.alt.take_written();
        let written = if self.alt_active { alt } else { main }?;
        let places = self.screen_places();
        Some(match usize::try_from(written) {
            Ok(row) => places[row.min(places.len() - 1)].0,
            // Each row above the screen is at most a line.
            Err(_) => places[0].0.saturating_sub(written.unsigned_abs() as u64),
        })
    }

    #[must_use]
    pub fn alt_screen(&self) -> bool {
        self.alt_active
    }

    #[must_use]
    pub fn modes(&self) -> &Modes {
        &self.modes
    }

    /// The keyboard enhancement flags the program asked for.
    #[must_use]
    pub fn keyboard_flags(&self) -> u8 {
        self.keyboard[usize::from(self.alt_active)]
    }

    /// Where the cursor is, shown or not.
    #[must_use]
    pub fn cursor_at(&self) -> (usize, usize) {
        (self.cursor.line, self.cursor.column)
    }

    /// Where the cursor is, if it is shown.
    #[must_use]
    pub fn cursor(&self) -> Option<(usize, usize)> {
        self.modes
            .show_cursor
            .then_some((self.cursor.line, self.cursor.column))
    }

    #[must_use]
    pub fn cursor_style(&self) -> CursorStyle {
        self.cursor_style.unwrap_or(self.options.cursor)
    }

    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    #[must_use]
    pub fn palette(&self) -> &Palette {
        &self.palette
    }

    pub fn set_palette_base(&mut self, base: crate::palette::Base) {
        if *self.palette.base() != base {
            self.palette.set_base(base);
            self.recolored = true;
        }
    }

    /// The address of link `link`, or None for none.
    #[must_use]
    pub fn link(&self, link: LinkId) -> Option<&str> {
        (link != 0)
            .then(|| self.links.uris.get(usize::from(link)).map(|uri| &**uri))
            .flatten()
    }

    pub fn set_options(&mut self, options: Options) {
        self.history.set_limit(options.scrollback);
        self.options = options;
    }

    pub fn set_cell_size(&mut self, width: u16, height: u16) {
        self.cell_size = (width, height);
    }

    /// Ends a synchronized update the program did not end in time.
    pub fn end_synchronized(&mut self) {
        self.modes.synchronized = false;
    }

    fn grid(&mut self) -> &mut Grid {
        if self.alt_active {
            &mut self.alt
        } else {
            &mut self.main
        }
    }

    fn row(&mut self) -> &mut Row {
        let line = self.cursor.line;
        self.grid().row_mut(line)
    }

    /// The style of blanks that erasing leaves.
    fn blank(&self) -> Style {
        self.cursor.style.erased()
    }

    fn reply(&mut self, text: &str) {
        self.replies.extend_from_slice(text.as_bytes());
    }

    // Printing.

    fn put(&mut self, c: char) {
        let charset = self
            .cursor
            .single_shift
            .take()
            .unwrap_or(self.cursor.shifted);
        let c = self.cursor.charsets[charset].map(c);
        if self.modes.graphemes && self.join_cluster(c) {
            return;
        }
        let width = c.width().unwrap_or(0);
        if width == 0 {
            self.add_mark(c);
            return;
        }
        self.place(c, width);
    }

    /// Writes `c`, `width` cells wide, wrapping first where it does not fit.
    fn place(&mut self, c: char, width: usize) {
        let columns = self.columns();
        if self.cursor.pending && self.modes.autowrap {
            self.wrap();
        }
        if width == 2 && self.cursor.column + 1 >= columns {
            if columns < 2 {
                return;
            }
            if self.modes.autowrap {
                // The last cell stays blank, and the line goes on in the next row.
                let column = self.cursor.column;
                // Drawn as the text around it is, so that a coloured bar goes on.
                let mut padding = Cell::blank(self.cursor.style);
                padding.style.flags.insert(Flags::SPACER);
                let row = self.row();
                row.split_wide(column, column + 1);
                row.set(column, padding);
                self.wrap();
            } else {
                self.cursor.column = columns - 2;
            }
        }
        self.write(c, width);
    }

    /// Puts `c`, `width` cells wide, at the cursor and moves past it.
    fn write(&mut self, c: char, width: usize) {
        let columns = self.columns();
        let (line, column) = (self.cursor.line, self.cursor.column);
        let insert = self.modes.insert;
        let blank = self.blank();
        let mut style = self.cursor.style;
        let row = self.row();
        if insert {
            row.insert(column, width, blank);
        }
        row.split_wide(column, (column + width).min(columns));
        if width == 2 {
            style.flags.insert(Flags::WIDE);
            row.set(column, Cell { c, style });
            style.flags.remove(Flags::WIDE);
            style.flags.insert(Flags::SPACER);
            row.set(column + 1, Cell::blank(style));
        } else {
            row.set(column, Cell { c, style });
        }
        self.last = Some((c, line, column));
        if column + width >= columns {
            self.cursor.column = columns - 1;
            self.cursor.pending = self.modes.autowrap;
        } else {
            self.cursor.column = column + width;
        }
    }

    /// Joins `c` to the cell the last character went in: a combining mark, or another
    /// part of its emoji.
    fn add_mark(&mut self, c: char) {
        let Some((_, line, column)) = self.last else {
            return;
        };
        if line >= self.lines() {
            return;
        }
        self.grid().row_mut(line).add_mark(column, c);
    }

    /// Keeps the last character for REP, and forgets where it went: marks after this do
    /// not join it.
    fn forget_last_cell(&mut self) {
        if let Some(last) = &mut self.last {
            last.1 = usize::MAX;
        }
    }

    /// With grapheme clusters on, joins `c` to the last character when it continues its
    /// cluster, widening the cell when the cluster turns two cells wide. Whether it did.
    fn join_cluster(&mut self, c: char) -> bool {
        let Some((_, line, column)) = self.last else {
            return false;
        };
        if line >= self.lines() {
            return false;
        }
        let row = self.grid().row(line);
        let mut text = match row.text(column) {
            Text::Char(first) => String::from(first),
            Text::Cluster(text) => text.to_owned(),
        };
        text.push(c);
        if text.graphemes(true).count() > 1 {
            return false;
        }
        if text.len() > MAX_CLUSTER_BYTES {
            return true;
        }
        let narrow = !row.cells[column].style.flags.contains(Flags::WIDE);
        let columns = self.columns();
        let widen = narrow && cluster_width(&text) == 2 && column + 1 < columns;
        let row = self.grid().row_mut(line);
        let mut cell = row.cells[column];
        if widen {
            row.split_wide(column + 1, column + 2);
            cell.style.flags.insert(Flags::WIDE);
            let mut spacer = Cell::blank(cell.style);
            spacer.style.flags.remove(Flags::WIDE | Flags::CLUSTER);
            spacer.style.flags.insert(Flags::SPACER);
            row.set(column + 1, spacer);
        }
        row.set_cluster(column, cell, &text);
        // The cursor was after the cell, and stays after it now that it is wider.
        if widen && self.cursor.line == line && self.cursor.column == column + 1 {
            if column + 2 >= columns {
                self.cursor.pending = self.modes.autowrap;
            } else {
                self.cursor.column = column + 2;
            }
        }
        true
    }

    /// Moves to the start of the next row, the line going on in it.
    fn wrap(&mut self) {
        self.row().wrapped = true;
        self.cursor.column = 0;
        self.cursor.pending = false;
        self.index();
    }

    // Moving the cursor.

    /// Moves down a line, scrolling the region up at its bottom.
    fn index(&mut self) {
        if self.cursor.line == self.bottom {
            self.scroll_up(1);
        } else if self.cursor.line + 1 < self.lines() {
            self.cursor.line += 1;
        }
    }

    /// Moves up a line, scrolling the region down at its top.
    fn reverse_index(&mut self) {
        if self.cursor.line == self.top {
            self.scroll_down(1);
        } else if self.cursor.line > 0 {
            self.cursor.line -= 1;
        }
    }

    fn scroll_up(&mut self, count: usize) {
        let (top, bottom) = (self.top, self.bottom);
        let blank = self.blank();
        if self.alt_active {
            self.alt.scroll_up(top, bottom, count, blank, |_| {});
            self.alt.wrote(top);
        } else if top == 0 {
            let history = &mut self.history;
            self.main
                .scroll_up(top, bottom, count, blank, |row| history.push(row));
        } else {
            self.main.scroll_up(top, bottom, count, blank, |_| {});
        }
        self.forget_last_cell();
    }

    fn scroll_down(&mut self, count: usize) {
        let (top, bottom) = (self.top, self.bottom);
        let blank = self.blank();
        self.grid().scroll_down(top, bottom, count, blank);
        self.forget_last_cell();
    }

    /// Moves down a line. Text that reached the last column still wraps before the next
    /// character, as it does after the other ways down without a carriage return.
    fn line_feed(&mut self) {
        self.index();
        if self.modes.line_feed_new_line {
            self.cursor.column = 0;
        }
    }

    fn carriage_return(&mut self) {
        self.cursor.column = 0;
        self.cursor.pending = false;
    }

    /// Puts the cursor at `line` and `column`, counted from the region's top in origin
    /// mode, and kept on the screen or in the region.
    fn goto(&mut self, line: usize, column: usize) {
        let (top, bottom) = if self.modes.origin {
            (self.top, self.bottom)
        } else {
            (0, self.lines() - 1)
        };
        self.cursor.line = (top + line).min(bottom);
        self.cursor.column = column.min(self.columns() - 1);
        self.cursor.pending = false;
    }

    /// The cursor's line as origin mode counts it.
    fn origin_line(&self) -> usize {
        if self.modes.origin {
            self.cursor.line.saturating_sub(self.top)
        } else {
            self.cursor.line
        }
    }

    fn move_up(&mut self, count: usize) {
        let limit = if self.cursor.line >= self.top {
            self.top
        } else {
            0
        };
        self.cursor.line = self.cursor.line.saturating_sub(count).max(limit);
        self.cursor.pending = false;
    }

    fn move_down(&mut self, count: usize) {
        let limit = if self.cursor.line <= self.bottom {
            self.bottom
        } else {
            self.lines() - 1
        };
        self.cursor.line = (self.cursor.line + count).min(limit);
        self.cursor.pending = false;
    }

    fn move_to_column(&mut self, column: usize) {
        self.cursor.column = column.min(self.columns() - 1);
        self.cursor.pending = false;
    }

    fn tab(&mut self, count: usize) {
        if self.cursor.pending {
            return;
        }
        let columns = self.columns();
        for _ in 0..count {
            if self.cursor.column == columns - 1 {
                break;
            }
            let next = (self.cursor.column + 1..columns).find(|&column| self.tabs[column]);
            self.cursor.column = next.unwrap_or(columns - 1);
        }
        self.cursor.pending = false;
    }

    fn back_tab(&mut self, count: usize) {
        for _ in 0..count {
            if self.cursor.column == 0 {
                break;
            }
            let previous = (0..self.cursor.column)
                .rev()
                .find(|&column| self.tabs[column]);
            self.cursor.column = previous.unwrap_or(0);
        }
        self.cursor.pending = false;
    }

    fn save_cursor(&mut self) {
        self.saved[usize::from(self.alt_active)] = Saved {
            cursor: self.cursor,
            origin: self.modes.origin,
        };
    }

    fn restore_cursor(&mut self) {
        let saved = self.saved[usize::from(self.alt_active)];
        self.cursor = saved.cursor;
        self.modes.origin = saved.origin;
        self.cursor.line = self.cursor.line.min(self.lines() - 1);
        self.cursor.column = self.cursor.column.min(self.columns() - 1);
    }

    // Screens.

    fn enter_alt_screen(&mut self, clear: bool) {
        if self.alt_active {
            return;
        }
        self.alt_active = true;
        if clear {
            let blank = self.blank();
            let (columns, lines) = (self.columns(), self.lines());
            self.alt
                .set_rows(vec![Row::new(columns, blank); lines], columns);
        }
        self.alt.touch_all();
        self.forget_last_cell();
    }

    fn leave_alt_screen(&mut self) {
        if !self.alt_active {
            return;
        }
        self.alt_active = false;
        self.main.touch_all();
        self.forget_last_cell();
    }

    /// Back to how a new terminal starts, keeping its history.
    fn reset(&mut self) {
        let (columns, lines) = (self.columns(), self.lines());
        self.leave_alt_screen();
        self.main = Grid::new(columns, lines);
        self.alt = Grid::new(columns, lines);
        self.cursor = Cursor::default();
        self.saved = [Saved::default(); 2];
        self.top = 0;
        self.bottom = lines - 1;
        self.tabs = default_tabs(columns);
        self.modes = Modes::default();
        self.cursor_style = None;
        self.keyboard = [0; 2];
        self.keyboard_stack = [Vec::new(), Vec::new()];
        self.palette.reset_all();
        self.recolored = true;
        self.last = None;
        if !self.title.is_empty() {
            self.title.clear();
            self.events.push(Event::Title(String::new()));
        }
    }

    // Resizing.

    /// Makes the terminal `columns` by `lines` cells. The main screen's lines are wrapped
    /// again; the history's are when they are next shown.
    pub fn resize(&mut self, columns: usize, lines: usize) {
        let columns = columns.max(1);
        let lines = lines.max(1);
        if (columns, lines) == (self.columns(), self.lines()) {
            return;
        }
        let main_cursor = if self.alt_active {
            self.saved[0].cursor
        } else {
            self.cursor
        };
        let (line, column) = self.reflow(columns, lines, main_cursor);
        if self.alt_active {
            self.saved[0].cursor.line = line;
            self.saved[0].cursor.column = column;
            self.saved[0].cursor.pending = false;
        } else {
            self.cursor.line = line;
            self.cursor.column = column;
            self.cursor.pending = false;
        }

        let mut rows = self.alt.take_rows();
        rows.resize_with(lines, || Row::new(columns, Style::default()));
        for row in &mut rows {
            row.resize(columns, Style::default());
            row.wrapped = false;
        }
        self.alt.set_rows(rows, columns);
        if self.alt_active {
            self.cursor.line = self.cursor.line.min(lines - 1);
            self.cursor.column = self.cursor.column.min(columns - 1);
            self.cursor.pending = false;
        }
        for saved in &mut self.saved {
            saved.cursor.line = saved.cursor.line.min(lines - 1);
            saved.cursor.column = saved.cursor.column.min(columns - 1);
        }
        self.top = 0;
        self.bottom = lines - 1;
        let old = self.tabs.len();
        self.tabs.resize(columns, false);
        for column in old..columns {
            self.tabs[column] = column % 8 == 0;
        }
        self.forget_last_cell();
    }

    /// Wraps the main screen's lines `columns` wide on a screen `lines` high, and gives the
    /// cursor's place, which moves with the text it was in.
    fn reflow(&mut self, columns: usize, lines: usize, cursor: Cursor) -> (usize, usize) {
        let old_lines = self.main.lines();
        let old_rows = self.main.take_rows();
        // Rows of text, with the line the newest history line went on in.
        let mut logical: Vec<Line> = Vec::new();
        let mut first_continues = false;
        if self.history.is_open()
            && let Some(line) = self.history.pop()
        {
            logical.push(line);
            first_continues = true;
        }
        let last_used = old_rows
            .iter()
            .rposition(|row| row.occupied() > 0 || row.prompt)
            .unwrap_or(0)
            .max(cursor.line);
        let mut cursor_at = (0, 0);
        let mut continues = first_continues;
        for (index, row) in old_rows.iter().enumerate().take(last_used + 1) {
            if !continues {
                logical.push(Line::default());
            }
            let current = logical.len() - 1;
            if index == cursor.line {
                // Text that reached the last column has the cursor after it.
                let column = cursor.column + usize::from(cursor.pending);
                cursor_at = (current, logical[current].width() + column);
            }
            logical[current].append(row);
            continues = row.wrapped;
        }

        let blank = Style::default();
        let mut rows: Vec<Row> = Vec::new();
        let mut cursor_row = 0;
        let mut cursor_column = 0;
        for (index, line) in logical.iter().enumerate() {
            let mut wrapped = line.wrap(columns, blank);
            if index == cursor_at.0 {
                let offset = cursor_at.1;
                let starts = line.row_starts(columns);
                let mut row = starts.partition_point(|&start| start <= offset).max(1) - 1;
                // Past the text, the cursor goes on in rows of blanks.
                let mut column = offset - starts[row];
                row += column / columns;
                column %= columns;
                while wrapped.len() <= row {
                    if let Some(last) = wrapped.last_mut() {
                        last.wrapped = true;
                    }
                    wrapped.push(Row::new(columns, blank));
                }
                if let Some(last) = wrapped.last_mut() {
                    last.wrapped = false;
                }
                cursor_row = rows.len() + row;
                cursor_column = column;
            }
            rows.extend(wrapped);
        }
        self.history.set_columns(columns);

        // The cursor keeps its distance from the bottom, less the blank rows a shorter
        // screen gives up first; rows come back from history to keep it on a taller one.
        let below = old_lines - 1 - cursor.line.min(old_lines - 1);
        let below = if lines < old_lines {
            below.saturating_sub(old_lines - lines)
        } else {
            below
        };
        let mut target = (lines - 1).saturating_sub(below);
        if target > cursor_row {
            let mut pulled: Vec<Row> = Vec::new();
            while pulled.len() < target - cursor_row {
                let Some(line) = self.history.pop() else {
                    break;
                };
                let mut wrapped = line.wrap(columns, blank);
                wrapped.append(&mut pulled);
                pulled = wrapped;
            }
            let wanted = target - cursor_row;
            if pulled.len() > wanted {
                let keep = pulled.split_off(pulled.len() - wanted);
                for row in &pulled {
                    self.history.push(row);
                }
                pulled = keep;
            }
            target = cursor_row + pulled.len();
            cursor_row += pulled.len();
            pulled.append(&mut rows);
            rows = pulled;
        }
        let excess = cursor_row.saturating_sub(target);
        for row in rows.drain(..excess) {
            self.history.push(&row);
        }
        cursor_row -= excess;
        rows.truncate(lines);
        rows.resize_with(lines, || Row::new(columns, blank));
        self.main.set_rows(rows, columns);
        (cursor_row, cursor_column.min(columns - 1))
    }
}

fn default_tabs(columns: usize) -> Vec<bool> {
    (0..columns).map(|column| column % 8 == 0).collect()
}

/// How many cells a grapheme cluster takes: as its first character does, but two for
/// emoji shown as pictures.
fn cluster_width(text: &str) -> usize {
    if text.contains('\u{fe0f}') {
        return 2;
    }
    if text.contains('\u{fe0e}') {
        return 1;
    }
    let first = text.chars().next().map_or(1, |c| c.width().unwrap_or(1));
    first.max(text.width().min(2)).min(2)
}

impl Perform for Term {
    fn print(&mut self, c: char) {
        self.put(c);
    }

    fn print_ascii(&mut self, text: &[u8]) {
        let plain = self.cursor.single_shift.is_none()
            && self.cursor.charsets[self.cursor.shifted] == Charset::Ascii
            && !self.modes.insert;
        if !plain {
            for &byte in text {
                self.put(char::from(byte));
            }
            return;
        }
        let columns = self.columns();
        let mut rest = text;
        while !rest.is_empty() {
            if self.cursor.pending && self.modes.autowrap {
                self.wrap();
            }
            let column = self.cursor.column;
            let room = columns - column;
            let count = rest.len().min(room);
            let style = self.cursor.style;
            let line = self.cursor.line;
            let row = self.row();
            row.split_wide(column, column + count);
            row.set_ascii(column, &rest[..count], style);
            self.last = Some((char::from(rest[count - 1]), line, column + count - 1));
            if column + count >= columns {
                self.cursor.column = columns - 1;
                self.cursor.pending = self.modes.autowrap;
                if !self.modes.autowrap {
                    // Without wrapping the rest goes over the last column, the last of
                    // it staying.
                    let last = rest[rest.len() - 1];
                    self.row().set(
                        columns - 1,
                        Cell {
                            c: char::from(last),
                            style,
                        },
                    );
                    self.last = Some((char::from(last), line, columns - 1));
                    return;
                }
            } else {
                self.cursor.column = column + count;
            }
            rest = &rest[count..];
        }
    }

    fn execute(&mut self, byte: u8) {
        match byte {
            // Only one of a run in the same read reaches anyone, and a byte each would add
            // up to tens of megabytes before it is taken.
            0x07 if self.events.last() == Some(&Event::Bell) => {}
            0x07 => self.events.push(Event::Bell),
            0x08 => {
                self.cursor.column = self.cursor.column.saturating_sub(1);
                self.cursor.pending = false;
            }
            // Past the last column, a tab goes on to the next line, as the text would.
            0x09 if self.cursor.pending => self.wrap(),
            0x09 => self.tab(1),
            0x0a..=0x0c => self.line_feed(),
            0x0d => self.carriage_return(),
            0x0e => self.cursor.shifted = 1,
            0x0f => self.cursor.shifted = 0,
            _ => {}
        }
    }

    fn csi(&mut self, params: &Params, intermediates: &[u8], ignored: bool, action: u8) {
        if !ignored {
            self.control(params, intermediates, action);
        }
    }

    fn esc(&mut self, intermediates: &[u8], ignored: bool, byte: u8) {
        if ignored {
            return;
        }
        match (intermediates, byte) {
            ([], b'7') => self.save_cursor(),
            ([], b'8') => self.restore_cursor(),
            ([], b'D') => self.index(),
            ([], b'E') => {
                self.carriage_return();
                self.index();
            }
            ([], b'H') => {
                let column = self.cursor.column;
                self.tabs[column] = true;
            }
            ([], b'M') => self.reverse_index(),
            ([], b'N') => self.cursor.single_shift = Some(2),
            ([], b'O') => self.cursor.single_shift = Some(3),
            ([], b'c') => self.reset(),
            ([], b'=') => self.modes.app_keypad = true,
            ([], b'>') => self.modes.app_keypad = false,
            ([slot @ (b'(' | b')' | b'*' | b'+')], set) => {
                let charset = match set {
                    b'0' => Charset::Graphics,
                    b'A' => Charset::British,
                    _ => Charset::Ascii,
                };
                self.cursor.charsets[usize::from(slot - b'(')] = charset;
            }
            ([b'#'], b'8') => self.align(),
            _ => {}
        }
    }

    fn osc(&mut self, fields: &[&[u8]], bell: bool) {
        self.command(fields, bell);
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], ignored: bool, action: u8) {
        self.dcs = (!ignored)
            .then(|| osc::Dcs::start(params, intermediates, action))
            .flatten();
    }

    fn put(&mut self, byte: u8) {
        if let Some(dcs) = &mut self.dcs {
            dcs.push(byte);
        }
    }

    fn unhook(&mut self) {
        if let Some(dcs) = self.dcs.take() {
            self.device_control(&dcs);
        }
    }
}
