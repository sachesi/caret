//! The visible screen: rows of cells, and which rows changed since they were last taken.

use crate::style::{Cell, Flags, Style};

#[derive(Debug, Clone, Default)]
pub struct Row {
    pub cells: Vec<Cell>,
    /// The line goes on in the next row, which the terminal wrapped it into.
    pub wrapped: bool,
    /// A prompt starts in this row.
    pub prompt: bool,
    /// For each cell with marks after its character, by column, the whole text.
    clusters: Vec<(u16, Box<str>)>,
    /// No cell from here on shows anything, so that the end of the text is found without
    /// looking at every blank after it.
    used: usize,
}

impl Row {
    #[must_use]
    pub fn new(columns: usize, style: Style) -> Self {
        Self {
            cells: vec![Cell::blank(style); columns],
            used: if style == Style::default() {
                0
            } else {
                columns
            },
            ..Self::default()
        }
    }

    /// Cells up to `end` may show something now.
    fn touch(&mut self, end: usize) {
        self.used = self.used.max(end).min(self.cells.len());
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.cells.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.cells.is_empty()
    }

    /// The text of the cell at `column`: its character and any marks after it.
    #[must_use]
    pub fn text(&self, column: usize) -> Text<'_> {
        let cell = &self.cells[column];
        if cell.style.flags.contains(Flags::CLUSTER)
            && let Some((_, text)) = self
                .clusters
                .iter()
                .find(|(at, _)| usize::from(*at) == column)
        {
            return Text::Cluster(text);
        }
        Text::Char(cell.c)
    }

    /// Adds `mark` to the text of the cell at `column`.
    pub fn add_mark(&mut self, column: usize, mark: char) {
        self.touch(column + 1);
        let cell = &mut self.cells[column];
        if cell.style.flags.contains(Flags::CLUSTER) {
            if let Some((_, text)) = self
                .clusters
                .iter_mut()
                .find(|(at, _)| usize::from(*at) == column)
            {
                let mut longer = String::from(&**text);
                longer.push(mark);
                *text = longer.into_boxed_str();
            }
            return;
        }
        cell.style.flags.insert(Flags::CLUSTER);
        let mut text = String::from(cell.c);
        text.push(mark);
        self.clusters.push((column as u16, text.into_boxed_str()));
    }

    /// Puts `cell` at `column`.
    pub fn set(&mut self, column: usize, cell: Cell) {
        self.forget_cluster(column);
        self.cells[column] = cell;
        self.touch(column + 1);
    }

    /// Puts `cell` at `column`, its text `text` rather than its character alone.
    pub fn set_cluster(&mut self, column: usize, mut cell: Cell, text: &str) {
        self.forget_cluster(column);
        cell.style.flags.insert(Flags::CLUSTER);
        self.cells[column] = cell;
        self.clusters.push((column as u16, text.into()));
        self.touch(column + 1);
    }

    fn forget_cluster(&mut self, column: usize) {
        if self.cells[column].style.flags.contains(Flags::CLUSTER) {
            self.clusters.retain(|(at, _)| usize::from(*at) != column);
        }
    }

    /// Makes it a blank row of `style`, keeping its memory.
    pub fn reset(&mut self, style: Style) {
        self.cells.fill(Cell::blank(style));
        self.clusters.clear();
        self.wrapped = false;
        self.prompt = false;
        self.used = if style == Style::default() {
            0
        } else {
            self.cells.len()
        };
    }

    /// Blanks the cells from `start` to `end`, and the other half of any wide character
    /// the range cuts in two.
    pub fn clear(&mut self, start: usize, end: usize, style: Style) {
        let end = end.min(self.cells.len());
        if start >= end {
            return;
        }
        self.split_wide(start, end);
        for column in start..end {
            self.forget_cluster(column);
            self.cells[column] = Cell::blank(style);
        }
        if style != Style::default() {
            self.touch(end);
        }
    }

    /// Blanks the halves outside `start..end` of wide characters it cuts, keeping their
    /// colours.
    pub fn split_wide(&mut self, start: usize, end: usize) {
        let blank = |cell: Cell| {
            let mut style = cell.style;
            style
                .flags
                .remove(Flags::WIDE | Flags::SPACER | Flags::CLUSTER);
            Cell::blank(style)
        };
        if start > 0
            && self.cells[start].style.flags.contains(Flags::SPACER)
            && self.cells[start - 1].style.flags.contains(Flags::WIDE)
        {
            self.forget_cluster(start - 1);
            self.cells[start - 1] = blank(self.cells[start - 1]);
        }
        if end < self.cells.len() && self.cells[end - 1].style.flags.contains(Flags::WIDE) {
            self.cells[end] = blank(self.cells[end]);
            self.touch(end + 1);
        }
    }

    /// Moves the cells from `column` on right by `count`, blanking those it opens and
    /// dropping those pushed past the end.
    pub fn insert(&mut self, column: usize, count: usize, style: Style) {
        let len = self.cells.len();
        if column >= len || count == 0 {
            return;
        }
        let count = count.min(len - column);
        if self.cells[column].style.flags.contains(Flags::SPACER) {
            self.clear(column - 1, column + 1, style);
        }
        let kept = len - count;
        // A wide character whose second half would be pushed off goes whole.
        if kept > column && self.cells[kept - 1].style.flags.contains(Flags::WIDE) {
            self.clear(kept - 1, kept, style);
        }
        for at in kept..len {
            self.forget_cluster(at);
        }
        self.cells[column..].rotate_right(count);
        self.move_clusters(column, count as isize);
        for cell in &mut self.cells[column..column + count] {
            *cell = Cell::blank(style);
        }
        let used = self.used + count;
        self.touch(used);
        if style != Style::default() {
            self.touch(column + count);
        }
    }

    /// Removes `count` cells at `column`, moving those after it left and blanking the
    /// ones it opens at the end.
    pub fn delete(&mut self, column: usize, count: usize, style: Style) {
        let len = self.cells.len();
        if column >= len || count == 0 {
            return;
        }
        let count = count.min(len - column);
        self.split_wide(column, column + count);
        for at in column..column + count {
            self.forget_cluster(at);
        }
        self.cells[column..].rotate_left(count);
        self.move_clusters(column + count, -(count as isize));
        for cell in &mut self.cells[len - count..] {
            *cell = Cell::blank(style);
        }
        if style != Style::default() {
            self.touch(len);
        }
    }

    /// Moves the clusters of the cells from `from` on by `by` columns, after the cells
    /// themselves moved.
    fn move_clusters(&mut self, from: usize, by: isize) {
        for (at, _) in &mut self.clusters {
            if usize::from(*at) >= from {
                *at = (*at as isize + by) as u16;
            }
        }
    }

    /// Makes the row `columns` wide, cutting or padding it with `style`'s blanks.
    pub fn resize(&mut self, columns: usize, style: Style) {
        if columns < self.cells.len() {
            if columns > 0 && self.cells[columns - 1].style.flags.contains(Flags::WIDE) {
                self.cells[columns - 1] = Cell::blank(style);
            }
            self.cells.truncate(columns);
            self.clusters.retain(|(at, _)| usize::from(*at) < columns);
            self.used = self.used.min(columns);
        } else {
            self.cells.resize(columns, Cell::blank(style));
        }
        if style != Style::default() {
            self.touch(columns);
        }
    }

    /// The cells it adds to its line where the line goes on past it: all but a blank left
    /// at the end for a wide character that did not fit.
    #[must_use]
    pub fn line_cells(&self) -> usize {
        let padded = self.cells.len() >= 2
            && self.cells[self.cells.len() - 1]
                .style
                .flags
                .contains(Flags::SPACER)
            && !self.cells[self.cells.len() - 2]
                .style
                .flags
                .contains(Flags::WIDE);
        self.cells.len() - usize::from(padded)
    }

    /// The number of cells up to the last that shows anything.
    #[must_use]
    pub fn occupied(&self) -> usize {
        self.cells[..self.used]
            .iter()
            .rposition(|cell| !cell.is_blank())
            .map_or(0, |last| last + 1)
    }
}

/// The text of a cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Text<'a> {
    Char(char),
    Cluster(&'a str),
}

#[derive(Debug, Clone)]
pub struct Grid {
    rows: Vec<Row>,
    columns: usize,
    dirty: Vec<bool>,
    /// The highest row output wrote to since it was last taken, where that row is now:
    /// above the screen for one that scrolled off it since.
    written: Option<isize>,
}

impl Grid {
    #[must_use]
    pub fn new(columns: usize, lines: usize) -> Self {
        Self {
            rows: vec![Row::new(columns, Style::default()); lines],
            columns,
            dirty: vec![true; lines],
            written: Some(0),
        }
    }

    #[must_use]
    pub fn columns(&self) -> usize {
        self.columns
    }

    #[must_use]
    pub fn lines(&self) -> usize {
        self.rows.len()
    }

    #[must_use]
    pub fn row(&self, line: usize) -> &Row {
        &self.rows[line]
    }

    pub fn row_mut(&mut self, line: usize) -> &mut Row {
        self.dirty[line] = true;
        self.wrote(line);
        &mut self.rows[line]
    }

    pub(crate) fn wrote(&mut self, line: usize) {
        let line = line as isize;
        self.written = Some(self.written.map_or(line, |written| written.min(line)));
    }

    /// The highest row output wrote to since this was last called, as [`Grid::written`]
    /// keeps it.
    pub fn take_written(&mut self) -> Option<isize> {
        self.written.take()
    }

    #[must_use]
    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    /// Whether `line` changed since the damage was last taken.
    #[must_use]
    pub fn is_dirty(&self, line: usize) -> bool {
        self.dirty[line]
    }

    /// Forgets which rows changed, once they have been looked at.
    pub fn clean(&mut self) {
        self.dirty.fill(false);
    }

    pub fn touch_all(&mut self) {
        self.dirty.fill(true);
        self.wrote(0);
    }

    /// Moves the rows from `top` to `bottom` up by `count`, blanking those it opens at the
    /// bottom with `style`. `gone` sees each row moved off the top, top first, before its
    /// cells are blanked for reuse.
    pub fn scroll_up(
        &mut self,
        top: usize,
        bottom: usize,
        count: usize,
        style: Style,
        mut gone: impl FnMut(&Row),
    ) {
        let count = count.min(bottom + 1 - top);
        for row in &mut self.rows[top..top + count] {
            gone(row);
            row.reset(style);
        }
        self.rows[top..=bottom].rotate_left(count);
        self.dirty[top..=bottom].fill(true);
        // Rows moved off the top keep their text, in the history or nowhere; rows moved
        // within the screen lose what they were.
        if top == 0 {
            self.written = self.written.map(|written| written - count as isize);
        } else {
            self.wrote(top);
        }
    }

    /// Moves the rows from `top` to `bottom` down by `count`, blanking those it opens at the
    /// top and dropping those pushed past the bottom.
    pub fn scroll_down(&mut self, top: usize, bottom: usize, count: usize, style: Style) {
        let count = count.min(bottom + 1 - top);
        self.rows[top..=bottom].rotate_right(count);
        for row in &mut self.rows[top..top + count] {
            *row = Row::new(self.columns, style);
        }
        self.dirty[top..=bottom].fill(true);
        self.wrote(top);
    }

    /// Takes every row out, leaving none, for a resize to make them again.
    pub fn take_rows(&mut self) -> Vec<Row> {
        self.dirty.clear();
        std::mem::take(&mut self.rows)
    }

    /// Puts `rows` in, each `columns` wide.
    pub fn set_rows(&mut self, rows: Vec<Row>, columns: usize) {
        self.columns = columns;
        self.dirty = vec![true; rows.len()];
        self.rows = rows;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Row {
        let mut row = Row::new(text.chars().count(), Style::default());
        for (column, c) in text.chars().enumerate() {
            row.set(
                column,
                Cell {
                    c,
                    style: Style::default(),
                },
            );
        }
        row
    }

    fn text(row: &Row) -> String {
        row.cells.iter().map(|cell| cell.c).collect()
    }

    #[test]
    fn inserting_and_deleting_moves_the_rest() {
        let mut line = row("abcdef");
        line.insert(1, 2, Style::default());
        assert_eq!(text(&line), "a  bcd");
        line.delete(0, 3, Style::default());
        assert_eq!(text(&line), "bcd   ");
    }

    #[test]
    fn clearing_half_a_wide_character_clears_it_whole() {
        let mut line = row("a  b");
        let wide = Style {
            flags: Flags::WIDE,
            ..Style::default()
        };
        let spacer = Style {
            flags: Flags::SPACER,
            ..Style::default()
        };
        line.set(
            1,
            Cell {
                c: '日',
                style: wide,
            },
        );
        line.set(
            2,
            Cell {
                c: ' ',
                style: spacer,
            },
        );
        line.clear(2, 3, Style::default());
        assert_eq!(text(&line), "a  b");
        assert!(line.cells.iter().all(|cell| cell.style == Style::default()));
    }

    #[test]
    fn marks_join_their_cell_and_move_with_it() {
        let mut line = row("ae  ");
        line.add_mark(1, '\u{301}');
        assert_eq!(line.text(1), Text::Cluster("e\u{301}"));
        line.insert(0, 1, Style::default());
        assert_eq!(line.text(2), Text::Cluster("e\u{301}"));
        assert_eq!(line.text(1), Text::Char('a'));
        line.delete(0, 3, Style::default());
        assert_eq!(line.text(0), Text::Char(' '));
        assert!(line.clusters.is_empty());
    }

    #[test]
    fn scrolling_up_hands_back_the_rows_it_moves_off() {
        let mut grid = Grid::new(3, 3);
        for (line, c) in ['a', 'b', 'c'].into_iter().enumerate() {
            grid.row_mut(line).set(
                0,
                Cell {
                    c,
                    style: Style::default(),
                },
            );
        }
        let mut gone = Vec::new();
        grid.scroll_up(0, 2, 1, Style::default(), |row| gone.push(row.cells[0].c));
        assert_eq!(gone, ['a']);
        assert_eq!(grid.row(0).cells[0].c, 'b');
        assert_eq!(grid.row(2).cells[0].c, ' ');
    }
}
