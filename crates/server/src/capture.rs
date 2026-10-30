//! What a client is sent of a terminal: the rows its view shows, in its colours, as the
//! cells the protocol carries.

use caret_protocol::{
    BOLD, Cell, CellFlags, Cursor, CursorShape, Frame, Glyph, GlyphText, Hovered, ITALIC, Mode,
    Point, Row, Selection,
};
use caret_term::Term;
use caret_term::grid::Text;
use caret_term::palette::{CURSOR, Palette};
use caret_term::style::{Color, Flags, Underline};
use caret_term::term::{CursorShape as Shape, MouseEncoding, MouseMode};
use caret_term::view::{Link, Place, View};

use crate::palette;

/// The whole of what a view shows: a row for every line.
pub struct Screen {
    frame: Frame,
}

/// What the program asked of the terminal that changes what keys, the mouse and pastes
/// send it.
pub fn mode(term: &Term) -> Mode {
    let modes = term.modes();
    let mut mode = Mode::empty();
    for (on, flag) in [
        (modes.app_cursor, Mode::APP_CURSOR),
        (modes.app_keypad, Mode::APP_KEYPAD),
        (modes.line_feed_new_line, Mode::LINE_FEED_NEW_LINE),
        (modes.mouse == MouseMode::Click, Mode::MOUSE_REPORT_CLICK),
        (modes.mouse == MouseMode::Drag, Mode::MOUSE_DRAG),
        (modes.mouse == MouseMode::Motion, Mode::MOUSE_MOTION),
        (modes.mouse_encoding == MouseEncoding::Sgr, Mode::SGR_MOUSE),
        (
            modes.mouse_encoding == MouseEncoding::Utf8,
            Mode::UTF8_MOUSE,
        ),
        (modes.bracketed_paste, Mode::BRACKETED_PASTE),
        (modes.focus, Mode::FOCUS_IN_OUT),
        (term.alt_screen(), Mode::ALT_SCREEN),
        (modes.alternate_scroll, Mode::ALTERNATE_SCROLL),
    ] {
        if on {
            mode.insert(flag);
        }
    }
    mode
}

/// The rows `view` shows of `term` in `palette`, with `link` underlined as the one a click
/// would open, and the cursor hollow while the client is not `focused`.
#[expect(clippy::too_many_lines, reason = "one pass over the visible cells")]
pub fn capture(
    term: &Term,
    view: &View,
    palette: &Palette,
    focused: bool,
    link: Option<&Link>,
) -> Screen {
    let (columns, lines) = (term.columns(), term.lines());
    let background = palette::rgb(palette.resolve(Color::Default, true));
    let foreground = palette::rgb(palette.resolve(Color::Default, false));
    let offset = view.offset(term);
    let style = term.cursor_style();
    let mut frame = Frame {
        columns: columns as u16,
        lines: lines as u16,
        cursor_blinks: style.blinking,
        background,
        cursor_color: palette.slot(CURSOR).map_or(foreground, palette::rgb),
        mode: mode(term),
        history: term.history().rows() as u32,
        offset: offset as u32,
        ..Frame::default()
    };
    frame.cursor = term.cursor().and_then(|(line, column)| {
        let line = line + offset;
        let shape = match style.shape {
            _ if !focused => CursorShape::HollowBlock,
            Shape::Block => CursorShape::Block,
            Shape::Underline => CursorShape::Underline,
            Shape::Beam => CursorShape::Beam,
        };
        (line < lines).then(|| Cursor {
            point: Point {
                line: line as u16,
                column: column as u16,
            },
            shape,
            wide: term.screen().row(line - offset).cells[column]
                .style
                .flags
                .contains(Flags::WIDE),
        })
    });
    frame.selection = view.selected_cells(term).map(|(start, end)| {
        let point = |(line, column): (usize, usize)| Point {
            line: line as u16,
            column: column as u16,
        };
        Selection {
            start: point(start),
            end: point(end),
            block: false,
        }
    });

    for (line, shown) in view.shown(term).iter().enumerate() {
        let row = &shown.row;
        let mut cells = Vec::new();
        for (column, cell) in row.cells.iter().enumerate() {
            let style = cell.style;
            let flags = style.flags;
            let text = match row.text(column) {
                _ if flags.intersects(Flags::SPACER | Flags::HIDDEN) => None,
                Text::Char(' ' | '\0') => None,
                Text::Char(c) => Some(GlyphText::Char(c)),
                Text::Cluster(text) => Some(GlyphText::Cluster(text.into())),
            };
            let mut lines_drawn = match flags.underline() {
                Underline::None => CellFlags::empty(),
                Underline::Single => CellFlags::UNDERLINE,
                Underline::Double => CellFlags::DOUBLE_UNDERLINE,
                Underline::Curly => CellFlags::UNDERCURL,
                Underline::Dotted => CellFlags::DOTTED_UNDERLINE,
                Underline::Dashed => CellFlags::DASHED_UNDERLINE,
            };
            let place = Place {
                line: shown.place.line,
                cell: shown.place.cell + column,
            };
            if lines_drawn == CellFlags::empty()
                && link.is_some_and(|link| link.span.covers(place, style.link))
            {
                lines_drawn = CellFlags::UNDERLINE;
            }
            if flags.contains(Flags::STRIKEOUT) {
                lines_drawn.insert(CellFlags::STRIKEOUT);
            }

            let mut fg = palette::rgb(palette.resolve(style.fg, false));
            let mut bg = palette::rgb(palette.resolve(style.bg, true));
            if flags.contains(Flags::DIM) {
                fg = palette::dim(fg, background);
            }
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let underline = match style.underline {
                Color::Default => fg,
                color => palette::rgb(palette.resolve(color, false)),
            };
            if text.is_none() && bg == background && lines_drawn == CellFlags::empty() {
                continue;
            }
            let mut weight = 0;
            if flags.contains(Flags::BOLD) {
                weight |= BOLD;
            }
            if flags.contains(Flags::ITALIC) {
                weight |= ITALIC;
            }
            cells.push(Cell {
                column: column as u16,
                glyph: text.map(|text| Glyph {
                    text,
                    style: weight,
                    wide: flags.contains(Flags::WIDE),
                }),
                fg,
                bg,
                underline,
                flags: lines_drawn,
            });
        }
        frame.rows.push(Row {
            line: line as u16,
            cells,
        });
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
    use caret_term::Options;

    use super::*;

    fn term(text: &str) -> Term {
        let mut term = Term::new(20, 4, Options::default());
        term.advance(text.as_bytes());
        term
    }

    fn palette() -> Palette {
        let mut palette = Palette::default();
        palette.set_base(palette::base(&palette::fallback()));
        palette
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

    fn frame(term: &Term, view: &View, sent: &mut Sent) -> Frame {
        capture(term, view, &palette(), true, None).changes(sent, None)
    }

    #[test]
    fn only_rows_that_changed_are_sent_again() {
        let view = View::default();
        let mut sent = Sent::default();
        let mut term = term("one\r\ntwo");
        let first = frame(&term, &view, &mut sent);
        assert_eq!(first.rows.len(), 4);
        assert_eq!(text(&first.rows[1]), "two");

        term.advance(b"\r\nthree");
        let second = frame(&term, &view, &mut sent);
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
        let view = View::default();
        let mut sent = Sent::default();
        let mut term = term("one\r\ntwo");
        frame(&term, &view, &mut sent);

        term.resize(30, 4);
        let wider = frame(&term, &view, &mut sent);
        assert_eq!(wider.columns, 30);
        assert_eq!(wider.rows.len(), 4);
        assert_eq!(text(&wider.rows[0]), "one");
    }

    #[test]
    fn styles_and_lines_come_with_the_cells() {
        let palette = palette::fallback();
        let term = term("\x1b[1;3;4:3;9mx\x1b[0m\x1b[7my");
        let frame = frame(&term, &View::default(), &mut Sent::default());
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

    #[test]
    fn a_scrolled_view_shows_the_history_and_moves_the_cursor_down() {
        let term = term("1\r\n2\r\n3\r\n4\r\n5\r\n6");
        let mut view = View::default();
        view.scroll(&term, 1);
        let frame = frame(&term, &view, &mut Sent::default());
        assert_eq!(text(&frame.rows[0]), "2");
        assert_eq!((frame.history, frame.offset), (2, 1));
        assert_eq!(frame.cursor.map(|cursor| cursor.point.line), None);
        view.scroll(&term, -1);
        let frame =
            capture(&term, &view, &palette(), false, None).changes(&mut Sent::default(), None);
        assert_eq!(
            frame.cursor.map(|cursor| (cursor.point.line, cursor.shape)),
            Some((3, CursorShape::HollowBlock))
        );
    }
}
