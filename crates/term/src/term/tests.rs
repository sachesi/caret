use super::*;
use crate::grid::Text;
use crate::style::{Color, Underline};

fn term(columns: usize, lines: usize) -> Term {
    Term::new(
        columns,
        lines,
        Options {
            version: "caret 0.2.0".to_owned(),
            ..Options::default()
        },
    )
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

fn screen(term: &Term) -> Vec<String> {
    term.screen().rows().iter().map(row_text).collect()
}

fn history(term: &Term) -> Vec<String> {
    term.history()
        .lines()
        .map(|line| {
            line.wrap(line.width().max(1), Style::default())
                .iter()
                .map(row_text)
                .collect()
        })
        .collect()
}

fn fed(columns: usize, lines: usize, input: &str) -> Term {
    let mut term = term(columns, lines);
    term.advance(input.as_bytes());
    term
}

#[test]
fn lines_scroll_into_history() {
    let term = fed(5, 2, "hello\r\nworld\r\nend");
    assert_eq!(screen(&term), ["world", "end"]);
    assert_eq!(history(&term), ["hello"]);
    assert!(!term.history().is_open());
}

#[test]
fn a_wrapped_line_goes_to_history_whole() {
    let term = fed(4, 2, "abcdefgh\r\n1\r\n2");
    assert_eq!(history(&term), ["abcdefgh"]);
    assert_eq!(screen(&term), ["1", "2"]);
}

#[test]
fn cursor_movement_and_erasing() {
    let term = fed(6, 3, "abcdef\r\nghijkl\x1b[1;3H\x1b[K\x1b[2;2H\x1b[1K");
    assert_eq!(screen(&term), ["ab", "  ijkl", ""]);
    assert_eq!(term.cursor(), Some((1, 1)));
}

#[test]
fn inserting_and_deleting_characters_and_lines() {
    let term = fed(
        6,
        3,
        "abcdef\x1b[1;2H\x1b[2@\x1b[2;1Hxyz\x1b[2;1H\x1b[P\x1b[3;1H\x1b[1;1H\x1b[L",
    );
    assert_eq!(screen(&term), ["", "a  bcd", "yz"]);
}

#[test]
fn the_scrolling_region_keeps_the_lines_outside_it() {
    let term = fed(3, 4, "top\r\n1\r\n2\r\nbot\x1b[2;3r\x1b[3;1H\nnew");
    assert_eq!(screen(&term), ["top", "2", "new", "bot"]);
    assert!(term.history().is_empty());
}

#[test]
fn wide_characters_take_two_cells_and_wrap_whole() {
    let term = fed(5, 2, "ab日本");
    assert_eq!(screen(&term), ["ab日", "本"]);
    assert!(term.screen().row(0).wrapped);
    assert!(
        term.screen().row(0).cells[4]
            .style
            .flags
            .contains(Flags::SPACER)
    );
    assert_eq!(term.cursor(), Some((1, 2)));
}

#[test]
fn marks_join_the_character_before() {
    let term = fed(5, 1, "e\u{301}x");
    assert_eq!(term.screen().row(0).text(0), Text::Cluster("e\u{301}"));
    assert_eq!(term.screen().row(0).text(1), Text::Char('x'));
}

#[test]
fn a_letter_after_a_prepended_mark_joins_it_when_clusters_are_asked_for() {
    let mut term = Term::new(10, 2, Options::default());
    term.advance("\x1b[?2027h\u{600}ab".as_bytes());
    let row = term.screen().row(0);
    assert_eq!(row.text(0), Text::Cluster("\u{600}a"));
    assert_eq!(row.text(2), Text::Char('b'));
    assert_eq!(term.cursor_at(), (0, 3));
}

#[test]
fn grapheme_clusters_join_when_asked() {
    let family = "👩\u{200d}💻";
    let plain = fed(8, 1, family);
    assert_eq!(plain.cursor(), Some((0, 4)));
    let clustered = fed(8, 1, &format!("\x1b[?2027h{family}x"));
    assert_eq!(clustered.screen().row(0).text(0), Text::Cluster(family));
    assert_eq!(row_text(clustered.screen().row(0)), format!("{family}x"));
    assert_eq!(clustered.cursor(), Some((0, 3)));
}

#[test]
fn graphic_rendition_sets_colours_and_attributes() {
    let term = fed(
        8,
        1,
        "\x1b[1;4:3;38;5;196;48:2::1:2:3;58;2;9;8;7mx\x1b[22;24;39my\x1b[0mz",
    );
    let cells = &term.screen().row(0).cells;
    let x = cells[0].style;
    assert!(x.flags.contains(Flags::BOLD));
    assert_eq!(x.flags.underline(), Underline::Curly);
    assert_eq!(x.fg, Color::indexed(196));
    assert_eq!(x.bg, Color::rgb(1, 2, 3));
    assert_eq!(x.underline, Color::rgb(9, 8, 7));
    let y = cells[1].style;
    assert!(!y.flags.contains(Flags::BOLD));
    assert_eq!(y.flags.underline(), Underline::None);
    assert_eq!(y.fg, Color::DEFAULT);
    assert_eq!(y.bg, Color::rgb(1, 2, 3));
    assert_eq!(cells[2].style, Style::default());
}

#[test]
fn the_alternate_screen_leaves_the_main_one_and_its_cursor() {
    let mut term = fed(5, 2, "shell\r\n$ ");
    term.advance(b"\x1b[?1049h\x1b[2J\x1b[Hfull");
    assert!(term.alt_screen());
    assert_eq!(screen(&term), ["full", ""]);
    term.advance(b"\x1b[?1049l");
    assert_eq!(screen(&term), ["shell", "$"]);
    assert_eq!(term.cursor(), Some((1, 2)));
}

#[test]
fn line_drawing_characters_come_from_the_graphics_set() {
    let term = fed(5, 1, "\x1b(0lqk\x1b(Bq");
    assert_eq!(screen(&term), ["┌─┐q"]);
}

#[test]
fn questions_are_answered() {
    let mut term = fed(
        10,
        5,
        "\x1b[3;4H\x1b[6n\x1b[c\x1b[>c\x1b[?2026$p\x1b[?2026h\x1b[?2026$p\x1b[18t\x1b[>q",
    );
    let replies = String::from_utf8(term.take_replies()).unwrap();
    assert_eq!(
        replies,
        "\x1b[3;4R\x1b[?62;22c\x1b[>1;200;0c\x1b[?2026;2$y\x1b[?2026;1$y\x1b[8;5;10t\x1bP>|caret 0.2.0\x1b\\"
    );
    assert!(term.modes().synchronized);
}

#[test]
fn colours_are_changed_and_told() {
    let mut term = fed(
        5,
        1,
        "\x1b]4;1;rgb:12/34/56\x07\x1b]4;1;?\x07\x1b]11;?\x1b\\",
    );
    let replies = String::from_utf8(term.take_replies()).unwrap();
    assert_eq!(
        replies,
        "\x1b]4;1;rgb:1212/3434/5656\x07\x1b]11;rgb:1d1d/1d1d/2020\x1b\\"
    );
    term.advance(b"\x1b]104\x07");
    assert_eq!(
        term.palette().slot(1),
        Some(crate::palette::Base::default().ansi[1])
    );
}

#[test]
fn links_are_given_to_the_text_between() {
    let term = fed(
        10,
        1,
        "a\x1b]8;id=x;https://example.org\x1b\\bc\x1b]8;;\x1b\\d",
    );
    let cells = &term.screen().row(0).cells;
    assert_eq!(cells[0].style.link, 0);
    assert_eq!(term.link(cells[1].style.link), Some("https://example.org"));
    assert_eq!(cells[1].style.link, cells[2].style.link);
    assert_eq!(cells[3].style.link, 0);
}

#[test]
fn shell_reports_become_events_and_marks() {
    let mut term = fed(
        10,
        2,
        "\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07\x1b]133;D;2\x07\x1b]7;file:///tmp\x07\x1b]777;notify;Done;it worked\x07",
    );
    assert!(term.screen().row(0).prompt);
    assert!(!term.screen().row(1).prompt);
    assert_eq!(
        term.take_events(),
        [
            Event::Report(Report::CommandStarted),
            Event::Report(Report::CommandFinished(Some(2))),
            Event::Report(Report::Directory(PathBuf::from("/tmp"))),
            Event::Report(Report::Notification {
                title: "Done".to_owned(),
                body: "it worked".to_owned(),
            }),
        ]
    );
}

#[test]
fn programs_copy_to_the_clipboard() {
    let mut term = fed(5, 1, "\x1b]52;c;aGVsbG8=\x07\x1b]52;c;?\x07");
    assert_eq!(
        term.take_events(),
        [Event::Clipboard {
            primary: false,
            text: "hello".to_owned()
        }]
    );
}

#[test]
fn keyboard_flags_are_pushed_and_popped_per_screen() {
    let mut term = fed(5, 1, "\x1b[>1u\x1b[=4;2u");
    assert_eq!(term.keyboard_flags(), 5);
    term.advance(b"\x1b[?1049h");
    assert_eq!(term.keyboard_flags(), 0);
    term.advance(b"\x1b[?1049l\x1b[?u\x1b[<u");
    assert_eq!(term.keyboard_flags(), 0);
    assert_eq!(term.take_replies(), b"\x1b[?5u");
}

#[test]
fn narrowing_wraps_the_screen_and_widening_unwraps_it() {
    let mut term = fed(10, 3, "abcdefgh\r\n$ ");
    term.resize(4, 3);
    assert_eq!(screen(&term), ["efgh", "$", ""]);
    assert_eq!(history(&term), ["abcd"]);
    assert!(term.history().is_open());
    assert_eq!(term.cursor(), Some((1, 2)));
    term.resize(10, 3);
    assert_eq!(screen(&term), ["abcdefgh", "$", ""]);
    assert!(term.history().is_empty());
    assert_eq!(term.cursor(), Some((1, 2)));
}

#[test]
fn a_shorter_screen_gives_up_blank_rows_before_text() {
    let mut term = fed(5, 5, "one\r\ntwo");
    term.resize(5, 3);
    assert_eq!(screen(&term), ["one", "two", ""]);
    term.resize(5, 2);
    assert_eq!(screen(&term), ["one", "two"]);
    term.resize(5, 1);
    assert_eq!(screen(&term), ["two"]);
    assert_eq!(history(&term), ["one"]);
    term.resize(5, 4);
    assert_eq!(screen(&term), ["one", "two", "", ""]);
    assert_eq!(term.cursor(), Some((1, 3)));
}

#[test]
fn the_alternate_screen_is_cut_rather_than_wrapped() {
    let mut term = fed(6, 2, "\x1b[?1049habcdef");
    term.resize(3, 2);
    assert_eq!(screen(&term), ["abc", ""]);
    term.advance(b"\x1b[?1049l");
    assert_eq!(screen(&term), ["", ""]);
}

#[test]
fn repeat_and_tabs() {
    let term = fed(20, 1, "a\x1b[3b\tx\x1b[3g\r\tz");
    assert_eq!(screen(&term), ["aaaa    x          z"]);
}

#[test]
fn text_survives_any_resizes_in_order() {
    let mut seed = 0x0e51_2e00_u64;
    let mut below = |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        (seed % bound as u64) as usize
    };
    let pieces = [
        "hello ",
        "wörld",
        "日本",
        "\r\n",
        "abcdefghijklmnop",
        "x",
        "e\u{301}",
    ];
    for _ in 0..2000 {
        let mut term = term(2 + below(14), 1 + below(6));
        let mut written = String::new();
        let mut steps = Vec::new();
        for _ in 0..4 {
            let text: String = (0..below(8)).map(|_| pieces[below(pieces.len())]).collect();
            term.advance(text.as_bytes());
            written.push_str(&text);
            let (columns, lines) = (2 + below(14), 1 + below(6));
            term.resize(columns, lines);
            steps.push(format!("{text:?} {columns}x{lines}"));
        }
        let shown: String = term
            .history()
            .lines()
            .map(|line| {
                line.cells()
                    .iter()
                    .map(|(text, _)| *text)
                    .collect::<String>()
            })
            .chain(term.screen().rows().iter().map(row_text))
            .collect();
        let wanted: String = written.chars().filter(|c| !c.is_whitespace()).collect();
        let shown: String = shown.chars().filter(|c| !c.is_whitespace()).collect();
        assert_eq!(shown, wanted, "{steps:?}");
    }
}

#[test]
fn clearing_the_screen_keeps_its_text_in_history() {
    let term = fed(5, 3, "one\r\ntwo\x1b[2J");
    assert_eq!(history(&term), ["one", "two"]);
    assert_eq!(screen(&term), ["", "", ""]);
    assert_eq!(term.cursor(), Some((1, 3)));
}

#[test]
fn text_that_reached_the_last_column_waits_to_wrap() {
    assert_eq!(screen(&fed(5, 2, "abcde\x1b[K\tx")), ["abcde", "x"]);
    assert_eq!(screen(&fed(5, 2, "abcde\x1b[3Ix")), ["abcde", "x"]);
    let mut term = fed(4, 2, "abcd");
    term.resize(6, 2);
    term.advance(b"e");
    assert_eq!(screen(&term), ["abcde", ""]);
}

#[test]
fn a_repeat_follows_cursor_moves() {
    assert_eq!(screen(&fed(10, 1, "a\x1b[3C\x1b[2b")), ["a   aa"]);
    assert_eq!(screen(&fed(4, 2, "日\x1b[2b")), ["日日", "日"]);
}

#[test]
fn writing_over_the_blank_before_a_wrapped_wide_character_keeps_its_neighbour() {
    let term = fed(3, 2, "ab日\x1b[1;3Hc");
    assert_eq!(screen(&term), ["abc", "日"]);
}

#[test]
fn marks_stop_adding_to_a_cell_past_its_limit() {
    let mark = "\u{301}";
    let mut flooded = term(10, 2);
    flooded.advance(format!("a{}b", mark.repeat(100_000)).as_bytes());
    let row = flooded.screen().row(0);
    let Text::Cluster(cluster) = row.text(0) else {
        panic!("{:?}", row.text(0));
    };
    assert!(cluster.starts_with('a') && cluster.len() <= MAX_CLUSTER_BYTES);
    assert_eq!(row.cells[1].c, 'b');

    let mut joined = term(10, 2);
    joined.advance(b"\x1b[?2027h");
    joined.advance(format!("👍{}b", "\u{200d}".repeat(100_000)).as_bytes());
    let row = joined.screen().row(0);
    let Text::Cluster(cluster) = row.text(0) else {
        panic!("{:?}", row.text(0));
    };
    assert!(cluster.len() <= MAX_CLUSTER_BYTES);
}

#[test]
fn the_cell_limit_keeps_the_longest_emoji_whole() {
    let family = "👩🏽\u{200d}❤\u{fe0f}\u{200d}💋\u{200d}👨🏿";
    let mut term = term(10, 2);
    term.advance(b"\x1b[?2027h");
    term.advance(family.as_bytes());
    assert_eq!(term.screen().row(0).text(0), Text::Cluster(family));
}

#[test]
fn repeating_a_character_writes_a_screenful_at_most() {
    let mut term = term(10, 3);
    term.advance(b"a\x1b[65535b");
    assert!(term.history().rows() <= 4, "{}", term.history().rows());
}

#[test]
fn tabbing_stops_at_the_edge() {
    let mut term = term(80, 2);
    term.advance(b"\x1b[65535I");
    assert_eq!(term.cursor_at(), (0, 79));
    term.advance(b"\x1b[65535Z");
    assert_eq!(term.cursor_at(), (0, 0));
}

#[test]
fn a_run_of_bells_is_one_event() {
    let mut term = term(10, 2);
    term.advance(&[0x07; 1000]);
    assert_eq!(term.take_events(), [Event::Bell]);
}

#[test]
fn long_titles_are_cut_between_characters() {
    let mut term = term(10, 2);
    term.advance(format!("\x1b]0;{}\x07", "日".repeat(10_000)).as_bytes());
    assert!(term.title().len() <= MAX_TEXT_BYTES);
    assert!(term.title().ends_with('日'));
}

#[test]
fn a_very_long_address_is_not_kept_as_a_link() {
    let mut term = term(10, 2);
    let long = "a".repeat(MAX_LINK_BYTES);
    term.advance(format!("\x1b]8;;http://{long}\x07x").as_bytes());
    assert_eq!(term.screen().row(0).cells[0].style.link, 0);
    assert_eq!(term.screen().row(0).cells[0].c, 'x');
}

#[test]
fn listing_the_same_addresses_again_takes_no_more_link_numbers() {
    let mut term = term(10, 2);
    for round in 0..70_000 {
        let name = round % 3;
        term.advance(format!("\x1b]8;;file:///{name}\x07x\x1b]8;;\x07\r").as_bytes());
    }
    term.advance(b"\x1b]8;;file:///other\x07y");
    assert_eq!(
        term.link(term.screen().row(0).cells[0].style.link),
        Some("file:///other")
    );
}

#[test]
fn inserting_where_a_short_rows_blank_moved_to_the_first_column_is_safe() {
    let mut term = term(3, 2);
    // The wide character leaves a blank at the end of the first row, which deleting the
    // cells before it brings to the first column.
    term.advance("ab日\x1b[A\x1b[1G\x1b[2P\x1b[@".as_bytes());
    assert_eq!(term.screen().row(0).len(), 3);
}

#[test]
fn a_program_that_asks_is_told_when_the_colours_go_dark_or_light() {
    use crate::palette::{Base, Rgb};
    let with_background = |value| Base {
        background: Rgb::from_u32(value),
        ..Base::default()
    };
    let mut term = fed(10, 5, "");
    term.set_palette_base(with_background(0xffffff));
    assert!(term.take_replies().is_empty(), "nobody asked yet");

    term.advance(b"\x1b[?2031h\x1b[?2031$p\x1b[?996n");
    assert_eq!(term.take_replies(), b"\x1b[?2031;1$y\x1b[?997;2n");
    term.set_palette_base(with_background(0xfafafa));
    assert!(term.take_replies().is_empty(), "still light");
    term.set_palette_base(with_background(0x1d1d20));
    assert_eq!(term.take_replies(), b"\x1b[?997;1n");

    term.advance(b"\x1b[?2031l");
    term.set_palette_base(with_background(0xffffff));
    assert!(term.take_replies().is_empty(), "no longer asked");
}

#[test]
fn a_mark_on_a_cell_after_the_screen_narrowed_is_kept_alone() {
    let mut term = Term::new(4, 2, Options::default());
    // On the alternate screen rows are cut rather than wrapped again: the cut halves the
    // wide character, whose cell has marks.
    term.advance("\x1b[?1049h\x1b[1;2H日\u{301}".as_bytes());
    term.resize(2, 2);
    assert_eq!(term.screen().row(0).text(1), Text::Char(' '));
    term.advance("\x1b[1;2He\u{302}".as_bytes());
    assert_eq!(term.screen().row(0).text(1), Text::Cluster("e\u{302}"));
}

#[test]
fn erasing_below_text_that_reached_the_last_column_keeps_its_last_character() {
    let mut term = term(4, 2);
    term.advance(b"abcd\x1b[J");
    assert_eq!(screen(&term)[0], "abcd");
    // Nothing is held back once the cursor has moved.
    term.advance(b"\x1b[1;4H\x1b[J");
    assert_eq!(screen(&term)[0], "abc");
}

#[test]
fn text_after_the_screen_starts_over_is_not_part_of_the_line_above_it() {
    for clear in ["\x1bc", "\x1b[2J\x1b[H", "\x1b[H\x1b[J"] {
        let mut term = term(4, 2);
        // The first row of a long line leaves the screen, and the rest of it goes.
        term.advance(b"aaaabbbbcc");
        assert!(term.history().is_open());
        term.advance(b"\x1b[H\x1b[2K\x1b[2;1H\x1b[2K\x1b[H");
        term.advance(clear.as_bytes());
        term.advance(b"new\r\n\r\n");
        assert_eq!(
            history(&term).last().map(String::as_str),
            Some("new"),
            "after {clear:?}"
        );
    }
}

#[test]
fn the_alternate_screen_takes_memory_once_it_is_used() {
    let mut term = term(10, 3);
    assert_eq!(term.alt.lines(), 0);
    term.resize(20, 5);
    assert_eq!(term.alt.lines(), 0);
    // Written to without being cleared first, it is there all the same.
    term.advance(b"\x1b[?47hup\x1b[?47l");
    assert_eq!(term.alt.lines(), 5);
    term.resize(30, 2);
    assert_eq!((term.alt.lines(), term.alt.row(0).len()), (2, 30));
    term.advance(b"\x1b[?47h");
    assert_eq!(screen(&term)[0], "up");
    term.advance(b"\x1bc");
    assert_eq!(term.alt.lines(), 0);
}

#[test]
fn editing_at_the_end_of_a_full_line_leaves_the_next_letter_on_it() {
    for edit in ["\x1b[X", "\x1b[P", "\x1b[@"] {
        let mut term = term(10, 2);
        term.advance(b"abcdefghij");
        term.advance(edit.as_bytes());
        term.advance(b"Z");
        assert_eq!(screen(&term), ["abcdefghiZ", ""], "after {edit:?}");
    }
}

/// A link to a new address on the first cell of the second line, over the last one's.
fn over_the_last_link(term: &mut Term, number: usize) {
    term.advance(format!("\x1b[2H\x1b]8;;file:///{number}\x07x\x1b]8;;\x07").as_bytes());
}

#[test]
fn links_no_text_has_any_more_give_their_numbers_back() {
    let mut term = term(10, 2);
    term.advance(b"\x1b]8;;file:///kept\x07a\x1b]8;;\x07");
    let kept = term.screen().row(0).cells[0].style.link;
    for number in 0..200_000 {
        over_the_last_link(&mut term, number);
    }
    assert_eq!(term.link(kept), Some("file:///kept"));
    assert_eq!(
        term.link(term.screen().row(1).cells[0].style.link),
        Some("file:///199999")
    );
}

#[test]
fn a_link_in_the_history_or_kept_for_the_next_text_keeps_its_address() {
    let mut term = term(10, 2);
    term.advance(b"\x1b]8;;file:///above\x07a\x1b]8;;\x07\r\n\n\n");
    term.advance(b"\x1b]8;;file:///open\x07");
    for number in 0..70_000 {
        term.advance(format!("\x1b7\x1b[2H\x1b]8;;file:///{number}\x07x\x1b8").as_bytes());
    }
    let above = term.history().line(0).cells()[0].1.link;
    assert_eq!(term.link(above), Some("file:///above"));
    assert_eq!(term.link(term.cursor.style.link), Some("file:///open"));
}

#[test]
fn links_in_use_are_kept_when_there_is_no_room_for_more() {
    let mut term = Term::new(
        10,
        2,
        Options {
            scrollback: 100_000,
            ..Options::default()
        },
    );
    for number in 0..usize::from(LinkId::MAX) {
        term.advance(format!("\x1b]8;;file:///{number}\x07x\x1b]8;;\x07\r\n").as_bytes());
    }
    term.advance(b"\x1b]8;;file:///more\x07y\x1b]8;;\x07");
    let cursor_row = term.screen().row(term.cursor_at().0);
    assert_eq!(cursor_row.cells[0].style.link, 0);
    assert_eq!(cursor_row.cells[0].c, 'y');
    let first = term.history().line(0).cells()[0].1.link;
    assert_eq!(term.link(first), Some("file:///0"));
}

#[test]
fn long_addresses_take_a_bounded_room() {
    let mut term = term(10, 2);
    let long = "a".repeat(3000);
    for number in 0..30_000 {
        term.advance(format!("\x1b[2H\x1b]8;;http://{long}/{number}\x07x\x1b]8;;\x07").as_bytes());
    }
    assert!(term.links.bytes <= MAX_LINK_TABLE_BYTES + 2 * MAX_LINK_BYTES);
    assert_eq!(
        term.link(term.screen().row(1).cells[0].style.link),
        Some(format!("http://{long}/29999").as_str())
    );
}

#[test]
fn text_keeps_its_own_address_while_numbers_are_taken_back() {
    let mut term = term(12, 4);
    let mut random = 1_u32;
    for number in 0..200_000_u32 {
        random = random.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        let letter = char::from(b'a' + (number % 26) as u8);
        term.advance(format!("\x1b]8;;file:///{number}\x07{letter}\x1b]8;;\x07").as_bytes());
        match random >> 24 {
            0..=19 => term.advance(b"\r\n"),
            20 => term.advance(b"\x1b[2J"),
            21 => term.advance(b"\x1b[?1049h"),
            22 => term.advance(b"\x1b[?1049l"),
            23 => term.resize(8 + (random >> 12) as usize % 8, 4),
            24 => term.advance(b"\x1b7"),
            25 => term.advance(b"\x1b8"),
            _ => {}
        }
    }
    let check = |letter: char, link: LinkId| {
        if link == 0 {
            return;
        }
        let uri = term.link(link).expect("a cell's link is kept");
        let number: u32 = uri
            .trim_start_matches("file:///")
            .parse()
            .expect("an address");
        assert_eq!(letter, char::from(b'a' + (number % 26) as u8), "{uri}");
    };
    for cell in term.screen().rows().iter().flat_map(|row| &row.cells) {
        check(cell.c, cell.style.link);
    }
    for line in term.history().lines() {
        for (text, style) in line.cells() {
            check(text.chars().next().unwrap_or(' '), style.link);
        }
    }
}
