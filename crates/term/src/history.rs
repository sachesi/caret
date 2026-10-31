//! The lines that scrolled off the top of the screen, kept as the program wrote them before
//! the terminal wrapped them, and wrapped again only for the rows someone looks at.

use std::collections::VecDeque;

use crate::grid::{Row, Text};
use crate::style::{Cell, Flags, Style};

/// A line as the program wrote it. Cells are mostly one narrow character each, so the text
/// is one string, and the cells that are not are listed apart.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Line {
    text: String,
    /// The number of cells, wide characters counting two.
    cells: u32,
    /// The first cells of characters two cells wide; the second cell has no text.
    wide: Vec<u32>,
    /// Cells whose text is more than one character, and its length in bytes.
    clusters: Vec<(u32, u32)>,
    /// Each style and the number of cells it covers, in order.
    runs: Vec<(u32, Style)>,
    /// A prompt starts in this line.
    pub prompt: bool,
}

impl Line {
    /// The number of cells it takes, before wrapping.
    #[must_use]
    pub fn width(&self) -> usize {
        self.cells as usize
    }

    /// The rows it takes when wrapped `columns` wide.
    #[must_use]
    pub fn rows(&self, columns: usize) -> usize {
        if self.wide.is_empty() {
            return self.width().div_ceil(columns.max(1)).max(1);
        }
        self.layout(columns, |_| {})
    }

    /// The cell each row starts with when wrapped `columns` wide.
    #[must_use]
    pub fn row_starts(&self, columns: usize) -> Vec<usize> {
        let mut starts = Vec::new();
        self.layout(columns, |start| starts.push(start));
        starts
    }

    /// Wraps the line as [`Line::wrap`] does, without making the rows: calls `start` with
    /// the cell each row starts with, and gives the number of rows.
    fn layout(&self, columns: usize, mut start: impl FnMut(usize)) -> usize {
        start(0);
        1 + self.walk(columns, 0, 0, start).0
    }

    /// Goes on wrapping from cell `cell`, which comes `column` cells into its row: calls
    /// `start` with the cell each further row starts with, and gives how many there are
    /// and how far into the last the line ends.
    fn walk(
        &self,
        columns: usize,
        mut cell: usize,
        mut column: usize,
        mut start: impl FnMut(usize),
    ) -> (usize, usize) {
        let columns = columns.max(1);
        let mut added = 0;
        let first = self.wide.partition_point(|&at| (at as usize) < cell);
        let wide = self.wide[first..].iter().map(|&at| at as usize);
        for (at, width) in wide.map(|at| (at, 2)).chain([(self.width(), 0)]) {
            // The narrow cells before the next wide one.
            while cell < at {
                if column == columns {
                    start(cell);
                    added += 1;
                    column = 0;
                }
                let taken = (at - cell).min(columns - column);
                column += taken;
                cell += taken;
            }
            // A wide character wider than a row is left out.
            if width == 0 || columns < width {
                cell = at + width;
                continue;
            }
            if column + width > columns {
                start(at);
                added += 1;
                column = 0;
            }
            column += width;
            cell = at + width;
        }
        (added, column)
    }

    /// Adds the cells of `row`, which follows the line: all of them where the terminal
    /// wrapped it, else those up to the last that shows anything.
    pub fn append(&mut self, row: &Row) {
        let end = if row.wrapped {
            row.len()
        } else {
            row.occupied()
        };
        self.prompt |= row.prompt;
        self.text.reserve(end);
        let mut column = 0;
        while column < end {
            let cell = row.cells[column];
            let flags = cell.style.flags;
            // Most cells are one narrow character each, in runs of one style.
            if !flags.intersects(Flags::WIDE | Flags::SPACER | Flags::CLUSTER) {
                let cells = &row.cells[column..end];
                let run = cells
                    .iter()
                    .position(|other| other.style != cell.style)
                    .unwrap_or(cells.len());
                push_text(&mut self.text, &cells[..run]);
                self.push_styles(cell.style, run as u32);
                self.cells += run as u32;
                column += run;
                continue;
            }
            // A blank left at the end of a row for a wide character that did not fit.
            if flags.contains(Flags::SPACER)
                && !(column > 0 && row.cells[column - 1].style.flags.contains(Flags::WIDE))
            {
                column += 1;
                continue;
            }
            if flags.contains(Flags::SPACER) {
                self.push_styles(cell.style, 1);
                self.cells += 1;
                column += 1;
                continue;
            }
            match row.text(column) {
                Text::Char(c) => self.text.push(c),
                Text::Cluster(text) => {
                    self.clusters.push((self.cells, text.len() as u32));
                    self.text.push_str(text);
                }
            }
            if flags.contains(Flags::WIDE) {
                self.wide.push(self.cells);
            }
            self.push_styles(cell.style, 1);
            self.cells += 1;
            column += 1;
        }
    }

    fn push_styles(&mut self, mut style: Style, cells: u32) {
        // Which cells are wide or carry marks is kept apart from the style.
        style
            .flags
            .remove(Flags::WIDE | Flags::SPACER | Flags::CLUSTER);
        match self.runs.last_mut() {
            Some((count, last)) if *last == style => *count += cells,
            _ => self.runs.push((cells, style)),
        }
    }

    /// The line wrapped into rows `columns` wide, padded with blanks of `pad`.
    #[must_use]
    pub fn wrap(&self, columns: usize, pad: Style) -> Vec<Row> {
        let columns = columns.max(1);
        let mut rows = vec![Row::new(columns, pad)];
        rows[0].prompt = self.prompt;
        let mut column = 0;
        let mut dropped = false;
        self.each_cell(|cell, text| {
            if cell.style.flags.contains(Flags::SPACER) {
                if !dropped {
                    let last = rows.len() - 1;
                    rows[last].set(column, cell);
                    column += 1;
                }
                return;
            }
            let width = if cell.style.flags.contains(Flags::WIDE) {
                2
            } else {
                1
            };
            // A character wider than a row is left out, with its second half.
            dropped = width > columns;
            if dropped {
                return;
            }
            if column + width > columns {
                let last = rows.len() - 1;
                if column < columns {
                    // The cell a wide character did not fit in stays blank.
                    let mut padding = Cell::blank(pad);
                    padding.style.flags.insert(Flags::SPACER);
                    rows[last].set(column, padding);
                }
                rows[last].wrapped = true;
                rows.push(Row::new(columns, pad));
                column = 0;
            }
            let last = rows.len() - 1;
            match text {
                Some(text) => rows[last].set_cluster(column, cell, text),
                None => rows[last].set(column, cell),
            }
            column += 1;
        });
        rows
    }

    /// Calls `each` with every cell in order, and its text where that is more than its
    /// character.
    fn each_cell(&self, mut each: impl FnMut(Cell, Option<&str>)) {
        let mut bytes = 0;
        let mut wide = self.wide.iter().peekable();
        let mut clusters = self.clusters.iter().peekable();
        let mut runs = self.runs.iter();
        let mut run = runs.next().copied().unwrap_or((u32::MAX, Style::default()));
        let mut in_run = 0;
        let mut spacer_next = false;
        for index in 0..self.cells {
            if in_run == run.0 {
                run = runs.next().copied().unwrap_or((u32::MAX, Style::default()));
                in_run = 0;
            }
            in_run += 1;
            let mut style = run.1;
            if spacer_next {
                spacer_next = false;
                style.flags.insert(Flags::SPACER);
                each(Cell::blank(style), None);
                continue;
            }
            if wide.next_if(|&&at| at == index).is_some() {
                style.flags.insert(Flags::WIDE);
                spacer_next = true;
            }
            if let Some(&(_, length)) = clusters.next_if(|&&(at, _)| at == index) {
                let text = &self.text[bytes..bytes + length as usize];
                bytes += length as usize;
                let c = text.chars().next().unwrap_or(' ');
                each(Cell { c, style }, Some(text));
            } else {
                let c = self.text[bytes..].chars().next().unwrap_or(' ');
                bytes += c.len_utf8();
                each(Cell { c, style }, None);
            }
        }
    }

    /// The text and style of each cell, in order; the second half of a wide character has
    /// no text.
    #[must_use]
    pub fn cells(&self) -> Vec<(&str, Style)> {
        let mut cells = Vec::with_capacity(self.width());
        let mut bytes = 0;
        self.each_cell(|cell, text| {
            if cell.style.flags.contains(Flags::SPACER) {
                cells.push(("", cell.style));
                return;
            }
            let length = text.map_or(cell.c.len_utf8(), str::len);
            cells.push((&self.text[bytes..bytes + length], cell.style));
            bytes += length;
        });
        cells
    }

    /// The bytes it holds, roughly, for keeping history within its memory.
    #[must_use]
    pub fn footprint(&self) -> usize {
        self.text.len()
            + self.wide.len() * 4
            + self.clusters.len() * 8
            + self.runs.len() * size_of::<(u32, Style)>()
    }
}

/// Adds the characters of `cells`. Every row that scrolls comes through here and nearly all
/// its text is ASCII, which goes in as a whole rather than a character and a capacity check
/// at a time.
fn push_text(text: &mut String, cells: &[Cell]) {
    let mut bytes = [0; 256];
    for chunk in cells.chunks(bytes.len()) {
        let mut ascii = true;
        for (byte, cell) in bytes.iter_mut().zip(chunk) {
            *byte = cell.c as u8;
            ascii &= cell.c.is_ascii();
        }
        match std::str::from_utf8(&bytes[..chunk.len()]) {
            Ok(ascii_text) if ascii => text.push_str(ascii_text),
            _ => text.extend(chunk.iter().map(|cell| cell.c)),
        }
    }
}

/// The lines above the screen, oldest first.
#[derive(Debug, Clone, Default)]
pub struct History {
    lines: VecDeque<Line>,
    /// The serial number of the oldest line; each line after has the next.
    first: u64,
    /// The row each line starts at, wrapped `columns` wide, counted from a row that stays
    /// put as the oldest lines go.
    starts: VecDeque<usize>,
    /// The row after the last line, counted the same way.
    end: usize,
    columns: usize,
    /// The rows it keeps at most, at the current width.
    limit: usize,
    /// The last line goes on in the next row pushed, which the terminal wrapped it into.
    open: bool,
    /// How far into its last row the last line ends. A row of the screen can be shorter
    /// than the history's, when a wide character did not fit at its end, so what a row
    /// brings may fit in the row the line ended in.
    tail: usize,
}

impl History {
    #[must_use]
    pub fn new(columns: usize, limit: usize) -> Self {
        Self {
            columns,
            limit,
            ..Self::default()
        }
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.lines.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.lines.is_empty()
    }

    /// The rows the lines take at the current width.
    #[must_use]
    pub fn rows(&self) -> usize {
        self.end - self.starts.front().copied().unwrap_or(self.end)
    }

    /// The row the line at `index` starts at, counted from the top.
    #[must_use]
    pub fn row_of(&self, index: usize) -> usize {
        self.starts[index] - self.starts[0]
    }

    /// The line `row` is in, counted from the top, and the row in that line; None past
    /// the last.
    #[must_use]
    pub fn at_row(&self, row: usize) -> Option<(usize, usize)> {
        let row = self.starts.front()? + row;
        if row >= self.end {
            return None;
        }
        let index = self.starts.partition_point(|&start| start <= row) - 1;
        Some((index, row - self.starts[index]))
    }

    /// The serial number of the line at `index`, which stays its own as older lines go.
    #[must_use]
    pub fn serial(&self, index: usize) -> u64 {
        self.first + index as u64
    }

    /// The index of the line with serial number `serial`, if it is still kept.
    #[must_use]
    pub fn index_of(&self, serial: u64) -> Option<usize> {
        let index = serial.checked_sub(self.first)? as usize;
        (index < self.lines.len()).then_some(index)
    }

    #[must_use]
    pub fn line(&self, index: usize) -> &Line {
        &self.lines[index]
    }

    #[must_use]
    pub fn lines(&self) -> impl DoubleEndedIterator<Item = &Line> + ExactSizeIterator {
        self.lines.iter()
    }

    /// Takes in a row moved off the top of the screen.
    pub fn push(&mut self, row: &Row) {
        if self.limit == 0 {
            // Lines are still numbered, so that the screen's keep their numbers.
            if !self.open {
                self.first += 1;
            }
            self.open = row.wrapped;
            return;
        }
        // A line that never ends, from a program that writes no newlines, is cut where
        // it fills the history, so that its older rows can go as other lines' do.
        let open = self.open
            && self
                .starts
                .back()
                .is_some_and(|&start| self.end - start < self.limit);
        if open && let Some(last) = self.lines.back_mut() {
            // Only what the row brings is wrapped: a line can be as long as the history.
            let before = last.width();
            last.append(row);
            let (added, tail) = last.walk(self.columns, before, self.tail, |_| {});
            self.end += added;
            self.tail = tail;
        } else {
            let mut line = Line::default();
            line.append(row);
            let (added, tail) = line.walk(self.columns, 0, 0, |_| {});
            self.starts.push_back(self.end);
            self.end += 1 + added;
            self.tail = tail;
            self.lines.push_back(line);
        }
        self.open = row.wrapped;
        self.trim();
    }

    /// Takes back the newest line, for a resize to put on the screen again.
    pub fn pop(&mut self) -> Option<Line> {
        let line = self.lines.pop_back()?;
        if let Some(start) = self.starts.pop_back() {
            self.end = start;
        }
        self.open = false;
        Some(line)
    }

    /// Whether the newest line goes on in the screen's first row.
    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Counts rows at `columns` wide from now on. No line is wrapped again until shown.
    pub fn set_columns(&mut self, columns: usize) {
        self.columns = columns;
        self.end = 0;
        for (line, start) in self.lines.iter().zip(&mut self.starts) {
            *start = self.end;
            self.end += line.rows(columns);
        }
        self.tail = self
            .lines
            .back()
            .map_or(0, |line| line.walk(columns, 0, 0, |_| {}).1);
        self.trim();
    }

    pub fn set_limit(&mut self, limit: usize) {
        self.limit = limit;
        if limit == 0 {
            self.clear();
        }
        self.trim();
    }

    pub fn clear(&mut self) {
        self.first += self.lines.len() as u64;
        self.lines.clear();
        self.starts.clear();
        self.end = 0;
        self.open = false;
    }

    fn trim(&mut self) {
        while self.rows() > self.limit && self.lines.len() > 1 {
            self.lines.pop_front();
            self.starts.pop_front();
            self.first += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::style::Color;

    fn row(text: &str, columns: usize, wrapped: bool) -> Row {
        let mut row = Row::new(columns, Style::default());
        let mut column = 0;
        for c in text.chars() {
            if c == '日' {
                let wide = Style {
                    flags: Flags::WIDE,
                    ..Style::default()
                };
                row.set(column, Cell { c, style: wide });
                let spacer = Style {
                    flags: Flags::SPACER,
                    ..Style::default()
                };
                row.set(column + 1, Cell::blank(spacer));
                column += 2;
            } else {
                row.set(
                    column,
                    Cell {
                        c,
                        style: Style::default(),
                    },
                );
                column += 1;
            }
        }
        row.wrapped = wrapped;
        row
    }

    fn texts(rows: &[Row]) -> Vec<String> {
        rows.iter()
            .map(|row| {
                row.cells
                    .iter()
                    .filter(|cell| !cell.style.flags.contains(Flags::SPACER))
                    .map(|cell| cell.c)
                    .collect::<String>()
                    .trim_end()
                    .to_owned()
            })
            .collect()
    }

    #[test]
    fn wrapped_rows_join_into_one_line() {
        let mut history = History::new(4, 100);
        history.push(&row("abcd", 4, true));
        history.push(&row("ef", 4, false));
        history.push(&row("gh", 4, false));
        assert_eq!(history.len(), 2);
        assert_eq!(history.line(0).width(), 6);
        assert_eq!(history.rows(), 3);
        assert_eq!(
            texts(&history.line(0).wrap(3, Style::default())),
            ["abc", "def"]
        );
        assert_eq!(
            texts(&history.line(0).wrap(8, Style::default())),
            ["abcdef"]
        );
    }

    #[test]
    fn wide_characters_and_marks_survive_wrapping() {
        let mut line = Line::default();
        let mut marked = row("a日b", 5, false);
        marked.add_mark(3, '\u{301}');
        line.append(&marked);
        let rows = line.wrap(2, Style::default());
        assert_eq!(texts(&rows), ["a", "日", "b"]);
        assert!(rows[0].cells[1].style.flags.contains(Flags::SPACER));
        assert_eq!(rows[2].text(0), Text::Cluster("b\u{301}"));
        let mut again = Line::default();
        for row in &rows {
            again.append(row);
        }
        assert_eq!(again, line);
    }

    #[test]
    fn styles_are_kept_as_runs() {
        let mut styled = row("abcd", 4, false);
        let red = Style {
            fg: Color::Indexed(1),
            ..Style::default()
        };
        styled.cells[1].style = red;
        styled.cells[2].style = red;
        let mut line = Line::default();
        line.append(&styled);
        assert_eq!(line.runs.len(), 3);
        let rows = line.wrap(4, Style::default());
        assert_eq!(rows[0].cells[2].style, red);
        assert_eq!(rows[0].cells[3].style, Style::default());
    }

    #[test]
    fn the_oldest_lines_go_past_the_limit() {
        let mut history = History::new(4, 3);
        for text in ["a", "b", "c", "d"] {
            history.push(&row(text, 4, false));
        }
        assert_eq!(history.len(), 3);
        assert_eq!(history.serial(0), 1);
        assert_eq!(history.index_of(0), None);
        assert_eq!(history.index_of(3), Some(2));
    }

    #[test]
    fn a_line_that_never_ends_keeps_to_the_limit() {
        let mut history = History::new(4, 3);
        for _ in 0..10 {
            history.push(&row("abcd", 4, true));
            assert!(history.rows() <= 3, "{} rows", history.rows());
        }
        assert_eq!(history.line(history.len() - 1).width(), 4);
    }

    #[test]
    fn rows_joined_to_a_line_are_counted_as_its_wrapping_makes_them() {
        let output = "日a日日日日bc日日日日日日日日日日d\r\n\
                      abcdefghijklmnopqrstuvwxyz\x1b[K\r\nabc\x1b[2K\r\n日日日日日\r\n";
        for columns in 2..9 {
            let mut term = crate::Term::new(columns, 2, crate::Options::default());
            term.advance(output.repeat(3).as_bytes());
            let history = term.history();
            let laid_out: usize = history.lines().map(|line| line.rows(columns)).sum();
            assert_eq!(history.rows(), laid_out, "at {columns} columns");
        }
    }

    #[test]
    fn a_line_with_shorter_rows_is_counted_as_it_wraps_in_the_history() {
        // Each wide character that does not fit leaves a row one cell short, so the rows
        // after it take up room in those before when wrapped again.
        let mut term = crate::Term::new(3, 1, crate::Options::default());
        term.advance("ab日\rab日日".as_bytes());
        let history = term.history();
        let laid_out: usize = history.lines().map(|line| line.rows(3)).sum();
        assert_eq!(history.rows(), laid_out);
    }

    #[test]
    fn rows_are_counted_as_wrapping_makes_them() {
        for text in [
            "",
            "abc",
            "日日日",
            "a日b日日c",
            "ab日日日日日d",
            "日a日a日a日a",
        ] {
            let mut line = Line::default();
            line.append(&row(text, 40, false));
            for columns in 1..12 {
                let rows = line.wrap(columns, Style::default());
                assert_eq!(line.rows(columns), rows.len(), "{text} at {columns}");
                // One column wide, wide characters are left out of the rows.
                if columns == 1 {
                    continue;
                }
                let mut again = Line::default();
                let mut starts = Vec::new();
                for row in &rows {
                    starts.push(again.width());
                    again.append(row);
                }
                assert_eq!(line.row_starts(columns), starts, "{text} at {columns}");
            }
        }
    }

    #[test]
    fn a_narrower_width_counts_more_rows() {
        let mut history = History::new(10, 100);
        history.push(&row("abcdefgh", 10, false));
        assert_eq!(history.rows(), 1);
        history.set_columns(3);
        assert_eq!(history.rows(), 3);
    }
}
