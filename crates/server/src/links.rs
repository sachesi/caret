//! Addresses in the text on screen, for Ctrl and a click to open.

use std::ops::{Range, RangeInclusive};

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point};
use alacritty_terminal::term::cell::{Cell, Flags, Hyperlink};
use alacritty_terminal::term::{Term, viewport_to_point};

const SCHEMES: [&str; 8] = [
    "https://",
    "http://",
    "file://",
    "ftp://",
    "sftp://",
    "ssh://",
    "gemini://",
    "mailto:",
];

/// Whether an address a program gave as a link is one Caret opens: of the schemes it
/// finds in text, so that a link cannot hand a file or an arbitrary handler to the desktop.
pub fn openable(uri: &str) -> bool {
    SCHEMES.iter().any(|scheme| {
        uri.get(..scheme.len())
            .is_some_and(|start| start.eq_ignore_ascii_case(scheme))
    })
}

/// An address and the cells it is in.
pub struct Link {
    pub uri: String,
    pub span: Span,
}

pub enum Span {
    /// Every cell a program gave this hyperlink.
    Program(Hyperlink),
    /// From the first cell to the last, through the lines between.
    Written(RangeInclusive<Point>),
}

impl Span {
    pub fn covers(&self, point: Point, cell: &Cell) -> bool {
        match self {
            Span::Program(link) => cell.hyperlink().as_ref() == Some(link),
            Span::Written(cells) => cells.contains(&point),
        }
    }
}

/// Whether an address can go on from the end of one line to the start of the next with
/// this character, where a program broke the line itself rather than let it wrap.
pub fn continues_address(c: char) -> bool {
    c.is_ascii_graphic() && !ends_address(c)
}

/// Characters that end an address even without a space: quotes and brackets around it,
/// and those no address holds unescaped.
fn ends_address(c: char) -> bool {
    c.is_whitespace()
        || c.is_control()
        || matches!(
            c,
            '"' | '\'' | '`' | '<' | '>' | '{' | '}' | '|' | '\\' | '^'
        )
}

/// The address `column` is part of, as a range of `text`'s characters, one per column
/// with `'\0'` where there is none (the second half of a wide character, an empty cell).
pub fn address_at(text: &[char], column: usize) -> Option<Range<usize>> {
    let at = |index: usize| text.get(index).copied().unwrap_or('\0');
    if column >= text.len() || at(column) == '\0' || ends_address(at(column)) {
        return None;
    }
    let mut start = column;
    while start > 0 && at(start - 1) != '\0' && !ends_address(at(start - 1)) {
        start -= 1;
    }
    let mut end = column + 1;
    while end < text.len() && at(end) != '\0' && !ends_address(at(end)) {
        end += 1;
    }

    let word: String = text[start..end].iter().collect();
    let (offset, _) = SCHEMES
        .iter()
        .filter_map(|scheme| word.find(scheme).map(|found| (found, scheme)))
        .filter(|&(found, scheme)| {
            // The scheme has to be followed by something.
            word[found + scheme.len()..].chars().next().is_some()
        })
        .min_by_key(|&(found, _)| found)?;
    // A byte offset in `word` back to a character count.
    let start = start + word[..offset].chars().count();

    // Punctuation after an address belongs to the sentence, as do closing brackets that
    // the address did not open.
    loop {
        match at(end - 1) {
            '.' | ',' | ':' | ';' | '!' | '?' => end -= 1,
            close @ (')' | ']') => {
                let open = if close == ')' { '(' } else { '[' };
                let opened = text[start..end].iter().filter(|&&c| c == open).count();
                let closed = text[start..end].iter().filter(|&&c| c == close).count();
                if closed > opened {
                    end -= 1;
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    (start..end).contains(&column).then_some(start..end)
}

/// Lines an address is followed through, up and down from the point.
const LONGEST_ADDRESS: i32 = 64;

/// The address at a point of the view: the hyperlink a program gave the text there, or
/// one written out in it.
pub fn link_in<T: EventListener>(term: &Term<T>, point: Point<usize>) -> Option<Link> {
    // A cell a client hovered before the grid shrank may be outside it now.
    if point.line >= term.screen_lines() || point.column >= term.columns() {
        return None;
    }
    let point = viewport_to_point(term.grid().display_offset(), point);
    if let Some(link) = term.grid()[point].hyperlink()
        && openable(link.uri())
    {
        return Some(Link {
            uri: link.uri().to_owned(),
            span: Span::Program(link),
        });
    }
    let grid = term.grid();

    let last = term.last_column();
    // A line goes on in the next where the terminal wrapped it, and where a program that
    // breaks lines itself filled it to the last column with an address.
    let goes_on = |line: i32| {
        let end = &grid[Line(line)][last];
        end.flags.contains(Flags::WRAPLINE)
            || (continues_address(end.c) && continues_address(grid[Line(line + 1)][Column(0)].c))
    };
    let top = term.topmost_line().0.max(point.line.0 - LONGEST_ADDRESS);
    let bottom = term.bottommost_line().0.min(point.line.0 + LONGEST_ADDRESS);
    let mut first = point.line.0;
    while first > top && goes_on(first - 1) {
        first -= 1;
    }
    let mut end = point.line.0;
    while end < bottom && goes_on(end) {
        end += 1;
    }
    let columns = term.columns();
    let mut text = Vec::with_capacity(columns * (end - first + 1) as usize);
    for line in first..=end {
        for column in 0..columns {
            let cell = &grid[Line(line)][Column(column)];
            let spacer = cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
            text.push(if spacer { '\0' } else { cell.c });
        }
    }
    let index = (point.line.0 - first) as usize * columns + point.column.0;
    let range = address_at(&text, index)?;
    let cell = |index: usize| {
        Point::new(
            Line(first + (index / columns) as i32),
            Column(index % columns),
        )
    };
    Some(Link {
        span: Span::Written(cell(range.start)..=cell(range.end - 1)),
        uri: text[range].iter().filter(|&&c| c != '\0').collect(),
    })
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term;

    use super::*;
    use crate::terminal::GridSize;

    #[test]
    fn a_cell_outside_a_shrunk_grid_has_no_link() {
        let size = GridSize {
            columns: 80,
            lines: 24,
        };
        let mut term = Term::new(term::Config::default(), &size, VoidListener);
        term.resize(GridSize {
            columns: 40,
            lines: 10,
        });
        assert!(link_in(&term, Point::new(20, Column(70))).is_none());
        assert!(link_in(&term, Point::new(5, Column(70))).is_none());
    }

    #[test]
    fn links_open_only_with_known_schemes() {
        assert!(openable("HTTPS://example.org"));
        assert!(openable("file:///tmp/x"));
        assert!(!openable("steam://run/1"));
        assert!(!openable("javascript:alert(1)"));
        assert!(!openable("é"));
    }

    fn found(line: &str, column: usize) -> Option<String> {
        let text: Vec<char> = line.chars().collect();
        address_at(&text, column).map(|range| text[range].iter().collect())
    }

    #[test]
    fn an_address_is_found_from_any_of_its_characters() {
        let line = "see https://example.org/a?b=c for more";
        assert_eq!(found(line, 4).as_deref(), Some("https://example.org/a?b=c"));
        assert_eq!(
            found(line, 20).as_deref(),
            Some("https://example.org/a?b=c")
        );
        assert_eq!(found(line, 1), None);
        assert_eq!(found(line, 3), None);
    }

    #[test]
    fn surrounding_punctuation_is_left_out() {
        let line = "(read https://example.org/wiki/Rust_(language)).";
        assert_eq!(
            found(line, 10).as_deref(),
            Some("https://example.org/wiki/Rust_(language)")
        );
        assert_eq!(
            found("\"file:///tmp/x\",", 3).as_deref(),
            Some("file:///tmp/x")
        );
    }

    #[test]
    fn text_before_the_scheme_is_not_part_of_it() {
        let line = "url=https://example.org";
        assert_eq!(found(line, 10).as_deref(), Some("https://example.org"));
        assert_eq!(found(line, 1), None);
        assert_eq!(found("https://", 2), None);
    }
}
