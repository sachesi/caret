//! The terminal's screen as the GPU draws it: the rows the server sent, turned into
//! cells of one size that index the glyph atlas, and which rows each copy on the GPU
//! still lacks.

use caret_protocol::{Cell, Cursor, Frame, Glyph, Mode, Rgb, Selection};

/// One cell as the shaders read it.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct GpuCell {
    /// The atlas entry plus one; zero for none.
    pub glyph: u32,
    /// Colours as 0xRRGGBB.
    pub fg: u32,
    pub bg: u32,
    pub underline: u32,
    /// [`CellFlags`] in the low half, then [`HAS_BACKGROUND`] and [`SELECTED`].
    pub flags: u32,
}

/// The cell has a background of its own, not the terminal's.
pub const HAS_BACKGROUND: u32 = 1 << 16;
pub const SELECTED: u32 = 1 << 17;

pub fn packed(color: Rgb) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

pub struct Grid {
    pub columns: usize,
    pub lines: usize,
    pub cells: Vec<GpuCell>,
    pub cursor: Option<Cursor>,
    pub background: Rgb,
    pub cursor_color: Rgb,
    pub mode: Mode,
    selection: Option<Selection>,
    /// Each line's cells as the server sent them, to resolve again when the atlas is.
    rows: Vec<Vec<Cell>>,
    /// For each copy of the cells on the GPU, the rows it lacks.
    stale: Vec<Vec<bool>>,
}

impl Grid {
    /// An empty grid, kept on the GPU in `copies` copies.
    pub fn new(copies: usize) -> Self {
        Self {
            columns: 0,
            lines: 0,
            cells: Vec::new(),
            cursor: None,
            background: Rgb::default(),
            cursor_color: Rgb::default(),
            mode: Mode::empty(),
            selection: None,
            rows: Vec::new(),
            stale: vec![Vec::new(); copies],
        }
    }

    /// Takes in what changed, as the server sent it, with `glyph` giving each glyph's
    /// atlas entry.
    pub fn apply(&mut self, frame: Frame, mut glyph: impl FnMut(&Glyph) -> u32) {
        let (columns, lines) = (usize::from(frame.columns), usize::from(frame.lines));
        if (columns, lines) != (self.columns, self.lines) {
            self.columns = columns;
            self.lines = lines;
            self.cells = vec![GpuCell::default(); columns * lines];
            self.rows = vec![Vec::new(); lines];
            for stale in &mut self.stale {
                *stale = vec![true; lines];
            }
        }
        let old_selection = std::mem::replace(&mut self.selection, frame.selection);
        // Which cells have a background of their own depends on the terminal's.
        let recolored = frame.background != self.background;
        self.background = frame.background;
        for row in frame.rows {
            let line = usize::from(row.line);
            if line >= lines {
                continue;
            }
            self.rows[line] = row.cells;
            if !recolored {
                self.fill(line, &mut glyph);
            }
        }
        if recolored {
            self.refill(&mut glyph);
        }
        if self.selection != old_selection {
            for line in lines_of(old_selection).chain(lines_of(self.selection)) {
                if line < lines {
                    self.select_line(line);
                    self.mark(line);
                }
            }
        }
        self.cursor = frame.cursor;
        self.cursor_color = frame.cursor_color;
        self.mode = frame.mode;
    }

    /// Resolves every cell again, with `glyph` giving glyphs' entries in a new atlas.
    pub fn refill(&mut self, mut glyph: impl FnMut(&Glyph) -> u32) {
        for line in 0..self.lines {
            self.fill(line, &mut glyph);
        }
    }

    /// Makes `line`'s cells from the ones the server sent for it.
    fn fill(&mut self, line: usize, glyph: &mut impl FnMut(&Glyph) -> u32) {
        let columns = self.columns;
        let start = line * columns;
        self.cells[start..start + columns].fill(GpuCell::default());
        for cell in &self.rows[line] {
            let column = usize::from(cell.column);
            if column >= columns {
                continue;
            }
            let has_background = if cell.bg == self.background {
                0
            } else {
                HAS_BACKGROUND
            };
            self.cells[start + column] = GpuCell {
                glyph: cell.glyph.as_ref().map_or(0, &mut *glyph),
                fg: packed(cell.fg),
                bg: packed(cell.bg),
                underline: packed(cell.underline),
                flags: u32::from(cell.flags.0) | has_background,
            };
        }
        self.select_line(line);
        self.mark(line);
    }

    /// The rows copy `copy` lacks, now counted as copied.
    pub fn take_stale(&mut self, copy: usize) -> Vec<usize> {
        let stale = &mut self.stale[copy];
        let rows = (0..stale.len()).filter(|&line| stale[line]).collect();
        stale.fill(false);
        rows
    }

    fn mark(&mut self, line: usize) {
        for stale in &mut self.stale {
            if let Some(row) = stale.get_mut(line) {
                *row = true;
            }
        }
    }

    /// Sets the selected flag on the cells of `line` that the selection covers.
    fn select_line(&mut self, line: usize) {
        let columns = self.columns;
        let row = &mut self.cells[line * columns..(line + 1) * columns];
        for (column, cell) in row.iter_mut().enumerate() {
            let selected = self
                .selection
                .is_some_and(|selection| covers(&selection, line, column));
            if selected {
                cell.flags |= SELECTED;
            } else {
                cell.flags &= !SELECTED;
            }
        }
    }
}

/// The lines `selection` reaches.
fn lines_of(selection: Option<Selection>) -> impl Iterator<Item = usize> {
    selection
        .into_iter()
        .flat_map(|selection| usize::from(selection.start.line)..=usize::from(selection.end.line))
}

fn covers(selection: &Selection, line: usize, column: usize) -> bool {
    let (start, end) = (selection.start, selection.end);
    let (first, last) = (usize::from(start.line), usize::from(end.line));
    if line < first || line > last {
        return false;
    }
    let (from, to) = (usize::from(start.column), usize::from(end.column));
    if selection.block {
        return (from.min(to)..=from.max(to)).contains(&column);
    }
    (line > first || column >= from) && (line < last || column <= to)
}

#[cfg(test)]
mod tests {
    use caret_protocol::{Cell, CellFlags, GlyphText, Point, Row};

    use super::*;

    fn frame(rows: Vec<Row>, selection: Option<Selection>) -> Frame {
        Frame {
            columns: 4,
            lines: 3,
            rows,
            selection,
            background: Rgb::from_u32(0x101010),
            ..Frame::default()
        }
    }

    fn row(line: u16, text: &str) -> Row {
        Row {
            line,
            cells: text
                .chars()
                .enumerate()
                .map(|(column, c)| Cell {
                    column: column as u16,
                    glyph: Some(Glyph {
                        text: GlyphText::Char(c),
                        style: 0,
                        wide: false,
                    }),
                    fg: Rgb::from_u32(0xffffff),
                    bg: Rgb::from_u32(0x101010),
                    underline: Rgb::from_u32(0xffffff),
                    flags: CellFlags::empty(),
                })
                .collect(),
        }
    }

    #[test]
    fn rows_sent_replace_theirs_and_are_stale_for_every_copy() {
        let mut grid = Grid::new(2);
        grid.apply(frame(vec![row(0, "ab"), row(1, "c")], None), |_| 7);
        assert_eq!(grid.take_stale(0), [0, 1, 2]);
        assert_eq!(grid.take_stale(0), Vec::<usize>::new());
        grid.apply(frame(vec![row(1, "")], None), |_| 7);
        assert_eq!(grid.cells[4], GpuCell::default());
        assert_eq!(grid.cells[0].glyph, 7);
        assert_eq!(grid.cells[0].flags & HAS_BACKGROUND, 0);
        assert_eq!(grid.take_stale(0), [1]);
        assert_eq!(grid.take_stale(1), [0, 1, 2]);
    }

    #[test]
    fn a_selection_marks_its_cells_and_leaves_them_when_it_goes() {
        let mut grid = Grid::new(1);
        let selection = Selection {
            start: Point { line: 0, column: 2 },
            end: Point { line: 1, column: 0 },
            block: false,
        };
        grid.apply(
            frame(vec![row(0, "abcd"), row(1, "efgh")], Some(selection)),
            |_| 1,
        );
        let selected: Vec<bool> = grid
            .cells
            .iter()
            .map(|cell| cell.flags & SELECTED != 0)
            .collect();
        assert_eq!(
            selected,
            [
                false, false, true, true, true, false, false, false, false, false, false, false
            ]
        );
        grid.take_stale(0);
        grid.apply(frame(Vec::new(), None), |_| 1);
        assert!(grid.cells.iter().all(|cell| cell.flags & SELECTED == 0));
        assert_eq!(grid.take_stale(0), [0, 1]);
    }
}
