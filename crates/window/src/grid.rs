//! The terminal's screen as the GPU draws it: the rows the server sent, turned into
//! cells of one size that index the glyph atlas, and which rows each copy on the GPU
//! still lacks.

use std::ops::Range;
use std::rc::Rc;

use caret_protocol::{
    Cell, Cursor, CursorShape, Frame, Glyph, GlyphText, Hovered, Mode, Rgb, Selection,
};

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

/// Where the cells' glyphs are looked up.
pub trait Glyphs {
    /// The atlas entry of `glyph` plus one; zero for none.
    fn index(&mut self, glyph: &Glyph) -> u32;

    /// How a run of ASCII symbols of `style` divides into glyphs, as the lengths of its
    /// parts; none to keep each symbol its own.
    fn ligatures(&mut self, run: &str, style: u8) -> Option<Rc<[u8]>>;
}

impl<G: Glyphs> Glyphs for &mut G {
    fn index(&mut self, glyph: &Glyph) -> u32 {
        (**self).index(glyph)
    }

    fn ligatures(&mut self, run: &str, style: u8) -> Option<Rc<[u8]>> {
        (**self).ligatures(run, style)
    }
}

/// Glyphs left for later, for the cells of a tab out of sight: they are looked up when
/// it is shown.
pub struct Unresolved;

impl Glyphs for Unresolved {
    fn index(&mut self, _glyph: &Glyph) -> u32 {
        0
    }

    fn ligatures(&mut self, _run: &str, _style: u8) -> Option<Rc<[u8]>> {
        None
    }
}

pub fn packed(color: Rgb) -> u32 {
    (u32::from(color.r) << 16) | (u32::from(color.g) << 8) | u32::from(color.b)
}

#[derive(Default)]
pub struct Grid {
    pub columns: usize,
    pub lines: usize,
    pub cells: Vec<GpuCell>,
    pub cursor: Option<Cursor>,
    pub background: Rgb,
    pub cursor_color: Rgb,
    pub mode: Mode,
    /// The link under the pointer.
    pub hovered: Option<Hovered>,
    selection: Option<Selection>,
    /// Each line's cells as the server sent them, to resolve again when the atlas is.
    rows: Vec<Vec<Cell>>,
    /// The rows changed since they were last taken to be drawn.
    stale: Vec<bool>,
}

impl Grid {
    pub fn has_selection(&self) -> bool {
        self.selection.is_some()
    }

    /// The characters of `line` as a screen reader reads them, left to right.
    pub fn characters(&self, line: usize) -> Vec<crate::access::Character> {
        let Some(row) = self.rows.get(line) else {
            return Vec::new();
        };
        row.iter()
            .filter_map(|cell| {
                let glyph = cell.glyph.as_ref()?;
                let text = match &glyph.text {
                    GlyphText::Char(c) => c.to_string(),
                    GlyphText::Cluster(text) => text.to_string(),
                };
                Some(crate::access::Character {
                    text,
                    column: cell.column,
                    cells: if glyph.wide { 2 } else { 1 },
                })
            })
            .collect()
    }

    /// The selection's first and last cell, as line and column.
    pub fn selection(&self) -> Option<((u16, u16), (u16, u16))> {
        self.selection.map(|selection| {
            (
                (selection.start.line, selection.start.column),
                (selection.end.line, selection.end.column),
            )
        })
    }

    /// The cells of `columns` on `lines` seen from afar, as 0xRRGGBBAA: a cell with a
    /// glyph shows half its glyph's colour, half its background.
    pub fn shade(&self, columns: Range<usize>, lines: Range<usize>) -> u32 {
        let mut sum = [0; 3];
        let mut add = |color: Rgb, weight: u32| {
            for (sum, part) in sum.iter_mut().zip([color.r, color.g, color.b]) {
                *sum += u32::from(part) * weight;
            }
        };
        let mut drawn = 0;
        for row in self.rows.get(lines.clone()).unwrap_or_default() {
            for cell in row
                .iter()
                .filter(|cell| columns.contains(&usize::from(cell.column)))
            {
                match &cell.glyph {
                    Some(glyph) if !matches!(glyph.text, GlyphText::Char(' ')) => {
                        add(cell.fg, 1);
                        add(cell.bg, 1);
                    }
                    _ => add(cell.bg, 2),
                }
                drawn += 1;
            }
        }
        let cells = (columns.len() * lines.len()) as u32;
        add(self.background, 2 * cells.saturating_sub(drawn));
        let [r, g, b] = sum.map(|sum| sum / (2 * cells).max(1));
        (r << 24) | (g << 16) | (b << 8) | 0xff
    }

    /// Takes in what changed, as the server sent it, with `glyphs` giving each glyph's
    /// atlas entry.
    pub fn apply(&mut self, frame: Frame, mut glyphs: impl Glyphs) {
        let (columns, lines) = (usize::from(frame.columns), usize::from(frame.lines));
        if (columns, lines) != (self.columns, self.lines) {
            self.columns = columns;
            self.lines = lines;
            self.cells = vec![GpuCell::default(); columns * lines];
            self.rows = vec![Vec::new(); lines];
            self.stale = vec![true; lines];
        }
        let old_selection = std::mem::replace(&mut self.selection, frame.selection);
        let old_cursor = std::mem::replace(&mut self.cursor, frame.cursor);
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
                self.fill(line, &mut glyphs);
            }
        }
        if recolored {
            self.refill(&mut glyphs);
        }
        if self.selection != old_selection {
            for line in lines_of(old_selection).chain(lines_of(self.selection)) {
                if line < lines {
                    self.select_line(line);
                    self.mark(line);
                }
            }
        }
        // Symbols under a block cursor are drawn apart from those they join.
        if self.cursor != old_cursor && !recolored {
            let blocks = [old_cursor, self.cursor]
                .into_iter()
                .flatten()
                .filter(|cursor| cursor.shape == CursorShape::Block);
            for cursor in blocks {
                let line = usize::from(cursor.point.line);
                if line < lines {
                    self.fill(line, &mut glyphs);
                }
            }
        }
        self.cursor_color = frame.cursor_color;
        self.mode = frame.mode;
        self.hovered = frame.hovered;
    }

    /// Resolves every cell again, with `glyphs` giving glyphs' entries in a new atlas.
    pub fn refill(&mut self, mut glyphs: impl Glyphs) {
        for line in 0..self.lines {
            self.fill(line, &mut glyphs);
        }
    }

    /// Makes `line`'s cells from the ones the server sent for it.
    fn fill(&mut self, line: usize, glyphs: &mut impl Glyphs) {
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
                glyph: cell.glyph.as_ref().map_or(0, |glyph| glyphs.index(glyph)),
                fg: packed(cell.fg),
                bg: packed(cell.bg),
                underline: packed(cell.underline),
                flags: u32::from(cell.flags.0) | has_background,
            };
        }
        self.join(line, glyphs);
        self.select_line(line);
        self.mark(line);
    }

    /// Draws symbols side by side on `line` that the font joins as one glyph, in the
    /// first of their cells.
    fn join(&mut self, line: usize, glyphs: &mut impl Glyphs) {
        let columns = self.columns;
        let row = &self.rows[line];
        let apart = self
            .cursor
            .filter(|cursor| cursor.shape == CursorShape::Block)
            .filter(|cursor| usize::from(cursor.point.line) == line)
            .map(|cursor| cursor.point.column);
        let symbol = |cell: &Cell| match &cell.glyph {
            Some(Glyph {
                text: GlyphText::Char(c),
                style,
                wide: false,
            }) if c.is_ascii_punctuation()
                && usize::from(cell.column) < columns
                && Some(cell.column) != apart =>
            {
                Some((*c, *style))
            }
            _ => None,
        };
        let mut run = String::new();
        let mut index = 0;
        while index < row.len() {
            let first = &row[index];
            run.clear();
            let Some((c, style)) = symbol(first) else {
                index += 1;
                continue;
            };
            run.push(c);
            // Symbols side by side in one colour and style, which the font may join.
            while let Some(next) = row.get(index + run.len())
                && usize::from(next.column) == usize::from(first.column) + run.len()
                && next.fg == first.fg
                && let Some((c, next_style)) = symbol(next)
                && next_style == style
            {
                run.push(c);
            }
            index += run.len();
            if run.len() < 2 {
                continue;
            }
            let Some(parts) = glyphs.ligatures(&run, style) else {
                continue;
            };
            let mut at = 0;
            for length in parts.iter().map(|&length| usize::from(length)) {
                if length > 1 && at + length <= run.len() {
                    let joined = Glyph {
                        text: GlyphText::Cluster(run[at..at + length].into()),
                        style,
                        wide: false,
                    };
                    let start = line * columns + usize::from(first.column) + at;
                    self.cells[start].glyph = glyphs.index(&joined);
                    for cell in &mut self.cells[start + 1..start + length] {
                        cell.glyph = 0;
                    }
                }
                at += length;
            }
        }
    }

    /// The rows changed since they were last taken, now counted as drawn.
    pub fn take_stale(&mut self) -> Vec<usize> {
        let rows = (0..self.stale.len())
            .filter(|&line| self.stale[line])
            .collect();
        self.stale.fill(false);
        rows
    }

    fn mark(&mut self, line: usize) {
        if let Some(row) = self.stale.get_mut(line) {
            *row = true;
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

    /// Every glyph at one entry, none joined.
    struct Fixed(u32);

    impl Glyphs for Fixed {
        fn index(&mut self, _glyph: &Glyph) -> u32 {
            self.0
        }

        fn ligatures(&mut self, _run: &str, _style: u8) -> Option<Rc<[u8]>> {
            None
        }
    }

    /// Letters at entry 1 and joined symbols at 9, with a run that starts `->` joining
    /// those two.
    struct Arrows;

    impl Glyphs for Arrows {
        fn index(&mut self, glyph: &Glyph) -> u32 {
            match glyph.text {
                GlyphText::Cluster(_) => 9,
                GlyphText::Char(_) => 1,
            }
        }

        fn ligatures(&mut self, run: &str, _style: u8) -> Option<Rc<[u8]>> {
            let mut parts = vec![1; run.len()];
            if run.starts_with("->") {
                parts.splice(0..2, [2]);
            }
            Some(parts.into())
        }
    }

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
    fn rows_sent_replace_theirs_and_are_stale_until_taken() {
        let mut grid = Grid::default();
        grid.apply(frame(vec![row(0, "ab"), row(1, "c")], None), Fixed(7));
        assert_eq!(grid.take_stale(), [0, 1, 2]);
        assert_eq!(grid.take_stale(), Vec::<usize>::new());
        grid.apply(frame(vec![row(1, "")], None), Fixed(7));
        assert_eq!(grid.cells[4], GpuCell::default());
        assert_eq!(grid.cells[0].glyph, 7);
        assert_eq!(grid.cells[0].flags & HAS_BACKGROUND, 0);
        assert_eq!(grid.take_stale(), [1]);
    }

    #[test]
    fn a_selection_marks_its_cells_and_leaves_them_when_it_goes() {
        let mut grid = Grid::default();
        let selection = Selection {
            start: Point { line: 0, column: 2 },
            end: Point { line: 1, column: 0 },
            block: false,
        };
        grid.apply(
            frame(vec![row(0, "abcd"), row(1, "efgh")], Some(selection)),
            Fixed(1),
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
        grid.take_stale();
        grid.apply(frame(Vec::new(), None), Fixed(1));
        assert!(grid.cells.iter().all(|cell| cell.flags & SELECTED == 0));
        assert_eq!(grid.take_stale(), [0, 1]);
    }

    #[test]
    fn symbols_the_font_joins_are_drawn_from_their_first_cell_but_under_a_block_cursor() {
        let glyphs =
            |grid: &Grid| -> Vec<u32> { grid.cells.iter().map(|cell| cell.glyph).collect() };
        let mut grid = Grid::default();
        grid.apply(frame(vec![row(0, "a->"), row(1, "->!-")], None), Arrows);
        assert_eq!(glyphs(&grid)[..8], [1, 9, 0, 0, 9, 0, 1, 1]);
        let mut on_arrow = frame(Vec::new(), None);
        on_arrow.cursor = Some(Cursor {
            point: Point { line: 0, column: 2 },
            shape: CursorShape::Block,
            wide: false,
        });
        grid.apply(on_arrow, Arrows);
        assert_eq!(glyphs(&grid)[..4], [1, 1, 1, 0]);
        grid.apply(frame(Vec::new(), None), Arrows);
        assert_eq!(glyphs(&grid)[..4], [1, 9, 0, 0]);
    }

    #[test]
    fn from_afar_a_glyph_s_cell_is_half_its_glyph_and_blank_ones_their_background() {
        let mut grid = Grid::default();
        grid.apply(frame(vec![row(0, "a b")], None), Unresolved);
        assert_eq!(grid.shade(0..1, 0..1), 0x8787_87ff);
        assert_eq!(grid.shade(1..2, 0..1), 0x1010_10ff);
        assert_eq!(grid.shade(0..2, 0..1), 0x4b4b_4bff);
        assert_eq!(grid.shade(0..4, 2..3), 0x1010_10ff);
    }
}
