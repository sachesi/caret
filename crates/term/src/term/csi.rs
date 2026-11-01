//! Control sequences: CSI, its parameters and final byte.

use super::{
    CursorShape, CursorStyle, MAX_KEYBOARD_STACK, MAX_TEXT_BYTES, MAX_TITLES, MouseEncoding,
    MouseMode, Term,
};
use crate::parser::Params;
use crate::style::{Cell, Color, Flags, Style, Underline};

/// What DECRQM answers for a mode.
#[derive(Clone, Copy)]
enum ModeState {
    Unknown = 0,
    Set = 1,
    Reset = 2,
}

impl From<bool> for ModeState {
    fn from(set: bool) -> Self {
        if set { Self::Set } else { Self::Reset }
    }
}

impl Term {
    pub(super) fn control(&mut self, params: &Params, intermediates: &[u8], action: u8) {
        // Marks after a sequence that moved the cursor do not join the character before.
        if !matches!(action, b'm' | b'b') {
            self.forget_last_cell();
        }
        let count = |index: usize| usize::from(params.get_or(index, 1));
        match (intermediates, action) {
            ([], b'@') => {
                let (column, blank) = (self.cursor.column, self.blank());
                self.row().insert(column, count(0), blank);
                self.cursor.pending = false;
            }
            ([], b'A') => self.move_up(count(0)),
            ([], b'B' | b'e') => self.move_down(count(0)),
            ([], b'C' | b'a') => self.move_to_column(self.cursor.column + count(0)),
            ([], b'D') => self.move_to_column(self.cursor.column.saturating_sub(count(0))),
            ([], b'E') => {
                self.move_down(count(0));
                self.cursor.column = 0;
            }
            ([], b'F') => {
                self.move_up(count(0));
                self.cursor.column = 0;
            }
            ([], b'G' | b'`') => self.move_to_column(count(0) - 1),
            ([], b'H' | b'f') => self.goto(count(0) - 1, count(1) - 1),
            ([], b'I') => self.tab(count(0)),
            ([], b'J') => self.erase_display(params.get_or(0, 0)),
            ([], b'K') => self.erase_line(params.get_or(0, 0)),
            ([], b'L') => self.insert_lines(count(0)),
            ([], b'M') => self.delete_lines(count(0)),
            ([], b'P') => {
                let (column, blank) = (self.cursor.column, self.blank());
                self.row().delete(column, count(0), blank);
                self.cursor.pending = false;
            }
            ([], b'S') => self.scroll_up(count(0)),
            // With five parameters it starts mouse tracking on old xterms, which no one uses.
            ([], b'T') if params.len() <= 1 => self.scroll_down(count(0)),
            ([], b'X') => {
                let (column, blank) = (self.cursor.column, self.blank());
                let end = column + count(0);
                self.row().clear(column, end, blank);
                self.cursor.pending = false;
            }
            ([], b'Z') => self.back_tab(count(0)),
            ([], b'b') => self.repeat(count(0)),
            ([], b'c') if params.get_or(0, 0) == 0 => self.reply("\x1b[?62;22c"),
            ([b'>'], b'c') if params.get_or(0, 0) == 0 => {
                let version = version_number(&self.options.version);
                self.reply(&format!("\x1b[>1;{version};0c"));
            }
            ([], b'd') => {
                let column = self.cursor.column;
                self.goto(count(0) - 1, column);
            }
            ([], b'g') => match params.get_or(0, 0) {
                0 => {
                    let column = self.cursor.column;
                    self.tabs[column] = false;
                }
                3 => self.tabs.fill(false),
                _ => {}
            },
            ([], b'h') => self.set_ansi_modes(params, true),
            ([], b'l') => self.set_ansi_modes(params, false),
            ([b'?'], b'h') => self.set_private_modes(params, true),
            ([b'?'], b'l') => self.set_private_modes(params, false),
            ([], b'm') => self.select_graphic_rendition(params),
            ([], b'n') => self.status_report(params.get_or(0, 0)),
            ([b'?'], b'n') if params.get_or(0, 0) == 6 => {
                let (line, column) = (self.origin_line() + 1, self.cursor.column + 1);
                self.reply(&format!("\x1b[?{line};{column}R"));
            }
            ([b'?'], b'n') if params.get_or(0, 0) == 996 => self.report_color_scheme(),
            ([], b'r') => self.set_region(params),
            ([], b's') => self.save_cursor(),
            ([], b't') => self.window_operation(params),
            ([], b'u') => self.restore_cursor(),
            ([b'$'], b'p') => self.report_mode(params.get_or(0, 0), false),
            ([b'?', b'$'], b'p') => self.report_mode(params.get_or(0, 0), true),
            ([b'>'], b'q') => {
                let version = self.options.version.clone();
                self.reply(&format!("\x1bP>|{version}\x1b\\"));
            }
            ([b' '], b'q') => self.set_cursor_style(params.get_or(0, 0)),
            ([b'!'], b'p') => self.soft_reset(),
            ([b'?'], b'u') => {
                let flags = self.keyboard_flags();
                self.reply(&format!("\x1b[?{flags}u"));
            }
            ([b'>'], b'u') => self.push_keyboard(params.get_or(0, 0)),
            ([b'<'], b'u') => self.pop_keyboard(count(0)),
            ([b'='], b'u') => self.set_keyboard(params.get_or(0, 0), params.get_or(1, 1)),
            _ => {}
        }
    }

    fn repeat(&mut self, count: usize) {
        let Some((c, _, _)) = self.last else {
            return;
        };
        let width = unicode_width::UnicodeWidthChar::width(c).unwrap_or(1);
        // More than a screenful shows nothing more, and each of these takes six bytes: a
        // megabyte of them would otherwise write cells for a minute with the keyboard waiting.
        for _ in 0..count.min(self.columns() * self.lines()) {
            self.place(c, width);
        }
    }

    fn erase_display(&mut self, mode: u16) {
        let blank = self.blank();
        let (line, column, lines, columns) = (
            self.cursor.line,
            self.cursor.column,
            self.lines(),
            self.columns(),
        );
        match mode {
            0 => {
                // As for erasing the line: nothing follows text that reached the last
                // column yet.
                if !self.cursor.pending {
                    self.row().clear(column, columns, blank);
                    self.row().wrapped = false;
                }
                for below in line + 1..lines {
                    self.grid().row_mut(below).reset(blank);
                }
                if (line, column) == (0, 0) && !self.alt_active {
                    self.history.end_line();
                }
            }
            1 => {
                for above in 0..line {
                    self.grid().row_mut(above).reset(blank);
                }
                self.row().clear(0, column + 1, blank);
            }
            2 => {
                // The text cleared off the main screen stays in the history.
                if !self.alt_active {
                    let rows = self.main.rows();
                    let used = rows
                        .iter()
                        .rposition(|row| row.occupied() > 0 || row.prompt)
                        .map_or(0, |last| last + 1);
                    for (index, row) in rows.iter().take(used).enumerate() {
                        if index + 1 == used && row.wrapped {
                            let mut last = row.clone();
                            last.wrapped = false;
                            self.history.push(&last);
                        } else {
                            self.history.push(row);
                        }
                    }
                    self.history.end_line();
                }
                for each in 0..lines {
                    self.grid().row_mut(each).reset(blank);
                }
            }
            3 => self.history.clear(),
            _ => {}
        }
    }

    fn erase_line(&mut self, mode: u16) {
        // Erasing what follows text that reached the last column keeps its last character:
        // nothing follows it yet.
        if mode == 0 && self.cursor.pending {
            return;
        }
        let blank = self.blank();
        let (column, columns) = (self.cursor.column, self.columns());
        match mode {
            0 => {
                self.row().clear(column, columns, blank);
                self.row().wrapped = false;
            }
            1 => self.row().clear(0, column + 1, blank),
            2 => {
                let row = self.row();
                row.clear(0, columns, blank);
                row.wrapped = false;
            }
            _ => {}
        }
    }

    fn insert_lines(&mut self, count: usize) {
        if !(self.top..=self.bottom).contains(&self.cursor.line) {
            return;
        }
        let (line, bottom, blank) = (self.cursor.line, self.bottom, self.blank());
        self.grid().scroll_down(line, bottom, count, blank);
        self.cursor.column = 0;
        self.cursor.pending = false;
    }

    fn delete_lines(&mut self, count: usize) {
        if !(self.top..=self.bottom).contains(&self.cursor.line) {
            return;
        }
        let (line, bottom, blank) = (self.cursor.line, self.bottom, self.blank());
        self.grid().scroll_up(line, bottom, count, blank, |_| {});
        self.cursor.column = 0;
        self.cursor.pending = false;
    }

    fn set_region(&mut self, params: &Params) {
        let lines = self.lines();
        let top = usize::from(params.get_or(0, 1)) - 1;
        let bottom = usize::from(params.get_or(1, lines as u16)).min(lines) - 1;
        if top < bottom {
            self.top = top;
            self.bottom = bottom;
            self.goto(0, 0);
        }
    }

    /// Whether the terminal's own colours are dark or light, whatever the program set.
    pub(super) fn report_color_scheme(&mut self) {
        let scheme = if self.palette.base().is_dark() { 1 } else { 2 };
        self.reply(&format!("\x1b[?997;{scheme}n"));
    }

    fn status_report(&mut self, kind: u16) {
        match kind {
            5 => self.reply("\x1b[0n"),
            6 => {
                let (line, column) = (self.origin_line() + 1, self.cursor.column + 1);
                self.reply(&format!("\x1b[{line};{column}R"));
            }
            _ => {}
        }
    }

    fn window_operation(&mut self, params: &Params) {
        let (cell_width, cell_height) =
            (usize::from(self.cell_size.0), usize::from(self.cell_size.1));
        let (columns, lines) = (self.columns(), self.lines());
        match params.get_or(0, 0) {
            14 => {
                let (height, width) = (lines * cell_height, columns * cell_width);
                self.reply(&format!("\x1b[4;{height};{width}t"));
            }
            16 => self.reply(&format!("\x1b[6;{cell_height};{cell_width}t")),
            18 => self.reply(&format!("\x1b[8;{lines};{columns}t")),
            22 => {
                if self.titles.len() == MAX_TITLES {
                    self.titles.remove(0);
                }
                self.titles.push(self.title.clone());
            }
            23 => {
                if let Some(title) = self.titles.pop() {
                    self.set_title(title);
                }
            }
            _ => {}
        }
    }

    fn set_cursor_style(&mut self, kind: u16) {
        self.cursor_style = match kind {
            0 => None,
            1..=6 => Some(CursorStyle {
                shape: match kind {
                    1 | 2 => CursorShape::Block,
                    3 | 4 => CursorShape::Underline,
                    _ => CursorShape::Beam,
                },
                blinking: kind % 2 == 1,
            }),
            _ => return,
        };
    }

    fn soft_reset(&mut self) {
        let lines = self.lines();
        self.modes.insert = false;
        self.modes.origin = false;
        self.modes.autowrap = true;
        self.modes.show_cursor = true;
        self.modes.app_cursor = false;
        self.modes.app_keypad = false;
        self.top = 0;
        self.bottom = lines - 1;
        self.cursor.style = Style::default();
        self.cursor.charsets = Default::default();
        self.cursor.shifted = 0;
        self.cursor.single_shift = None;
        self.saved[usize::from(self.alt_active)] = super::Saved::default();
    }

    fn set_ansi_modes(&mut self, params: &Params, on: bool) {
        for group in params {
            match group[0] {
                4 => self.modes.insert = on,
                20 => self.modes.line_feed_new_line = on,
                _ => {}
            }
        }
    }

    fn set_private_modes(&mut self, params: &Params, on: bool) {
        for group in params {
            match group[0] {
                1 => self.modes.app_cursor = on,
                6 => {
                    self.modes.origin = on;
                    self.goto(0, 0);
                }
                7 => self.modes.autowrap = on,
                12 => {
                    let mut style = self.cursor_style();
                    style.blinking = on;
                    self.cursor_style = Some(style);
                }
                25 => self.modes.show_cursor = on,
                9 | 1000 => self.set_mouse(MouseMode::Click, on),
                1002 => self.set_mouse(MouseMode::Drag, on),
                1003 => self.set_mouse(MouseMode::Motion, on),
                1004 => self.modes.focus = on,
                1005 => self.set_mouse_encoding(MouseEncoding::Utf8, on),
                1006 => self.set_mouse_encoding(MouseEncoding::Sgr, on),
                1007 => self.modes.alternate_scroll = on,
                47 => self.switch_screen(on, false),
                1047 => {
                    if !on && self.alt_active {
                        let (columns, lines) = (self.columns(), self.lines());
                        self.alt.set_rows(
                            vec![crate::grid::Row::new(columns, Style::default()); lines],
                            columns,
                        );
                    }
                    self.switch_screen(on, false);
                }
                1048 => {
                    if on {
                        self.save_cursor();
                    } else {
                        self.restore_cursor();
                    }
                }
                // A program that leaves twice would otherwise move the cursor back.
                1049 if on == self.alt_active => {}
                1049 => {
                    if on {
                        self.save_cursor();
                        self.switch_screen(true, true);
                    } else {
                        self.switch_screen(false, false);
                        self.restore_cursor();
                    }
                }
                2004 => self.modes.bracketed_paste = on,
                2026 => self.modes.synchronized = on,
                2027 => self.modes.graphemes = on,
                2031 => self.modes.color_scheme_reports = on,
                _ => {}
            }
        }
    }

    fn switch_screen(&mut self, alt: bool, clear: bool) {
        if alt {
            self.enter_alt_screen(clear);
        } else {
            self.leave_alt_screen();
        }
    }

    fn set_mouse(&mut self, mode: MouseMode, on: bool) {
        if on {
            self.modes.mouse = mode;
        } else if self.modes.mouse == mode {
            self.modes.mouse = MouseMode::Off;
        }
    }

    fn set_mouse_encoding(&mut self, encoding: MouseEncoding, on: bool) {
        if on {
            self.modes.mouse_encoding = encoding;
        } else if self.modes.mouse_encoding == encoding {
            self.modes.mouse_encoding = MouseEncoding::Default;
        }
    }

    /// Answers DECRQM: whether mode `mode` is set, of the private ones or the ANSI ones.
    fn report_mode(&mut self, mode: u16, private: bool) {
        let modes = self.modes;
        let state = if private {
            match mode {
                1 => modes.app_cursor.into(),
                6 => modes.origin.into(),
                7 => modes.autowrap.into(),
                12 => self.cursor_style().blinking.into(),
                25 => modes.show_cursor.into(),
                9 | 1000 => (modes.mouse == MouseMode::Click).into(),
                1002 => (modes.mouse == MouseMode::Drag).into(),
                1003 => (modes.mouse == MouseMode::Motion).into(),
                1004 => modes.focus.into(),
                1005 => (modes.mouse_encoding == MouseEncoding::Utf8).into(),
                1006 => (modes.mouse_encoding == MouseEncoding::Sgr).into(),
                1007 => modes.alternate_scroll.into(),
                47 | 1047 | 1049 => self.alt_active.into(),
                2004 => modes.bracketed_paste.into(),
                2026 => modes.synchronized.into(),
                2027 => modes.graphemes.into(),
                2031 => modes.color_scheme_reports.into(),
                _ => ModeState::Unknown,
            }
        } else {
            match mode {
                4 => modes.insert.into(),
                20 => modes.line_feed_new_line.into(),
                _ => ModeState::Unknown,
            }
        };
        let marker = if private { "?" } else { "" };
        let state = state as u8;
        self.reply(&format!("\x1b[{marker}{mode};{state}$y"));
    }

    fn push_keyboard(&mut self, flags: u16) {
        let screen = usize::from(self.alt_active);
        let stack = &mut self.keyboard_stack[screen];
        if stack.len() == MAX_KEYBOARD_STACK {
            stack.remove(0);
        }
        stack.push(self.keyboard[screen]);
        self.keyboard[screen] = flags as u8 & 0x1f;
    }

    fn pop_keyboard(&mut self, count: usize) {
        let screen = usize::from(self.alt_active);
        for _ in 0..count {
            if let Some(flags) = self.keyboard_stack[screen].pop() {
                self.keyboard[screen] = flags;
            } else {
                self.keyboard[screen] = 0;
                break;
            }
        }
    }

    fn set_keyboard(&mut self, flags: u16, mode: u16) {
        let screen = usize::from(self.alt_active);
        let flags = flags as u8 & 0x1f;
        let current = &mut self.keyboard[screen];
        match mode {
            1 => *current = flags,
            2 => *current |= flags,
            3 => *current &= !flags,
            _ => {}
        }
    }

    /// Fills the screen with `E`, as DECALN does to line a monitor up.
    pub(super) fn align(&mut self) {
        let (columns, lines) = (self.columns(), self.lines());
        self.top = 0;
        self.bottom = lines - 1;
        for line in 0..lines {
            let row = self.grid().row_mut(line);
            *row = crate::grid::Row::new(columns, Style::default());
            for column in 0..columns {
                row.set(
                    column,
                    Cell {
                        c: 'E',
                        style: Style::default(),
                    },
                );
            }
        }
        self.goto(0, 0);
    }

    fn select_graphic_rendition(&mut self, params: &Params) {
        if params.is_empty() {
            self.cursor.style = Style {
                link: self.cursor.style.link,
                ..Style::default()
            };
            return;
        }
        let mut groups = params.iter();
        while let Some(group) = groups.next() {
            let style = &mut self.cursor.style;
            let flags = &mut style.flags;
            match group[0] {
                0 => {
                    *style = Style {
                        link: style.link,
                        ..Style::default()
                    };
                }
                1 => flags.insert(Flags::BOLD),
                2 => flags.insert(Flags::DIM),
                3 => flags.insert(Flags::ITALIC),
                4 => flags.set_underline(match group.get(1).copied() {
                    None | Some(1) => Underline::Single,
                    Some(2) => Underline::Double,
                    Some(3) => Underline::Curly,
                    Some(4) => Underline::Dotted,
                    Some(5) => Underline::Dashed,
                    Some(_) => Underline::None,
                }),
                5 | 6 => flags.insert(Flags::BLINK),
                7 => flags.insert(Flags::INVERSE),
                8 => flags.insert(Flags::HIDDEN),
                9 => flags.insert(Flags::STRIKEOUT),
                21 => flags.set_underline(Underline::Double),
                22 => flags.remove(Flags::BOLD | Flags::DIM),
                23 => flags.remove(Flags::ITALIC),
                24 => flags.set_underline(Underline::None),
                25 => flags.remove(Flags::BLINK),
                27 => flags.remove(Flags::INVERSE),
                28 => flags.remove(Flags::HIDDEN),
                29 => flags.remove(Flags::STRIKEOUT),
                value @ 30..=37 => style.fg = Color::indexed(value as u8 - 30),
                value @ 40..=47 => style.bg = Color::indexed(value as u8 - 40),
                value @ 90..=97 => style.fg = Color::indexed(value as u8 - 90 + 8),
                value @ 100..=107 => style.bg = Color::indexed(value as u8 - 100 + 8),
                39 => style.fg = Color::DEFAULT,
                49 => style.bg = Color::DEFAULT,
                59 => style.underline = Color::DEFAULT,
                kind @ (38 | 48 | 58) => {
                    let color = if group.len() > 1 {
                        extended_color(&group[1..], true)
                    } else {
                        extended_from_groups(&mut groups)
                    };
                    if let Some(color) = color {
                        match kind {
                            38 => style.fg = color,
                            48 => style.bg = color,
                            _ => style.underline = color,
                        }
                    }
                }
                _ => {}
            }
        }
    }

    pub(super) fn set_title(&mut self, mut title: String) {
        title.truncate(title.floor_char_boundary(MAX_TEXT_BYTES));
        if title != self.title {
            self.title.clone_from(&title);
            self.events.push(super::Event::Title(title));
        }
    }
}

/// A colour from the values after 38, 48 or 58 in one group, split by `:`: `5:n`, or `2`
/// then red, green and blue, with or without a colour space before them.
fn extended_color(values: &[u16], colon: bool) -> Option<Color> {
    match values {
        [5, index, ..] => Some(Color::indexed(*index as u8)),
        [2, _, r, g, b, ..] if colon => Some(Color::rgb(*r as u8, *g as u8, *b as u8)),
        [2, r, g, b] => Some(Color::rgb(*r as u8, *g as u8, *b as u8)),
        _ => None,
    }
}

/// A colour given in the groups after 38, 48 or 58, split by `;`, taking those it uses.
fn extended_from_groups<'a>(groups: &mut impl Iterator<Item = &'a [u16]>) -> Option<Color> {
    match groups.next()?.first()? {
        5 => Some(Color::indexed(*groups.next()?.first()? as u8)),
        2 => {
            let mut next = || groups.next().and_then(|group| group.first().copied());
            let (r, g, b) = (next()?, next()?, next()?);
            Some(Color::rgb(r as u8, g as u8, b as u8))
        }
        _ => None,
    }
}

/// `name 1.2.3` as the number DA2 gives a version in: 10203.
fn version_number(version: &str) -> u32 {
    let numbers = version.rsplit(' ').next().unwrap_or_default();
    let mut parts = numbers
        .split('.')
        .map(|part| part.parse::<u32>().unwrap_or(0));
    let major = parts.next().unwrap_or(0);
    let minor = parts.next().unwrap_or(0);
    let patch = parts.next().unwrap_or(0);
    major * 10_000 + minor * 100 + patch
}
