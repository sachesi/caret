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

/// What a view shows, for a client that was last sent [`Sent`]: for every line its row, or
/// none where the client has that already.
pub struct Screen {
    frame: Frame,
    rows: Vec<Option<Row>>,
    /// Each line's [`stamp`](caret_term::grid::Row::stamp).
    stamps: Vec<u64>,
    palette: Palette,
    linked: bool,
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
    mode.with_keyboard(term.keyboard_flags())
}

/// The rows `view` shows of `term` in `palette`, with `link` underlined as the one a click
/// would open, and the cursor hollow while the client is not `focused`. Rows are made only
/// for the lines that may differ from what the client was `sent`: this runs for each frame,
/// and most frames change a row or two of a screen of thousands of cells.
#[expect(clippy::too_many_lines, reason = "one pass over the visible cells")]
pub fn capture(
    term: &Term,
    view: &View,
    palette: &Palette,
    focused: bool,
    link: Option<&Link>,
    sent: &Sent,
) -> Screen {
    let (columns, lines) = (term.columns(), term.lines());
    let background = palette::rgb(palette.resolve(Color::DEFAULT, true));
    let foreground = palette::rgb(palette.resolve(Color::DEFAULT, false));
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

    // What a row's cells are made from beside the row itself.
    let same_look = sent.columns == columns as u16
        && sent.rows.len() == lines
        && link.is_none()
        && !sent.linked
        && sent.palette.as_ref() == Some(palette);
    let shown_rows = view.shown(term);
    let mut rows = Vec::with_capacity(shown_rows.len());
    let mut stamps = Vec::with_capacity(shown_rows.len());
    for (line, shown) in shown_rows.iter().enumerate() {
        let row = &shown.row;
        let stamp = row.stamp();
        stamps.push(stamp);
        if same_look && stamp != 0 && sent.stamps.get(line) == Some(&stamp) {
            rows.push(None);
            continue;
        }
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
            let underline = if style.underline == Color::DEFAULT {
                fg
            } else {
                palette::rgb(palette.resolve(style.underline, false))
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
        rows.push(Some(Row {
            line: line as u16,
            cells,
        }));
    }
    Screen {
        frame,
        rows,
        stamps,
        palette: palette.clone(),
        linked: link.is_some(),
    }
}

/// Bytes of a frame's rows, about, past which the rest wait for the next frame. A client
/// is hung up on for a message past [`caret_protocol::LONGEST`], which a large screen of
/// long clusters would be, and the rows of a smaller message arrive just as well.
const FRAME_BYTES: usize = 16 << 20;

/// About what `row` takes in a message.
fn row_bytes(row: &Row) -> usize {
    let glyphs: usize = row
        .cells
        .iter()
        .map(|cell| match &cell.glyph {
            Some(Glyph {
                text: GlyphText::Cluster(text),
                ..
            }) => text.len(),
            _ => 0,
        })
        .sum();
    row.cells.len() * 24 + glyphs
}

/// What a client was last sent of a screen.
#[derive(Debug, Default)]
pub struct Sent {
    columns: u16,
    rows: Vec<Row>,
    /// The stamp of the row each line showed; zero for one that is still to be sent.
    stamps: Vec<u64>,
    palette: Option<Palette>,
    /// A link was underlined.
    linked: bool,
    /// All but the rows of its last frame.
    last: Frame,
}

impl Screen {
    /// The frame for a client that was last sent `sent`, which becomes this screen's rows:
    /// only the rows that differ, or all of them after the size changed. And whether that
    /// is all of them: past [`FRAME_BYTES`], the rest go in the next frame. None when the
    /// client has all of it already, so that what it draws stays as it is.
    pub fn changes(self, sent: &mut Sent, hovered: Option<Hovered>) -> Option<(Frame, bool)> {
        self.changes_within(sent, hovered, FRAME_BYTES)
    }

    fn changes_within(
        self,
        sent: &mut Sent,
        hovered: Option<Hovered>,
        limit: usize,
    ) -> Option<(Frame, bool)> {
        let Self {
            mut frame,
            rows,
            stamps,
            palette,
            linked,
        } = self;
        let all = sent.columns != frame.columns || sent.rows.len() != rows.len();
        if all {
            sent.columns = frame.columns;
            sent.rows = vec![Row::default(); rows.len()];
            sent.stamps = vec![0; rows.len()];
        }
        let mut bytes = 0;
        let mut whole = true;
        let mut changed = Vec::new();
        for (line, (row, stamp)) in rows.into_iter().zip(stamps).enumerate() {
            let Some(row) = row else {
                continue;
            };
            if !all && sent.rows[line] == row {
                sent.stamps[line] = stamp;
                continue;
            }
            bytes += row_bytes(&row);
            if bytes > limit && !changed.is_empty() {
                whole = false;
                // Differs from any row there is, so it is sent next time.
                sent.rows[line] = Row {
                    line: u16::MAX,
                    cells: Vec::new(),
                };
                sent.stamps[line] = 0;
            } else {
                changed.push(row.clone());
                sent.rows[line] = row;
                sent.stamps[line] = stamp;
            }
        }
        if sent.palette.as_ref() != Some(&palette) {
            sent.palette = Some(palette);
        }
        sent.linked = linked;
        frame.hovered = hovered;
        if changed.is_empty() && frame == sent.last {
            return None;
        }
        sent.last.clone_from(&frame);
        frame.rows = changed;
        Some((frame, whole))
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

    /// What a client holds of a screen after the frames it was sent.
    #[derive(Default)]
    struct Client {
        sent: Sent,
        rows: Vec<Vec<Cell>>,
        frames: usize,
    }

    impl Client {
        fn take_frame(&mut self, term: &Term, view: &View, palette: &Palette, link: Option<&Link>) {
            let screen = capture(term, view, palette, true, link, &self.sent);
            let Some((frame, whole)) = screen.changes(&mut self.sent, None) else {
                return;
            };
            assert!(whole);
            self.frames += 1;
            self.rows.resize(usize::from(frame.lines), Vec::new());
            for row in frame.rows {
                self.rows[usize::from(row.line)] = row.cells;
            }
        }
    }

    /// Whatever happened in between, the rows a client put together from its frames are
    /// the ones a client new to the terminal would be sent.
    #[test]
    fn frames_add_up_to_the_whole_screen() {
        const OUTPUT: [&str; 18] = [
            "hello ",
            "\r\n",
            "\x1b[31mred\x1b[0m",
            "\x1b[2;5H",
            "\x1b[2J",
            "\x1b[K",
            "\x1b[1L",
            "\x1b[1M",
            "\x1b[2;4r",
            "\x1b[r",
            "\x1b[?1049h",
            "\x1b[?1049l",
            "\x1bc",
            "\x1b]4;1;#102030\x07",
            "\x1b]104\x07",
            "日本語",
            "e\u{301}",
            "https://example.org/some/path ",
        ];
        for seed in 1..=64u64 {
            let mut random = seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1;
            let mut below = |limit: usize| {
                random ^= random >> 12;
                random ^= random << 25;
                random ^= random >> 27;
                (random.wrapping_mul(0x2545_f491_4f6c_dd1d) >> 33) as usize % limit
            };
            let mut term = Term::new(20, 6, Options::default());
            let mut view = View::default();
            let mut base = palette::base(&palette::fallback());
            // One takes every frame, and one takes a frame now and then.
            let (mut often, mut seldom) = (Client::default(), Client::default());
            for _ in 0..300 {
                match below(24) {
                    0 => term.resize(10 + below(20), 3 + below(6)),
                    1 => view.scroll(&term, 2),
                    2 => view.scroll_to_bottom(),
                    3 => {
                        base.background = caret_term::palette::Rgb::from_u32(below(0xff) as u32);
                        term.set_palette_base(base.clone());
                    }
                    _ => term.advance(OUTPUT[below(OUTPUT.len())].as_bytes()),
                }
                let hovered = below(3) == 0;
                let link = hovered
                    .then(|| view.link_at(&term, below(term.lines()), below(term.columns())))
                    .flatten();
                let palette = term.palette().clone();
                often.take_frame(&term, &view, &palette, link.as_ref());
                if below(4) == 0 {
                    seldom.take_frame(&term, &view, &palette, link.as_ref());
                }
                let new = capture(
                    &term,
                    &view,
                    &palette,
                    true,
                    link.as_ref(),
                    &Sent::default(),
                );
                let (whole, _) = new.changes(&mut Sent::default(), None).expect("all of it");
                assert_eq!(whole.rows.len(), term.lines(), "seed {seed}");
                for row in whole.rows {
                    assert_eq!(
                        often.rows[usize::from(row.line)],
                        row.cells,
                        "seed {seed}, line {}",
                        row.line
                    );
                }
            }
            seldom.take_frame(&term, &view, &term.palette().clone(), None);
            let new = capture(&term, &view, term.palette(), true, None, &Sent::default());
            let (whole, _) = new.changes(&mut Sent::default(), None).expect("all of it");
            for row in whole.rows {
                assert_eq!(seldom.rows[usize::from(row.line)], row.cells, "seed {seed}");
            }
            assert!(often.frames >= seldom.frames);
        }
    }

    #[test]
    fn a_frame_with_nothing_new_is_not_sent() {
        let view = View::default();
        let mut term = term("one");
        let mut sent = Sent::default();
        let palette = palette();
        let frame = |term: &Term, sent: &mut Sent| {
            capture(term, &view, &palette, true, None, sent).changes(sent, None)
        };
        assert!(frame(&term, &mut sent).is_some());
        assert!(frame(&term, &mut sent).is_none());
        // Written over with what it held: the row changed, what it shows did not.
        term.advance(b"\rone");
        assert!(frame(&term, &mut sent).is_none());
        term.advance(b"\x08");
        let (moved, _) = frame(&term, &mut sent).expect("the cursor moved");
        assert!(moved.rows.is_empty());
    }

    #[test]
    fn only_the_row_written_to_is_made_again() {
        let view = View::default();
        let mut term = term("one\r\ntwo\r\nthree");
        let mut sent = Sent::default();
        let palette = palette();
        let _ = capture(&term, &view, &palette, true, None, &sent).changes(&mut sent, None);
        term.advance(b"!");
        let screen = capture(&term, &view, &palette, true, None, &sent);
        let made: Vec<usize> = screen
            .rows
            .iter()
            .enumerate()
            .filter_map(|(line, row)| row.as_ref().map(|_| line))
            .collect();
        assert_eq!(made, [2]);
    }

    fn frame(term: &Term, view: &View, sent: &mut Sent) -> Frame {
        capture(term, view, &palette(), true, None, sent)
            .changes(sent, None)
            .map_or_else(Frame::default, |(frame, _)| frame)
    }

    #[test]
    fn a_screen_past_what_a_frame_takes_goes_over_several() {
        let (columns, lines) = (20, 10);
        let limit = 2000;
        let mut term = Term::new(columns, lines, Options::default());
        let cell = "e\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}\u{301}";
        term.advance(cell.repeat(columns * lines - 1).as_bytes());
        let view = View::default();
        let mut sent = Sent::default();
        let mut got = vec![false; lines];
        let mut frames = 0;
        loop {
            let (frame, whole) = capture(&term, &view, &palette(), true, None, &sent)
                .changes_within(&mut sent, None, limit)
                .expect("rows remain");
            frames += 1;
            assert!(caret_protocol::wire::encode(&frame).len() < limit);
            for row in &frame.rows {
                assert!(!std::mem::replace(&mut got[usize::from(row.line)], true));
            }
            if whole {
                break;
            }
        }
        assert!(frames > 1);
        assert!(got.iter().all(|&line| line), "every row arrived");
        let next = capture(&term, &view, &palette(), true, None, &sent).changes(&mut sent, None);
        assert_eq!(next, None);
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
        let (frame, _) = capture(&term, &view, &palette(), false, None, &Sent::default())
            .changes(&mut Sent::default(), None)
            .expect("the first frame");
        assert_eq!(
            frame.cursor.map(|cursor| (cursor.point.line, cursor.shape)),
            Some((3, CursorShape::HollowBlock))
        );
    }
}
