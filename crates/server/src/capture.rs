//! What is on a terminal's screen, copied out while it is locked, as the rows of cells
//! clients are sent.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::Column;
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{self, Color, NamedColor};
use caret_protocol::{
    BOLD, Cell, CellFlags, Cursor, CursorShape, Frame, Glyph, GlyphText, Hovered, ITALIC, Mode,
    Palette, Point, Row, Selection,
};

use crate::links::Span;
use crate::palette;

/// The whole screen: a row for every line.
pub struct Screen {
    frame: Frame,
}

const MODES: [(TermMode, Mode); 12] = [
    (TermMode::APP_CURSOR, Mode::APP_CURSOR),
    (TermMode::APP_KEYPAD, Mode::APP_KEYPAD),
    (TermMode::LINE_FEED_NEW_LINE, Mode::LINE_FEED_NEW_LINE),
    (TermMode::MOUSE_REPORT_CLICK, Mode::MOUSE_REPORT_CLICK),
    (TermMode::MOUSE_DRAG, Mode::MOUSE_DRAG),
    (TermMode::MOUSE_MOTION, Mode::MOUSE_MOTION),
    (TermMode::SGR_MOUSE, Mode::SGR_MOUSE),
    (TermMode::UTF8_MOUSE, Mode::UTF8_MOUSE),
    (TermMode::BRACKETED_PASTE, Mode::BRACKETED_PASTE),
    (TermMode::FOCUS_IN_OUT, Mode::FOCUS_IN_OUT),
    (TermMode::ALT_SCREEN, Mode::ALT_SCREEN),
    (TermMode::ALTERNATE_SCROLL, Mode::ALTERNATE_SCROLL),
];

const LINES: [(Flags, CellFlags); 6] = [
    (Flags::UNDERLINE, CellFlags::UNDERLINE),
    (Flags::DOUBLE_UNDERLINE, CellFlags::DOUBLE_UNDERLINE),
    (Flags::UNDERCURL, CellFlags::UNDERCURL),
    (Flags::DOTTED_UNDERLINE, CellFlags::DOTTED_UNDERLINE),
    (Flags::DASHED_UNDERLINE, CellFlags::DASHED_UNDERLINE),
    (Flags::STRIKEOUT, CellFlags::STRIKEOUT),
];

pub fn mode(mode: TermMode) -> Mode {
    let mut converted = Mode::empty();
    for (term, ours) in MODES {
        if mode.contains(term) {
            converted.insert(ours);
        }
    }
    converted
}

pub fn cursor_shape(shape: CursorShape) -> ansi::CursorShape {
    match shape {
        CursorShape::Block => ansi::CursorShape::Block,
        CursorShape::Underline => ansi::CursorShape::Underline,
        CursorShape::Beam => ansi::CursorShape::Beam,
        CursorShape::HollowBlock => ansi::CursorShape::HollowBlock,
    }
}

/// The screen of `term` in `palette`, with `link` underlined as the one a click would
/// open.
#[expect(clippy::too_many_lines, reason = "one pass over the visible cells")]
pub fn capture<T: EventListener>(term: &Term<T>, palette: &Palette, link: Option<&Span>) -> Screen {
    let content = term.renderable_content();
    let colors = content.colors;
    let offset = content.display_offset as i32;
    let lines = term.screen_lines();
    let columns = term.columns();
    let mut frame = Frame {
        columns: columns as u16,
        lines: lines as u16,
        rows: (0..lines)
            .map(|line| Row {
                line: line as u16,
                cells: Vec::new(),
            })
            .collect(),
        cursor_blinks: term.cursor_style().blinking,
        background: palette::resolve(palette, Color::Named(NamedColor::Background), colors),
        cursor_color: palette::resolve(palette, Color::Named(NamedColor::Cursor), colors),
        mode: mode(content.mode),
        history: term.history_size() as u32,
        offset: content.display_offset as u32,
        ..Frame::default()
    };

    let cursor_line = content.cursor.point.line.0 + offset;
    let shape = match content.cursor.shape {
        ansi::CursorShape::Block => Some(CursorShape::Block),
        ansi::CursorShape::Underline => Some(CursorShape::Underline),
        ansi::CursorShape::Beam => Some(CursorShape::Beam),
        ansi::CursorShape::HollowBlock => Some(CursorShape::HollowBlock),
        ansi::CursorShape::Hidden => None,
    };
    frame.cursor = shape
        .filter(|_| (0..lines as i32).contains(&cursor_line))
        .map(|shape| Cursor {
            point: Point {
                line: cursor_line as u16,
                column: content.cursor.point.column.0 as u16,
            },
            shape,
            wide: false,
        });

    let last_column = columns.saturating_sub(1);
    frame.selection = content.selection.and_then(|range| {
        let start_line = range.start.line.0 + offset;
        let end_line = range.end.line.0 + offset;
        if end_line < 0 || start_line >= lines as i32 {
            return None;
        }
        let point = |line: usize, column: Column| Point {
            line: line as u16,
            column: column.0 as u16,
        };
        let start = if start_line < 0 {
            let column = if range.is_block {
                range.start.column
            } else {
                Column(0)
            };
            point(0, column)
        } else {
            point(start_line as usize, range.start.column)
        };
        let end = if end_line >= lines as i32 {
            let column = if range.is_block {
                range.end.column
            } else {
                Column(last_column)
            };
            point(lines - 1, column)
        } else {
            point(end_line as usize, range.end.column)
        };
        Some(Selection {
            start,
            end,
            block: range.is_block,
        })
    });

    for indexed in content.display_iter {
        let cell = indexed.cell;
        let line = (indexed.point.line.0 + offset) as usize;
        let column = indexed.point.column.0;
        let mut flags = cell.flags;
        if !flags.intersects(Flags::ALL_UNDERLINES)
            && link.is_some_and(|link| link.covers(indexed.point, cell))
        {
            flags |= Flags::UNDERLINE;
        }

        let mut fg = palette::resolve(palette, cell.fg, colors);
        let mut bg = palette::resolve(palette, cell.bg, colors);
        if flags.contains(Flags::DIM) {
            fg = palette::dim(palette, fg);
        }
        if flags.contains(Flags::INVERSE) {
            std::mem::swap(&mut fg, &mut bg);
        }
        let underline = cell
            .underline_color()
            .map_or(fg, |color| palette::resolve(palette, color, colors));

        let spacer = flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
        let text = match cell.zerowidth() {
            _ if spacer || flags.contains(Flags::HIDDEN) => None,
            Some(marks) if !marks.is_empty() => Some(GlyphText::Cluster(
                std::iter::once(cell.c)
                    .chain(marks.iter().copied())
                    .collect(),
            )),
            _ if cell.c == ' ' || cell.c == '\t' || cell.c == '\0' => None,
            _ => Some(GlyphText::Char(cell.c)),
        };
        let mut lines_drawn = CellFlags::empty();
        for (term, ours) in LINES {
            if flags.contains(term) {
                lines_drawn.insert(ours);
            }
        }
        if let Some(cursor) = &mut frame.cursor
            && usize::from(cursor.point.line) == line
            && usize::from(cursor.point.column) == column
        {
            cursor.wide = flags.contains(Flags::WIDE_CHAR);
        }
        if text.is_none() && bg == frame.background && lines_drawn == CellFlags::empty() {
            continue;
        }
        let mut style = 0;
        if flags.contains(Flags::BOLD) {
            style |= BOLD;
        }
        if flags.contains(Flags::ITALIC) {
            style |= ITALIC;
        }
        if let Some(row) = frame.rows.get_mut(line) {
            row.cells.push(Cell {
                column: column as u16,
                glyph: text.map(|text| Glyph {
                    text,
                    style,
                    wide: flags.contains(Flags::WIDE_CHAR),
                }),
                fg,
                bg,
                underline,
                flags: lines_drawn,
            });
        }
    }
    Screen { frame }
}

/// What a client was last sent of a screen.
#[derive(Debug, Default)]
pub struct Sent {
    columns: u16,
    rows: Vec<Row>,
}

impl Screen {
    /// The frame for a client that was last sent `sent`, which becomes this screen's rows:
    /// only the rows that differ, or all of them after the size changed.
    pub fn changes(self, sent: &mut Sent, hovered: Option<Hovered>) -> Frame {
        let Self { mut frame } = self;
        let rows = std::mem::take(&mut frame.rows);
        let all = sent.columns != frame.columns || sent.rows.len() != rows.len();
        frame.rows = rows
            .iter()
            .enumerate()
            .filter(|&(line, row)| all || sent.rows[line] != *row)
            .map(|(_, row)| row.clone())
            .collect();
        *sent = Sent {
            columns: frame.columns,
            rows,
        };
        frame.hovered = hovered;
        frame
    }
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term;
    use alacritty_terminal::vte::ansi::Processor;

    use super::*;
    use crate::terminal::GridSize;

    fn term(text: &str) -> Term<VoidListener> {
        let size = GridSize {
            columns: 20,
            lines: 4,
        };
        let mut term = Term::new(term::Config::default(), &size, VoidListener);
        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, text.as_bytes());
        term
    }

    fn text(row: &Row) -> String {
        row.cells
            .iter()
            .filter_map(|cell| match &cell.glyph.as_ref()?.text {
                GlyphText::Char(c) => Some(*c),
                GlyphText::Cluster(_) => None,
            })
            .collect()
    }

    #[test]
    fn only_rows_that_changed_are_sent_again() {
        let palette = palette::fallback();
        let mut sent = Sent::default();
        let mut term = term("one\r\ntwo");
        let first = capture(&term, &palette, None).changes(&mut sent, None);
        assert_eq!(first.rows.len(), 4);
        assert_eq!(text(&first.rows[1]), "two");

        let mut parser: Processor = Processor::new();
        parser.advance(&mut term, b"\r\nthree");
        let second = capture(&term, &palette, None).changes(&mut sent, None);
        assert_eq!(second.rows.len(), 1);
        assert_eq!(second.rows[0].line, 2);
        assert_eq!(text(&second.rows[0]), "three");
        assert_eq!(
            second.cursor.map(|cursor| cursor.point),
            Some(Point { line: 2, column: 5 })
        );
    }

    #[test]
    fn every_row_is_sent_again_after_the_width_changes() {
        let palette = palette::fallback();
        let mut sent = Sent::default();
        let mut term = term("one\r\ntwo");
        capture(&term, &palette, None).changes(&mut sent, None);

        term.resize(GridSize {
            columns: 30,
            lines: 4,
        });
        let wider = capture(&term, &palette, None).changes(&mut sent, None);
        assert_eq!(wider.columns, 30);
        assert_eq!(wider.rows.len(), 4);
        assert_eq!(text(&wider.rows[0]), "one");
    }

    #[test]
    fn styles_and_lines_come_with_the_cells() {
        let palette = palette::fallback();
        let screen = capture(&term("\x1b[1;3;4:3;9mx\x1b[0m\x1b[7my"), &palette, None);
        let frame = screen.changes(&mut Sent::default(), None);
        let cells = &frame.rows[0].cells;
        let x = &cells[0];
        assert_eq!(
            x.glyph.as_ref().map(|glyph| glyph.style),
            Some(BOLD | ITALIC)
        );
        assert!(
            x.flags
                .contains(CellFlags::UNDERCURL | CellFlags::STRIKEOUT)
        );
        let y = &cells[1];
        assert_eq!((y.fg, y.bg), (palette.background, palette.foreground));
    }
}
