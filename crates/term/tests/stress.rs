//! Random programs and random clients against the emulator: whatever bytes come in, it
//! neither panics nor breaks what its readers rely on. Every failure prints the seed that
//! gives it. `CARET_STRESS_ROUNDS` runs more of them than a normal test run has time for:
//!
//!     CARET_STRESS_ROUNDS=20000 cargo test --release -p caret-term --test stress

use caret_term::search::{Progress, Search};
use caret_term::style::{Flags, Style};
use caret_term::view::{Unit, View};
use caret_term::{Options, Term};

/// xorshift64*: the same seed gives the same run, everywhere.
struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() >> 33) as usize % limit
    }

    fn pick<T: Copy>(&mut self, items: &[T]) -> T {
        items[self.below(items.len())]
    }
}

const NUMBERS: [u32; 14] = [0, 1, 2, 3, 4, 5, 7, 8, 13, 80, 255, 256, 65535, 99999];

/// Final bytes of the control sequences the emulator acts on, and some it does not.
const FINALS: &[u8] = b"@ABCDEFGHIJKLMPSTXZ`abcdefghlmnrstuqpy";

const PRIVATE_MODES: [u32; 22] = [
    1, 6, 7, 9, 12, 25, 47, 1000, 1002, 1003, 1004, 1005, 1006, 1007, 1047, 1048, 1049, 2004, 2026,
    2027, 3, 5,
];

const TEXT: [&str; 22] = [
    "a",
    "hello ",
    " ",
    "日",
    "本語",
    "e\u{301}",
    "\u{301}",
    "\u{200d}",
    "\u{fe0f}",
    "\u{fe0e}",
    "👍",
    "👍🏽",
    "👨\u{200d}👩\u{200d}👧",
    "🇺🇦",
    "❤\u{fe0f}",
    "\r\n",
    "\n",
    "\r",
    "\t",
    "\x08",
    "\x07",
    "https://example.org/a?b=c ",
];

const ESCAPES: [&str; 22] = [
    "\x1b7", "\x1b8", "\x1bD", "\x1bE", "\x1bH", "\x1bM", "\x1bN", "\x1bO", "\x1bc", "\x1b=",
    "\x1b>", "\x1b#8", "\x1b(0", "\x1b(A", "\x1b(B", "\x1b)0", "\x0e", "\x0f", "\x18", "\x1a",
    "\x1b", "\x1b\\",
];

const COMMANDS: [&str; 20] = [
    "\x1b]0;title\x07",
    "\x1b]2;\x07",
    "\x1b]4;1;#ff0000\x07",
    "\x1b]4;1;?\x07",
    "\x1b]4;999;?\x07",
    "\x1b]10;?\x1b\\",
    "\x1b]11;rgb:10/20/30\x07",
    "\x1b]104\x07",
    "\x1b]8;;https://example.org\x07",
    "\x1b]8;id=a;https://example.org/b\x07",
    "\x1b]8;;\x07",
    "\x1b]52;c;aGVsbG8=\x07",
    "\x1b]52;c;?\x07",
    "\x1b]7;file:///tmp\x07",
    "\x1b]9;a notice\x07",
    "\x1b]777;notify;title;body\x07",
    "\x1b]777;container;push;box;toolbox\x07",
    "\x1b]777;container;pop\x07",
    "\x1b]133;A\x07",
    "\x1b]133;D;1\x07",
];

const STRINGS: [&str; 6] = [
    "\x1bP$qm\x1b\\",
    "\x1bP$qr\x1b\\",
    "\x1bP$q q\x1b\\",
    "\x1bP+q544e\x1b\\",
    "\x1bXignored\x1b\\",
    "\x1b_ignored\x07",
];

fn sequence(random: &mut Random, out: &mut Vec<u8>) {
    out.extend_from_slice(b"\x1b[");
    match random.below(8) {
        0 => out.push(b'?'),
        1 => out.push(b'>'),
        2 => out.push(b'<'),
        3 => out.push(b'='),
        _ => {}
    }
    let private = out.last() == Some(&b'?');
    let count = random.below(6);
    for index in 0..count {
        if index > 0 {
            out.push(if random.below(8) == 0 { b':' } else { b';' });
        }
        let value = if private {
            random.pick(&PRIVATE_MODES)
        } else {
            random.pick(&NUMBERS)
        };
        out.extend_from_slice(value.to_string().as_bytes());
    }
    match random.below(16) {
        0 => out.push(b'$'),
        1 => out.push(b' '),
        2 => out.push(b'!'),
        _ => {}
    }
    out.push(if private {
        random.pick(b"hlnupy")
    } else {
        random.pick(FINALS)
    });
}

/// A stretch of what a program might write, sane and not.
fn output(random: &mut Random) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..=random.below(40) {
        match random.below(16) {
            0..=5 => out.extend_from_slice(random.pick(&TEXT).as_bytes()),
            6..=10 => sequence(random, &mut out),
            11 => out.extend_from_slice(random.pick(&ESCAPES).as_bytes()),
            12 => out.extend_from_slice(random.pick(&COMMANDS).as_bytes()),
            13 => out.extend_from_slice(random.pick(&STRINGS).as_bytes()),
            14 => {
                let line = "abcdefghijklmnopqrstuvwxyz".repeat(1 + random.below(6));
                out.extend_from_slice(line.as_bytes());
            }
            _ => out.extend((0..random.below(24)).map(|_| random.next() as u8)),
        }
    }
    out
}

/// What the terminal's readers take for granted, whatever it was fed.
fn check(term: &Term, seed: u64) {
    let (columns, lines) = (term.columns(), term.lines());
    let (line, column) = term.cursor_at();
    assert!(
        line < lines,
        "seed {seed}: cursor at line {line} of {lines}"
    );
    assert!(
        column < columns,
        "seed {seed}: cursor at column {column} of {columns}"
    );
    let rows = term.screen().rows();
    assert_eq!(rows.len(), lines, "seed {seed}");
    for (at, row) in rows.iter().enumerate() {
        assert_eq!(row.len(), columns, "seed {seed}: row {at}");
        assert!(row.occupied() <= columns, "seed {seed}: row {at}");
        assert!(row.is_consistent(), "seed {seed}: row {at}: {row:?}");
        for (index, cell) in row.cells.iter().enumerate() {
            // A wide character has its second half beside it, always.
            if cell.style.flags.contains(Flags::WIDE) {
                assert!(
                    row.cells
                        .get(index + 1)
                        .is_some_and(|next| next.style.flags.contains(Flags::SPACER)),
                    "seed {seed}: row {at} column {index} is half a wide character"
                );
            }
            let _ = row.text(index);
        }
    }
    let history = term.history();
    let laid_out: usize = history.lines().map(|line| line.rows(columns)).sum();
    assert_eq!(history.rows(), laid_out, "seed {seed}");
    for index in 0..history.len() {
        let line = history.line(index);
        assert_eq!(
            line.row_starts(columns).len(),
            line.rows(columns),
            "seed {seed}"
        );
        assert_eq!(
            line.wrap(columns, Style::default()).len(),
            line.rows(columns),
            "seed {seed}"
        );
    }
    let serials = term.serials();
    assert!(serials.start() <= serials.end(), "seed {seed}");
    assert!(term.line(*serials.end()).is_some(), "seed {seed}");
    // Parts of a line are read as all of it reads.
    for serial in *serials.start()..=*serials.end() {
        let Some(line) = term.line(serial) else {
            assert_eq!(term.line_width(serial), None, "seed {seed}");
            continue;
        };
        let whole: Vec<(char, Style)> = line
            .cells()
            .iter()
            .map(|&(text, style)| (text.chars().next().unwrap_or('\0'), style))
            .collect();
        assert_eq!(term.line_width(serial), Some(whole.len()), "seed {seed}");
        let (first, end) = (serial as usize % 7, serial as usize % 7 + 30);
        let part = term.line_cells(serial, first..end).map(|(_, cells)| cells);
        let expected = whole.get(first..end.min(whole.len())).unwrap_or_default();
        assert_eq!(part.as_deref(), Some(expected), "seed {seed}");
    }
}

/// What a client might do to its view, in or out of range.
fn client(random: &mut Random, term: &Term, view: &mut View, seed: u64) {
    let far = [0, 1, 2, 7, 40, 500, 65535];
    let (line, column) = (random.pick(&far), random.pick(&far));
    let right = random.below(2) == 0;
    match random.below(12) {
        0 => view.scroll(term, random.pick(&[-65535, -3, -1, 1, 3, 65535])),
        1 => view.scroll_to_bottom(),
        2 => view.scroll_to_prompt(term, right),
        3 => view.set_offset(term, random.pick(&far)),
        4 => {
            let unit = random.pick(&[Unit::Cell, Unit::Word, Unit::Line]);
            view.select(term, unit, line, column, right);
        }
        5 => view.extend(term, line, column, right),
        6 => view.select_all(term),
        7 => view.clear_selection(),
        8 => {
            let _ = view.selected_text(term);
        }
        9 => {
            let _ = view.link_at(term, line, column);
        }
        10 => {
            let needle = random.pick(&["a", "hello", "日", "e\u{301}", "zz"]);
            let from = view.search_from(term, right);
            let mut search = Search::new(term, needle, right, from).expect("the text is not empty");
            let mut steps = 0;
            loop {
                match search.step(term, 3) {
                    Progress::Going => {
                        steps += 1;
                        assert!(steps < 100_000, "seed {seed}: a search that never ends");
                    }
                    Progress::Found(first, last) => {
                        view.select_cells(first, last);
                        view.reveal(term, first);
                        break;
                    }
                    Progress::Missing => break,
                }
            }
        }
        _ => {}
    }
    let shown = view.shown(term);
    assert_eq!(shown.len(), term.lines(), "seed {seed}");
    for row in &shown {
        assert_eq!(row.row.len(), term.columns(), "seed {seed}");
    }
    assert!(view.offset(term) <= term.history().rows(), "seed {seed}");
    if let Some(((top, _), (bottom, right))) = view.selected_cells(term) {
        assert!(top <= bottom && bottom < term.lines(), "seed {seed}");
        assert!(right < term.columns(), "seed {seed}");
    }
    let with_below = view.shown_and_below(term);
    assert!(with_below.len() <= term.lines() + 1, "seed {seed}");
    assert_eq!(
        with_below.len() > term.lines(),
        view.offset(term) > 0,
        "seed {seed}: a row under the view is only there scrolled up"
    );
    for (row, same) in with_below.iter().zip(&shown) {
        assert_eq!(row.row.cells, same.row.cells, "seed {seed}");
    }
    for row in &with_below {
        assert_eq!(row.row.len(), term.columns(), "seed {seed}");
    }
    let selected = view.selected_cells(term);
    match view.selected_cells_and_below(term) {
        Some((start, (bottom, right))) => {
            assert!(
                start.0 <= bottom && bottom < with_below.len(),
                "seed {seed}"
            );
            assert!(right < term.columns(), "seed {seed}");
            if let Some((first, _)) = selected {
                assert_eq!(start, first, "seed {seed}");
            }
        }
        None => assert_eq!(selected, None, "seed {seed}"),
    }
    let _ = view.at_bottom(term);
}

fn round(seed: u64) {
    let mut random = Random(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
    let scrollback = random.pick(&[0, 1, 5, 50, 1000]);
    let (columns, lines) = (1 + random.below(40), 1 + random.below(15));
    let mut term = Term::new(
        columns,
        lines,
        Options {
            scrollback,
            version: "caret 1.2.3".to_owned(),
            ..Options::default()
        },
    );
    term.set_cell_size(8, 16);
    let mut views = [View::default(), View::default()];
    for _ in 0..=random.below(30) {
        let bytes = output(&mut random);
        // Sequences and characters cut in two by the reads.
        let cut = random.below(bytes.len() + 1);
        term.advance(&bytes[..cut]);
        term.advance(&bytes[cut..]);
        if random.below(4) == 0 {
            let written = term.take_written();
            if let Some(written) = written {
                for view in &mut views {
                    view.written(written);
                }
            }
        }
        match random.below(10) {
            0 => term.resize(1 + random.below(40), 1 + random.below(15)),
            1 => {
                term.set_options(Options {
                    scrollback: random.pick(&[0, 1, 5, 50, 1000]),
                    ..Options::default()
                });
            }
            2 => term.end_synchronized(),
            _ => {}
        }
        check(&term, seed);
        for view in &mut views {
            client(&mut random, &term, view, seed);
        }
        let _ = term.take_replies();
        let _ = term.take_events();
    }
}

#[test]
fn random_programs_and_clients_break_nothing() {
    let rounds = std::env::var("CARET_STRESS_ROUNDS")
        .ok()
        .and_then(|rounds| rounds.parse().ok())
        .unwrap_or(400);
    for seed in 1..=rounds {
        round(seed);
    }
}

#[test]
fn pure_noise_breaks_nothing() {
    let mut random = Random(0x1234_5678_9abc_def1);
    for columns in [1, 2, 3, 80] {
        let mut term = Term::new(columns, 5, Options::default());
        for _ in 0..200 {
            let bytes: Vec<u8> = (0..4096).map(|_| random.next() as u8).collect();
            term.advance(&bytes);
            check(&term, columns as u64);
            let _ = term.take_replies();
            let _ = term.take_events();
        }
    }
}
