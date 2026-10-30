//! The same output through this emulator and the one the server used before it, screens
//! compared cell by cell. Where they differ on purpose, the input avoids it.

use std::fmt::Write;

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::cell::Flags as TheirFlags;
use alacritty_terminal::term::test::TermSize;
use alacritty_terminal::vte::ansi::{Color as TheirColor, Processor};
use caret_term::grid::Text;
use caret_term::style::{Color, Flags, Underline};
use caret_term::{Options, Term};

type Theirs = alacritty_terminal::Term<VoidListener>;

/// A cell as both describe it: text, width, colours and attributes.
#[derive(Debug, PartialEq, Eq)]
struct Seen {
    text: String,
    fg: Color,
    bg: Color,
    attributes: Vec<&'static str>,
}

fn theirs(columns: usize, lines: usize, input: &[u8]) -> Theirs {
    let mut term = Theirs::new(
        alacritty_terminal::term::Config::default(),
        &TermSize::new(columns, lines),
        VoidListener,
    );
    let mut parser: Processor = Processor::new();
    parser.advance(&mut term, input);
    term
}

fn ours(columns: usize, lines: usize, input: &[u8]) -> Term {
    let mut term = Term::new(columns, lines, Options::default());
    term.advance(input);
    term
}

fn their_color(color: TheirColor) -> Color {
    match color {
        TheirColor::Named(named) if (named as usize) < 16 => Color::Indexed(named as u8),
        TheirColor::Named(_) => Color::Default,
        TheirColor::Indexed(index) => Color::Indexed(index),
        TheirColor::Spec(rgb) => Color::Rgb(rgb.r, rgb.g, rgb.b),
    }
}

/// Half of a wide character whose other half was erased, which one keeps showing and
/// the other blanks.
fn orphan(
    row: &alacritty_terminal::grid::Row<alacritty_terminal::term::cell::Cell>,
    column: usize,
) -> bool {
    let flags = |column: usize| row[Column(column)].flags;
    let wide = TheirFlags::WIDE_CHAR;
    let spacer = TheirFlags::WIDE_CHAR_SPACER;
    (flags(column).contains(wide)
        && (column + 1 >= row.len() || !flags(column + 1).contains(spacer)))
        || (flags(column).contains(spacer) && (column == 0 || !flags(column - 1).contains(wide)))
}

fn their_cells(term: &Theirs) -> Vec<Vec<Seen>> {
    let grid = term.grid();
    (0..term.screen_lines())
        .map(|line| {
            (0..term.columns())
                .map(|column| {
                    let cell = &grid[Line(line as i32)][Column(column)];
                    let flags = cell.flags;
                    let text = if orphan(&grid[Line(line as i32)], column) {
                        " ".to_owned()
                    } else if flags.contains(TheirFlags::WIDE_CHAR_SPACER) {
                        "<spacer>".to_owned()
                    } else if flags.contains(TheirFlags::LEADING_WIDE_CHAR_SPACER) {
                        // One leaves the blank before a wide character marked after the
                        // character is overwritten, the other does not; both show a blank.
                        " ".to_owned()
                    } else if cell.c == '\t' {
                        // Kept where a tab moved over blanks, for copying.
                        " ".to_owned()
                    } else {
                        std::iter::once(cell.c)
                            .chain(cell.zerowidth().unwrap_or_default().iter().copied())
                            .collect()
                    };
                    let mut attributes = Vec::new();
                    for (flag, name) in [
                        (TheirFlags::BOLD, "bold"),
                        (TheirFlags::DIM, "dim"),
                        (TheirFlags::ITALIC, "italic"),
                        (TheirFlags::INVERSE, "inverse"),
                        (TheirFlags::HIDDEN, "hidden"),
                        (TheirFlags::STRIKEOUT, "strikeout"),
                        (TheirFlags::WIDE_CHAR, "wide"),
                        (TheirFlags::UNDERLINE, "underline"),
                        (TheirFlags::DOUBLE_UNDERLINE, "double"),
                        (TheirFlags::UNDERCURL, "curly"),
                        (TheirFlags::DOTTED_UNDERLINE, "dotted"),
                        (TheirFlags::DASHED_UNDERLINE, "dashed"),
                    ] {
                        if flags.contains(flag) && !(name == "wide" && text == " ") {
                            attributes.push(name);
                        }
                    }
                    if orphan(&grid[Line(line as i32)], column) {
                        return Seen {
                            text,
                            fg: their_color(cell.fg),
                            bg: their_color(cell.bg),
                            attributes,
                        };
                    }
                    Seen {
                        text,
                        fg: their_color(cell.fg),
                        bg: their_color(cell.bg),
                        attributes,
                    }
                })
                .collect()
        })
        .collect()
}

fn our_cells(term: &Term) -> Vec<Vec<Seen>> {
    term.screen()
        .rows()
        .iter()
        .map(|row| {
            (0..row.len())
                .map(|column| {
                    let style = row.cells[column].style;
                    let flags = style.flags;
                    let text = if flags.contains(Flags::SPACER) {
                        let after_wide =
                            column > 0 && row.cells[column - 1].style.flags.contains(Flags::WIDE);
                        if after_wide { "<spacer>" } else { " " }.to_owned()
                    } else {
                        match row.text(column) {
                            Text::Char(c) => c.to_string(),
                            Text::Cluster(text) => text.to_owned(),
                        }
                    };
                    let mut attributes = Vec::new();
                    for (flag, name) in [
                        (Flags::BOLD, "bold"),
                        (Flags::DIM, "dim"),
                        (Flags::ITALIC, "italic"),
                        (Flags::INVERSE, "inverse"),
                        (Flags::HIDDEN, "hidden"),
                        (Flags::STRIKEOUT, "strikeout"),
                        (Flags::WIDE, "wide"),
                    ] {
                        if flags.contains(flag) {
                            attributes.push(name);
                        }
                    }
                    match flags.underline() {
                        Underline::None => {}
                        Underline::Single => attributes.push("underline"),
                        Underline::Double => attributes.push("double"),
                        Underline::Curly => attributes.push("curly"),
                        Underline::Dotted => attributes.push("dotted"),
                        Underline::Dashed => attributes.push("dashed"),
                    }
                    Seen {
                        text,
                        fg: style.fg,
                        bg: style.bg,
                        attributes,
                    }
                })
                .collect()
        })
        .collect()
}

/// The history's lines as text, oldest first.
fn their_history(term: &Theirs) -> Vec<String> {
    let grid = term.grid();
    let mut lines = Vec::new();
    let mut current = String::new();
    for line in (1..=term.history_size()).rev() {
        let row = &grid[Line(-(line as i32))];
        for column in 0..term.columns() {
            let cell = &row[Column(column)];
            if !orphan(row, column)
                && !cell
                    .flags
                    .intersects(TheirFlags::WIDE_CHAR_SPACER | TheirFlags::LEADING_WIDE_CHAR_SPACER)
            {
                current.push(if cell.c == '\t' { ' ' } else { cell.c });
                current.extend(cell.zerowidth().unwrap_or_default());
            }
        }
        if !row[Column(term.columns() - 1)]
            .flags
            .contains(TheirFlags::WRAPLINE)
        {
            lines.push(current.trim_end().to_owned());
            current.clear();
        }
    }
    if !current.is_empty() {
        lines.push(current.trim_end().to_owned());
    }
    lines
}

fn our_history(term: &Term) -> Vec<String> {
    term.history()
        .lines()
        .map(|line| {
            line.cells()
                .iter()
                .map(|(text, _)| *text)
                .collect::<String>()
                .trim_end()
                .to_owned()
        })
        .collect()
}

/// Where they first differ, if they do.
fn difference(columns: usize, lines: usize, input: &[u8]) -> Option<String> {
    let theirs = theirs(columns, lines, input);
    let ours = ours(columns, lines, input);
    // Blanks one inserts take the whole style, the other's only the background.
    let blanks = |mut rows: Vec<Vec<Seen>>| {
        for cell in rows.iter_mut().flatten() {
            if cell.text == " " && !cell.attributes.contains(&"inverse") {
                cell.fg = Color::Default;
                cell.attributes.clear();
            }
        }
        rows
    };
    let (their_cells, our_cells) = (blanks(their_cells(&theirs)), blanks(our_cells(&ours)));
    for (line, (their_row, our_row)) in their_cells.iter().zip(&our_cells).enumerate() {
        for (column, (their_cell, our_cell)) in their_row.iter().zip(our_row).enumerate() {
            if their_cell != our_cell {
                return Some(format!(
                    "cell {line},{column}: theirs {their_cell:?}, ours {our_cell:?}"
                ));
            }
        }
    }
    let point = theirs.grid().cursor.point;
    let their_cursor = (point.line.0 as usize, point.column.0);
    let our_cursor = ours.cursor().unwrap_or_default();
    if their_cursor != our_cursor {
        return Some(format!(
            "cursor: theirs {their_cursor:?}, ours {our_cursor:?}"
        ));
    }
    // Clearing a blank screen leaves a blank line in one's history, overwriting a wide
    // character leaves a blank in one where it did not fit at the end of a row, and
    // writing to the last column of a wrapped row ends its line in one. The text is the
    // same.
    let text = |lines: Vec<String>| -> String { lines.concat().replace(' ', "") };
    let (their_history, our_history) = (text(their_history(&theirs)), text(our_history(&ours)));
    // The other keeps no history while the alternate screen is shown.
    if !ours.alt_screen() && their_history != our_history {
        return Some(format!(
            "history: theirs {their_history:?}, ours {our_history:?}"
        ));
    }
    None
}

fn check(columns: usize, lines: usize, input: &str) {
    if let Some(difference) = difference(columns, lines, input.as_bytes()) {
        panic!("{input:?} at {columns}x{lines}: {difference}");
    }
}

#[test]
fn text_wraps_scrolls_and_keeps_its_attributes() {
    check(
        10,
        4,
        "hello\r\nworld\x1b[1;31m red bold \x1b[0mand more text that wraps\r\n\r\n",
    );
    check(5, 3, "日本語の文字\r\nab日\r\na\u{301}e\u{302}x");
    check(
        8,
        3,
        "\x1b[4:3;38;2;1;2;3;48;5;200mcurly\x1b[24;7minv\x1b[27;9mstruck",
    );
}

#[test]
fn erasing_inserting_and_deleting() {
    check(
        10,
        4,
        "0123456789abcdefghij\x1b[1;5H\x1b[K\x1b[2;3H\x1b[1K\x1b[3;1Hxyz\x1b[3;2H\x1b[2@\x1b[P\x1b[3X",
    );
    check(
        6,
        5,
        "1\r\n2\r\n3\r\n4\r\n5\x1b[2;1H\x1b[2L\x1b[4;1H\x1b[M\x1b[1;1H\x1b[J",
    );
    check(6, 5, "aaaaaa\r\nbbbbbb\r\ncc\x1b[2J\x1b[Hd\x1b[0J");
}

#[test]
fn regions_tabs_and_line_drawing() {
    check(
        6,
        5,
        "top\r\n1\r\n2\r\n3\r\nbot\x1b[2;4r\x1b[4;1H\n\nx\x1b[r\x1b[1;1H\x1bM",
    );
    check(20, 2, "a\tb\x1bH\tc\x1b[3g\r\td\x1b[2Ie\tf");
    check(8, 2, "\x1b(0lqqk\x1b(Bq\x0ex\x0fy");
}

#[test]
fn the_alternate_screen_and_saved_cursors() {
    check(
        8,
        3,
        "main\x1b7\x1b[?1049h\x1b[2;2Halt\x1b[?1049lback\x1b8!",
    );
    check(8, 3, "ab\x1b[s\x1b[3;3Hcd\x1b[uef\x1b[?6h\x1b[2;3r\x1b[Hg");
}

/// A small generator, the same on every run.
struct Random(u64);

impl Random {
    fn below(&mut self, bound: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 % bound as u64) as usize
    }

    fn pick<'a>(&mut self, items: &[&'a str]) -> &'a str {
        items[self.below(items.len())]
    }
}

/// Output a program might write: text and the sequences both emulators handle alike.
fn output(random: &mut Random, lines: usize) -> Vec<String> {
    let mut text = Vec::new();
    for _ in 0..random.below(60) {
        let n = random.below(12);
        let m = random.below(12);
        let piece = match random.below(16) {
            0..=3 => random
                .pick(&[
                    "hello",
                    "wörld",
                    "日本",
                    "x",
                    " ",
                    "e\u{301}",
                    "abcdefghijklmnop",
                    "│",
                ])
                .to_owned(),
            4 => random.pick(&["\r", "\n", "\r\n", "\x08", "\t"]).to_owned(),
            5 => format!("\x1b[{n};{m}{}", random.pick(&["H", "f"])),
            // A region past the bottom is taken by one and ignored by the other.
            15 if lines > 1 => format!("\x1b[{};{}r", n % lines, 2 + m % (lines - 1)),
            6 => format!(
                "\x1b[{n}{}",
                random.pick(&["C", "D", "G", "d", "S", "T", "I"])
            ),
            // Erasing above the cursor leaves the top line in one when on the second, and
            // erasing the first half of a wide character leaves the second in one.
            7 => random
                .pick(&["\x1b[J", "\x1b[2J", "\x1b[K", "\x1b[2K"])
                .to_owned(),
            8 => format!(
                "\x1b[{}m",
                random.pick(&[
                    "0",
                    "1",
                    "2",
                    "3",
                    "4",
                    "7",
                    "8",
                    "9",
                    "22",
                    "23",
                    "24",
                    "27",
                    "31",
                    "42",
                    "93",
                    "38;5;100",
                    "48;2;1;2;3",
                    "4:3",
                    "4:2",
                    "39;49",
                ])
            ),
            9 => random
                .pick(&[
                    "\x1b7", "\x1b8", "\x1bD", "\x1bM", "\x1bE", "\x1bH", "\x1b[g",
                ])
                .to_owned(),
            // Inserting blanks or characters before wide characters, deleting more
            // characters than follow the
            // cursor, leaving origin mode and writing without wrapping differ too, as do
            // moving up and down past a margin, tabbing back with no tab stop and erasing
            // half a wide character, or repeating a combining mark.
            10 => random.pick(&["\x1b[?2004h", "\x1b[?1000h"]).to_owned(),
            11 => random.pick(&["\x1b[?1049h", "\x1b[?1049l"]).to_owned(),
            12 => random.pick(&["\x1b[s", "\x1b[u", "\x1b[r"]).to_owned(),
            _ => "x".repeat(n * 3),
        };
        text.push(piece);
    }
    text
}

/// What differs, compared without panicking.
fn compared(columns: usize, lines: usize, input: &str) -> Option<String> {
    std::panic::catch_unwind(|| difference(columns, lines, input.as_bytes()))
        .unwrap_or_else(|_| Some(": panicked".to_owned()))
}

/// The fewest pieces of `pieces` that still show a difference of the same kind.
fn smallest(columns: usize, lines: usize, mut pieces: Vec<String>, kind: &str) -> Vec<String> {
    let mut index = 0;
    while index < pieces.len() {
        let mut fewer = pieces.clone();
        fewer.remove(index);
        if compared(columns, lines, &fewer.concat()).is_some_and(|found| found.contains(kind)) {
            pieces = fewer;
        } else {
            index += 1;
        }
    }
    pieces
}

#[test]
fn random_output_shows_the_same() {
    let mut random = Random(0x5eed_cafe_f00d);
    let mut failures = Vec::new();
    for _ in 0..3000 {
        let columns = 2 + random.below(14);
        let lines = 1 + random.below(6);
        let pieces = output(&mut random, lines);
        if let Some(found) = compared(columns, lines, &pieces.concat()) {
            let kind = found
                .split([' ', ':'])
                .find(|word| !word.is_empty())
                .unwrap_or_default();
            let input = smallest(columns, lines, pieces, kind).concat();
            let difference = compared(columns, lines, &input).unwrap_or_default();
            failures.push(format!("{input:?} at {columns}x{lines}: {difference}"));
        }
    }
    let mut report = String::new();
    for kind in ["cell", "cursor", "history", "panicked"] {
        let of_kind: Vec<_> = failures
            .iter()
            .filter(|failure| failure.contains(&format!(": {kind}")))
            .collect();
        if let Some(shortest) = of_kind.iter().min_by_key(|failure| failure.len()) {
            let _ = writeln!(report, "{} {kind}: {shortest}", of_kind.len());
        }
    }
    assert!(failures.is_empty(), "{} differ:\n{report}", failures.len());
}

#[test]
#[ignore = "prints both screens for the input in AGREE_INPUT"]
fn show() {
    let input = std::env::var("AGREE_INPUT").unwrap_or_default();
    let input = input
        .replace("\\e", "\x1b")
        .replace("\\r", "\r")
        .replace("\\n", "\n")
        .replace("\\t", "\t");
    let size: Vec<usize> = std::env::var("AGREE_SIZE")
        .unwrap_or_default()
        .split('x')
        .filter_map(|n| n.parse().ok())
        .collect();
    let (columns, lines) = (size[0], size[1]);
    let theirs = theirs(columns, lines, input.as_bytes());
    let ours = ours(columns, lines, input.as_bytes());
    let line = |row: &Vec<Seen>| {
        row.iter()
            .map(|seen| match seen.text.as_str() {
                "<spacer>" => "_",
                "<pad>" => "|",
                text => text,
            })
            .collect::<String>()
    };
    for (their_row, our_row) in their_cells(&theirs).iter().zip(&our_cells(&ours)) {
        eprintln!("[{}]  [{}]", line(their_row), line(our_row));
    }
    eprintln!(
        "history {:?} {:?}",
        their_history(&theirs),
        our_history(&ours)
    );
}
