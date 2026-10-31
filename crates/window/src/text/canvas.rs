//! The terminal's screen as cells, so that a frame writes only what changed: a console
//! takes long over a whole screen, and many terminals show a clear before what follows.

use std::fmt::Write as _;

use caret_protocol::{Cell, GlyphText, Rgb};

pub const DIM: u8 = 1 << 6;
pub const REVERSE: u8 = 1 << 7;

/// How a cell is written: its colours, none for the terminal's own, and its attributes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Pen {
    pub fg: Option<Rgb>,
    pub bg: Option<Rgb>,
    /// `caret_protocol::BOLD`, `caret_protocol::ITALIC`, [`DIM`], [`REVERSE`].
    pub style: u8,
}

impl Pen {
    fn write(self, out: &mut String) {
        out.push_str("\x1b[0");
        for (bit, code) in [
            (caret_protocol::BOLD, ";1"),
            (DIM, ";2"),
            (caret_protocol::ITALIC, ";3"),
            (REVERSE, ";7"),
        ] {
            if self.style & bit != 0 {
                out.push_str(code);
            }
        }
        if let Some(Rgb { r, g, b }) = self.fg {
            let _ = write!(out, ";38;2;{r};{g};{b}");
        }
        if let Some(Rgb { r, g, b }) = self.bg {
            let _ = write!(out, ";48;2;{r};{g};{b}");
        }
        out.push('m');
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct Spot {
    /// Nothing is a space.
    text: Option<GlyphText>,
    pen: Pen,
    wide: bool,
    /// The right half of the wide character to its left.
    covered: bool,
}

#[derive(Debug, Default)]
pub struct Canvas {
    columns: usize,
    lines: usize,
    spots: Vec<Spot>,
}

impl Canvas {
    pub fn size(&self) -> (usize, usize) {
        (self.columns, self.lines)
    }

    /// Blank, at this size.
    pub fn reset(&mut self, columns: usize, lines: usize) {
        self.columns = columns;
        self.lines = lines;
        self.spots.clear();
        self.spots.resize(columns * lines, Spot::default());
    }

    fn index(&self, x: i32, y: i32) -> Option<usize> {
        let (x, y) = (usize::try_from(x).ok()?, usize::try_from(y).ok()?);
        (x < self.columns && y < self.lines).then_some(y * self.columns + x)
    }

    /// Puts text at a cell; outside the screen it goes nowhere, and a wide character
    /// without room for its right half is a blank.
    pub fn put(&mut self, x: i32, y: i32, text: Option<GlyphText>, pen: Pen, wide: bool) {
        let Some(index) = self.index(x, y) else {
            return;
        };
        let right = wide.then(|| self.index(x + 1, y)).flatten();
        if wide && right.is_none() {
            self.spots[index] = Spot {
                pen,
                ..Spot::default()
            };
            return;
        }
        self.spots[index] = Spot {
            text,
            pen,
            wide,
            covered: false,
        };
        if let Some(right) = right {
            self.spots[right] = Spot {
                pen,
                covered: true,
                ..Spot::default()
            };
        }
    }

    /// A pane's line of cells, `width` wide from `x`, its background where it has no cell.
    pub fn put_row(&mut self, (x, y): (i32, i32), row: &[Cell], width: i32, background: Rgb) {
        let blank = Pen {
            bg: Some(background),
            ..Pen::default()
        };
        for column in 0..width {
            self.put(x + column, y, None, blank, false);
        }
        for cell in row {
            let column = i32::from(cell.column);
            let wide = cell.glyph.as_ref().is_some_and(|glyph| glyph.wide);
            if column + i32::from(wide) >= width {
                continue;
            }
            let pen = Pen {
                fg: Some(cell.fg),
                bg: Some(cell.bg),
                style: cell.glyph.as_ref().map_or(0, |glyph| glyph.style)
                    & (caret_protocol::BOLD | caret_protocol::ITALIC),
            };
            let text = cell.glyph.as_ref().map(|glyph| glyph.text.clone());
            self.put(x + column, y, text, pen, wide);
        }
    }

    /// Writes what differs from `old`, a canvas of the same size that is on the screen.
    pub fn write_changes(&self, old: &Self, out: &mut String) {
        let mut pen = None;
        // Where the terminal's cursor is known to be. A wide character or a cluster may
        // take another width there than here, and the last column leaves it pending.
        let mut at = None;
        for (index, (spot, before)) in self.spots.iter().zip(&old.spots).enumerate() {
            if spot == before || spot.covered {
                continue;
            }
            let (y, x) = (index / self.columns, index % self.columns);
            if at != Some(index) {
                let _ = write!(out, "\x1b[{};{}H", y + 1, x + 1);
            }
            if pen != Some(spot.pen) {
                spot.pen.write(out);
                pen = Some(spot.pen);
            }
            let simple = match &spot.text {
                Some(GlyphText::Char(c)) => {
                    out.push(*c);
                    true
                }
                Some(GlyphText::Cluster(text)) => {
                    out.push_str(text);
                    false
                }
                None => {
                    out.push(' ');
                    true
                }
            };
            at = (simple && !spot.wide && x + 1 < self.columns).then_some(index + 1);
        }
        if pen.is_some() {
            out.push_str("\x1b[0m");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canvas(columns: usize, lines: usize) -> Canvas {
        let mut canvas = Canvas::default();
        canvas.reset(columns, lines);
        canvas
    }

    fn letter(c: char) -> GlyphText {
        GlyphText::Char(c)
    }

    fn changes(new: &Canvas, old: &Canvas) -> String {
        let mut out = String::new();
        new.write_changes(old, &mut out);
        out
    }

    #[test]
    fn the_same_screen_writes_nothing() {
        let (mut old, mut new) = (canvas(4, 2), canvas(4, 2));
        old.put(1, 1, Some(letter('a')), Pen::default(), false);
        new.put(1, 1, Some(letter('a')), Pen::default(), false);
        assert_eq!(changes(&new, &old), "");
    }

    #[test]
    fn one_changed_cell_writes_only_that_cell() {
        let old = canvas(4, 2);
        let mut new = canvas(4, 2);
        new.put(2, 1, Some(letter('a')), Pen::default(), false);
        assert_eq!(changes(&new, &old), "\x1b[2;3H\x1b[0ma\x1b[0m");
    }

    #[test]
    fn neighbours_of_one_pen_share_a_move_and_a_colour() {
        let old = canvas(4, 1);
        let mut new = canvas(4, 1);
        let pen = Pen {
            fg: Some(Rgb { r: 1, g: 2, b: 3 }),
            bg: None,
            style: DIM,
        };
        new.put(0, 0, Some(letter('a')), pen, false);
        new.put(1, 0, Some(letter('b')), pen, false);
        assert_eq!(
            changes(&new, &old),
            "\x1b[1;1H\x1b[0;2;38;2;1;2;3mab\x1b[0m"
        );
    }

    #[test]
    fn after_a_wide_character_and_at_the_line_end_the_cursor_moves_again() {
        let old = canvas(4, 2);
        let mut new = canvas(4, 2);
        new.put(0, 0, Some(letter('好')), Pen::default(), true);
        new.put(2, 0, Some(letter('a')), Pen::default(), false);
        new.put(3, 0, Some(letter('b')), Pen::default(), false);
        new.put(0, 1, Some(letter('c')), Pen::default(), false);
        assert_eq!(
            changes(&new, &old),
            "\x1b[1;1H\x1b[0m好\x1b[1;3Hab\x1b[2;1Hc\x1b[0m"
        );
    }

    #[test]
    fn a_wide_character_at_the_line_end_is_a_blank() {
        let old = canvas(2, 1);
        let mut new = canvas(2, 1);
        new.put(1, 0, Some(letter('好')), Pen::default(), true);
        assert_eq!(changes(&new, &old), "");
    }

    #[test]
    fn replacing_a_wide_character_writes_both_its_cells() {
        let mut old = canvas(3, 1);
        old.put(0, 0, Some(letter('好')), Pen::default(), true);
        let new = canvas(3, 1);
        assert_eq!(changes(&new, &old), "\x1b[1;1H\x1b[0m  \x1b[0m");
    }

    #[test]
    fn what_falls_outside_goes_nowhere() {
        let old = canvas(2, 1);
        let mut new = canvas(2, 1);
        new.put(5, 0, Some(letter('a')), Pen::default(), false);
        new.put(0, 3, Some(letter('a')), Pen::default(), false);
        new.put(-1, 0, Some(letter('a')), Pen::default(), false);
        assert_eq!(changes(&new, &old), "");
    }
}
