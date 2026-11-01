use std::fmt::Write;

use super::*;
use crate::grid::Text;
use crate::search::{Progress, Search};
use crate::style::Flags;
use crate::term::Options;

fn fed(columns: usize, lines: usize, input: &str) -> Term {
    let mut term = Term::new(columns, lines, Options::default());
    term.advance(input.as_bytes());
    term
}

fn row_text(row: &Row) -> String {
    let mut text = String::new();
    for (column, cell) in row.cells.iter().enumerate() {
        if cell.style.flags.contains(Flags::SPACER) {
            continue;
        }
        match row.text(column) {
            Text::Char(c) => text.push(c),
            Text::Cluster(cluster) => text.push_str(cluster),
        }
    }
    text.trim_end().to_owned()
}

fn shown(view: &View, term: &Term) -> Vec<String> {
    view.shown(term)
        .iter()
        .map(|shown| row_text(&shown.row))
        .collect()
}

fn numbered(count: usize) -> String {
    (0..count).fold(String::new(), |mut text, line| {
        let _ = write!(text, "{line}\r\n");
        text
    })
}

#[test]
fn a_scrolled_view_stays_on_its_lines_as_output_goes_on() {
    let mut term = fed(10, 3, &numbered(10));
    let mut view = View::default();
    let mut following = View::default();
    view.scroll(&term, 2);
    assert_eq!(shown(&view, &term), ["6", "7", "8"]);
    assert_eq!(view.offset(&term), 2);
    term.advance(b"10\r\n11\r\n");
    assert_eq!(shown(&view, &term), ["6", "7", "8"]);
    assert_eq!(view.offset(&term), 4);
    assert_eq!(shown(&following, &term), ["10", "11", ""]);
    following.scroll(&term, -5);
    assert!(following.at_bottom(&term));
    view.set_offset(&term, usize::MAX);
    assert_eq!(shown(&view, &term), ["0", "1", "2"]);
    assert_eq!(view.offset(&term), term.history().rows());
}

#[test]
fn history_rows_are_wrapped_to_the_width_and_counted_exactly() {
    let mut term = fed(8, 2, "abcdefghij\r\nx日本語y\r\n\r\n");
    term.resize(4, 2);
    let mut view = View::default();
    view.set_offset(&term, usize::MAX);
    let history = term.history();
    let rows: usize = history
        .lines()
        .map(|line| line.wrap(4, Style::default()).len())
        .sum();
    assert_eq!(history.rows(), rows);
    assert_eq!(shown(&view, &term), ["abcd", "efgh"]);
    view.scroll(&term, -2);
    assert_eq!(shown(&view, &term), ["ij", "x日"]);
    view.scroll(&term, -1);
    assert_eq!(shown(&view, &term), ["x日", "本語"]);
}

#[test]
fn a_selection_follows_its_text_into_the_history_and_through_a_resize() {
    let mut term = fed(6, 4, "one two three\r\n");
    let mut view = View::default();
    view.select(&term, Unit::Cell, 0, 4, false);
    view.extend(&term, 1, 4, true);
    assert_eq!(view.selected_text(&term).as_deref(), Some("two thr"));
    term.advance(b"\r\n\r\n\r\n");
    assert_eq!(view.selected_text(&term).as_deref(), Some("two thr"));
    term.resize(20, 3);
    assert_eq!(view.selected_text(&term).as_deref(), Some("two thr"));
    view.set_offset(&term, usize::MAX);
    assert_eq!(view.selected_cells(&term), Some(((0, 4), (0, 10))));
}

#[test]
fn words_and_lines_are_selected_whole() {
    let term = fed(10, 3, "say \"hello world\"\r\nnext");
    let mut view = View::default();
    view.select(&term, Unit::Word, 0, 7, false);
    assert_eq!(view.selected_text(&term).as_deref(), Some("hello"));
    view.extend(&term, 1, 1, false);
    assert_eq!(view.selected_text(&term).as_deref(), Some("hello world"));
    view.extend(&term, 2, 1, false);
    assert_eq!(
        view.selected_text(&term).as_deref(),
        Some("hello world\"\nnext")
    );
    view.select(&term, Unit::Line, 1, 3, false);
    assert_eq!(
        view.selected_text(&term).as_deref(),
        Some("say \"hello world\"")
    );
}

#[test]
fn a_click_without_a_drag_selects_nothing() {
    let term = fed(10, 2, "text");
    let mut view = View::default();
    view.select(&term, Unit::Cell, 0, 1, true);
    view.extend(&term, 0, 2, false);
    assert_eq!(view.selected(&term), None);
    view.select_all(&term);
    assert_eq!(view.selected_text(&term).as_deref(), Some("text"));
}

#[test]
fn output_written_into_a_selection_lets_go_of_it() {
    let mut term = fed(10, 4, "keep\r\nchange\r\n");
    term.take_written();
    let mut view = View::default();
    view.select(&term, Unit::Line, 0, 0, false);
    term.advance(b"more");
    view.written(term.take_written().unwrap_or(u64::MAX));
    assert!(view.selected(&term).is_some());
    view.select(&term, Unit::Line, 1, 0, false);
    term.advance(b"\x1b[2;1Hchanged");
    view.written(term.take_written().unwrap_or(u64::MAX));
    assert_eq!(view.selected(&term), None);
}

#[test]
fn addresses_are_found_across_wrapped_rows() {
    let term = fed(10, 3, "go https://example.org/a now");
    let view = View::default();
    let link = view.link_at(&term, 1, 2).map(|link| link.uri);
    assert_eq!(link.as_deref(), Some("https://example.org/a"));
    assert_eq!(view.link_at(&term, 0, 1), None);
}

#[test]
fn links_programs_give_cover_their_cells() {
    let term = fed(
        20,
        2,
        "a\x1b]8;;https://example.org\x1b\\link\x1b]8;;\x1b\\ \x1b]8;;app://x\x1b\\no\x1b]8;;\x1b\\",
    );
    let view = View::default();
    let link = view.link_at(&term, 0, 2);
    let Some(Link {
        uri,
        span: Span::Program(id),
    }) = link
    else {
        panic!("no link: {link:?}");
    };
    assert_eq!(uri, "https://example.org");
    assert_eq!(term.screen().row(0).cells[4].style.link, id);
    assert_eq!(view.link_at(&term, 0, 7), None);
}

#[test]
fn prompts_are_scrolled_to_in_turn() {
    let mut input = String::new();
    for prompt in 0..3 {
        let _ = write!(input, "\x1b]133;A\x07$ cmd{prompt}\r\n");
        input.push_str(&numbered(4));
    }
    let term = fed(10, 3, &input);
    let mut view = View::default();
    view.scroll_to_prompt(&term, true);
    assert_eq!(shown(&view, &term)[0], "$ cmd2");
    view.scroll_to_prompt(&term, true);
    assert_eq!(shown(&view, &term)[0], "$ cmd1");
    view.scroll_to_prompt(&term, false);
    assert_eq!(shown(&view, &term)[0], "$ cmd2");
    view.scroll_to_prompt(&term, false);
    assert!(view.at_bottom(&term));
}

#[test]
fn searches_go_round_every_line_in_steps() {
    let mut input = String::new();
    for line in 0..100 {
        match line {
            10 => input.push_str("a Needle here\r\n"),
            60 => input.push_str("needle\r\n"),
            line => {
                let _ = write!(input, "hay {line}\r\n");
            }
        }
    }
    let mut term = fed(20, 5, &input);
    term.set_options(Options {
        scrollback: 1000,
        ..Options::default()
    });
    let mut view = View::default();
    let find = |view: &mut View, text: &str, upwards: bool| {
        let from = view.search_from(&term, upwards);
        let mut search = Search::new(&term, text, upwards, from)?;
        loop {
            match search.step(&term, 7) {
                Progress::Found(first, last) => {
                    view.select_cells(first, last);
                    view.reveal(&term, first);
                    return view.selected_text(&term);
                }
                Progress::Missing => return None,
                Progress::Going => {}
            }
        }
    };
    assert_eq!(find(&mut view, "needle", true).as_deref(), Some("needle"));
    assert_eq!(shown(&view, &term)[0], "needle");
    assert_eq!(find(&mut view, "needle", true).as_deref(), Some("Needle"));
    assert_eq!(find(&mut view, "needle", true).as_deref(), Some("needle"));
    assert_eq!(find(&mut view, "needle", false).as_deref(), Some("Needle"));
    assert_eq!(find(&mut view, "Needle", false).as_deref(), Some("Needle"));
    assert_eq!(find(&mut view, "thread", false), None);
}

#[test]
fn an_address_is_found_and_a_word_selected_in_a_line_as_long_as_the_history() {
    let columns = 80;
    let mut term = Term::new(columns, 5, Options::default());
    let filler = "x".repeat(40 * ADDRESS_REACH);
    term.advance(format!("{filler} https://example.org/a {filler}").as_bytes());
    let mut view = View::default();
    let line = *term.serials().end();
    assert!(
        term.line_width(line)
            .is_some_and(|width| width > 80 * ADDRESS_REACH)
    );
    let address = filler.len() + 1 + "https://".len();
    let top = term
        .history()
        .at_row(address / columns)
        .expect("kept in the history");
    view.top = Some((term.history().serial(top.0), top.1));
    let link = view
        .link_at(&term, 0, address % columns)
        .expect("an address");
    assert_eq!(link.uri, "https://example.org/a");
    view.select(&term, Unit::Word, 0, address % columns, false);
    assert_eq!(
        view.selected_text(&term).as_deref(),
        Some("//example.org/a")
    );
    // A word longer than a stretch read at a time is taken whole.
    let word = filler.len() + 30;
    let long = Place {
        line: term.history().serial(top.0),
        cell: word + WORD_STRETCH,
    };
    assert_eq!(word_edge(&term, long, false), filler.len() + 1 + 22);
}

#[test]
fn the_row_below_a_scrolled_view_is_the_one_a_row_further_down_shows_last() {
    let mut term = Term::new(5, 3, Options::default());
    term.advance(b"1\r\n2\r\nlong line\r\n4\r\n5\r\n6\r\n7");
    let mut view = View::default();
    assert_eq!(
        view.shown_and_below(&term).len(),
        3,
        "nothing is below the screen"
    );
    let texts = |rows: Vec<Shown<'_>>| -> Vec<String> {
        rows.iter().map(|shown| row_text(&shown.row)).collect()
    };
    for offset in 1..=term.history().rows() {
        view.set_offset(&term, offset);
        let with_below = texts(view.shown_and_below(&term));
        assert_eq!(with_below.len(), 4);
        assert_eq!(with_below[..3], texts(view.shown(&term)));
        view.set_offset(&term, offset - 1);
        assert_eq!(with_below[1..], texts(view.shown(&term)));
    }
}
