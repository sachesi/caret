//! The visible screen: rows of cells, each with a stamp that tells when it changed.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::style::{Cell, Flags, Style};

/// Bytes of text one cell keeps. Marks past it are dropped: a program could otherwise pile
/// them on a cell without end, each copying all those before, and clients would have to
/// shape the result. The longest emoji sequences take 35.
pub const MAX_CLUSTER_BYTES: usize = 64;

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
    /// Changes with what the row holds, and is never the same on two rows of any screen,
    /// so that someone who saw this stamp on a line knows the line still shows that. Zero
    /// for a row made for showing the history, which no one can tell that of.
    stamp: u64,
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

    /// See [`Row::stamp`](field@Row::stamp); zero for a row of the history.
    #[must_use]
    pub fn stamp(&self) -> u64 {
        self.stamp
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
                .filter(|(_, text)| text.len() + mark.len_utf8() <= MAX_CLUSTER_BYTES)
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

    /// Puts one cell of `style` for each byte of ASCII in `text` from `column` on.
    pub fn set_ascii(&mut self, column: usize, text: &[u8], style: Style) {
        let end = column + text.len();
        if !self.clusters.is_empty() {
            for at in column..end {
                self.forget_cluster(at);
            }
        }
        for (cell, &byte) in self.cells[column..end].iter_mut().zip(text) {
            *cell = Cell {
                c: char::from(byte),
                style,
            };
        }
        self.touch(end);
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
        debug_assert!(
            self.cells[self.used..]
                .iter()
                .all(|cell| *cell == Cell::blank(Style::default())),
            "cells past those used are blank"
        );
        self.clusters.clear();
        self.wrapped = false;
        self.prompt = false;
        // A row is mostly shorter than the screen is wide, and this is done for each that
        // scrolls: those past `used` are blanks of this style already.
        if style == Style::default() {
            self.cells[..self.used].fill(Cell::blank(style));
            self.used = 0;
        } else {
            self.cells.fill(Cell::blank(style));
            self.used = self.cells.len();
        }
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
        // The blank left at the end of a row for a wide character that did not fit can
        // have been moved to the first column.
        if self.cells[column].style.flags.contains(Flags::SPACER) {
            self.clear(column.saturating_sub(1), column + 1, style);
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
                self.forget_cluster(columns - 1);
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

    /// Whether what the row keeps about its cells is true: each cell marked as having marks
    /// has its text kept once and no other has any, and the cells past those it counts as
    /// used are blank. For tests, whatever a program did to the row.
    #[must_use]
    pub fn is_consistent(&self) -> bool {
        let marked = self
            .cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| cell.style.flags.contains(Flags::CLUSTER))
            .map(|(column, _)| column as u16);
        let mut kept: Vec<u16> = self.clusters.iter().map(|(at, _)| *at).collect();
        kept.sort_unstable();
        kept.into_iter().eq(marked)
            && self.cells[self.used.min(self.cells.len())..]
                .iter()
                .all(|cell| *cell == Cell::blank(Style::default()))
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

/// Which grid the next one is: each counts its stamps from a number of its own, so that
/// rows of the two screens, or of one before and after a reset, never share one.
static GRIDS: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Clone)]
pub struct Grid {
    /// A queue, as scrolling the whole screen is done for every line of output and then
    /// moves no row but the one going round.
    rows: VecDeque<Row>,
    columns: usize,
    last_stamp: u64,
    /// The highest row output wrote to since it was last taken, where that row is now:
    /// above the screen for one that scrolled off it since.
    written: Option<isize>,
}

impl Grid {
    #[must_use]
    pub fn new(columns: usize, lines: usize) -> Self {
        let mut grid = Self {
            rows: VecDeque::new(),
            columns,
            last_stamp: GRIDS.fetch_add(1, Ordering::Relaxed) << 40,
            written: Some(0),
        };
        grid.set_rows(vec![Row::new(columns, Style::default()); lines], columns);
        grid
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
        self.wrote(line);
        self.last_stamp += 1;
        let row = &mut self.rows[line];
        row.stamp = self.last_stamp;
        row
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
    pub fn rows(&self) -> &VecDeque<Row> {
        &self.rows
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
        for row in self.rows.range_mut(top..top + count) {
            gone(row);
            row.reset(style);
            self.last_stamp += 1;
            row.stamp = self.last_stamp;
        }
        if top == 0 && bottom + 1 == self.rows.len() {
            self.rows.rotate_left(count);
        } else {
            self.rows.make_contiguous()[top..=bottom].rotate_left(count);
        }
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
        if top == 0 && bottom + 1 == self.rows.len() {
            self.rows.rotate_right(count);
        } else {
            self.rows.make_contiguous()[top..=bottom].rotate_right(count);
        }
        for row in self.rows.range_mut(top..top + count) {
            row.reset(style);
            self.last_stamp += 1;
            row.stamp = self.last_stamp;
        }
        self.wrote(top);
    }

    /// Takes every row out, leaving none, for a resize to make them again.
    pub fn take_rows(&mut self) -> Vec<Row> {
        std::mem::take(&mut self.rows).into()
    }

    /// Puts `rows` in, each `columns` wide.
    pub fn set_rows(&mut self, mut rows: Vec<Row>, columns: usize) {
        self.columns = columns;
        for row in &mut rows {
            self.last_stamp += 1;
            row.stamp = self.last_stamp;
        }
        self.rows = rows.into();
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
    fn a_row_keeps_its_stamp_until_it_changes() {
        let mut grid = Grid::new(3, 3);
        let stamps = |grid: &Grid| -> Vec<u64> { grid.rows().iter().map(Row::stamp).collect() };
        let before = stamps(&grid);
        assert!(before.iter().all(|&stamp| stamp != 0));
        let _ = grid.row(1);
        assert_eq!(stamps(&grid), before);
        grid.row_mut(1);
        let after = stamps(&grid);
        assert_eq!((after[0], after[2]), (before[0], before[2]));
        assert_ne!(after[1], before[1]);

        grid.scroll_up(0, 2, 1, Style::default(), |_| {});
        let scrolled = stamps(&grid);
        assert_eq!(scrolled[..2], [after[1], after[2]]);
        assert!(!after.contains(&scrolled[2]));
        grid.scroll_down(0, 2, 1, Style::default());
        let back = stamps(&grid);
        assert_eq!(back[1..], scrolled[..2]);
        assert!(!scrolled.contains(&back[0]));
    }

    #[test]
    fn no_two_grids_share_a_stamp() {
        let (one, other) = (Grid::new(3, 3), Grid::new(3, 3));
        let stamps: std::collections::HashSet<u64> = one
            .rows()
            .iter()
            .chain(other.rows())
            .map(Row::stamp)
            .collect();
        assert_eq!(stamps.len(), 6);
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
        assert_eq!(line.clusters, [] as [(u16, Box<str>); 0]);
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
