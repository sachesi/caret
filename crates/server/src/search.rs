//! Finding text in a terminal's history, on a thread of its own, a few thousand lines at
//! a time.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::Term;
use alacritty_terminal::term::search::RegexSearch;

/// Lines a search goes through while it holds the terminal.
const SEARCH_STEP: usize = 5000;

/// `text` as a regular expression that matches it literally.
pub fn literal(text: &str) -> String {
    let mut pattern = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern
}

/// Selects the next match of `regex` after the selection, or from the edge of the view,
/// going round the whole history, and scrolls to it. Whether there is one, or None when
/// `cancelled` says so between steps.
///
/// The lease is held throughout, so that no output moves the lines between steps, and the
/// terminal only for a step at a time; the server's loop takes it without the lease.
pub fn search_history<T: EventListener>(
    term: &FairMutex<Term<T>>,
    mut regex: RegexSearch,
    upwards: bool,
    cancelled: impl Fn() -> bool,
) -> Option<bool> {
    let _lease = term.lease();
    'search: loop {
        let (mut from, total, size) = {
            let term = term.lock_unfair();
            (
                search_origin(&term, upwards),
                term.total_lines(),
                (term.columns(), term.screen_lines()),
            )
        };
        let mut searched = 0;
        // One line more than the history, for the part of the first line before the origin.
        while searched <= total {
            let mut term = term.lock_unfair();
            if cancelled() {
                return None;
            }
            // A resize reflows the lines under the step.
            if (term.columns(), term.screen_lines()) != size {
                continue 'search;
            }
            let (found, to) = if upwards {
                let line = (from.line - SEARCH_STEP as i32).max(term.topmost_line());
                let to = term.line_search_left(Point::new(line, Column(0)));
                (term.regex_search_left(&mut regex, from, to), to)
            } else {
                let line = (from.line + SEARCH_STEP as i32).min(term.bottommost_line());
                let to = term.line_search_right(Point::new(line, term.last_column()));
                (term.regex_search_right(&mut regex, from, to), to)
            };
            if let Some(found) = found {
                let mut selection =
                    Selection::new(SelectionType::Simple, *found.start(), Side::Left);
                selection.update(*found.end(), Side::Right);
                term.selection = Some(selection);
                term.scroll_to_point(*found.start());
                return Some(true);
            }
            searched += from.line.0.abs_diff(to.line.0) as usize + 1;
            // Past the end of the history, the next step starts at the other end.
            from = if upwards {
                to.sub(&*term, Boundary::None, 1)
            } else {
                to.add(&*term, Boundary::None, 1)
            };
        }
        return Some(false);
    }
}

/// Where a search starts: next to the selection, or at the edge of the view it goes away
/// from.
fn search_origin<T>(term: &Term<T>, upwards: bool) -> Point {
    let offset = term.grid().display_offset() as i32;
    let current = term
        .selection
        .as_ref()
        .and_then(|selection| selection.to_range(term));
    let (origin, direction) = match current {
        Some(range) if upwards => (range.start.sub(term, Boundary::None, 1), Direction::Left),
        Some(range) => (range.end.add(term, Boundary::None, 1), Direction::Right),
        None if upwards => (
            Point::new(
                Line(term.screen_lines() as i32 - 1 - offset),
                term.last_column(),
            ),
            Direction::Left,
        ),
        None => (Point::new(Line(-offset), Column(0)), Direction::Right),
    };
    term.expand_wide(origin, direction)
}

#[cfg(test)]
mod tests {
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::term;
    use alacritty_terminal::vte::ansi::Processor;

    use super::*;

    #[test]
    fn searches_go_round_the_history_in_steps() {
        let config = term::Config {
            scrolling_history: 3 * SEARCH_STEP,
            ..term::Config::default()
        };
        let size = crate::terminal::GridSize {
            columns: 20,
            lines: 5,
        };
        let term = FairMutex::new(Term::new(config, &size, VoidListener));
        let mut parser: Processor = Processor::new();
        for line in 0..2 * SEARCH_STEP {
            let text = match line {
                100 => "a needle here".to_owned(),
                line if line == SEARCH_STEP + 100 => "needle".to_owned(),
                line => format!("hay {line}"),
            };
            parser.advance(&mut *term.lock(), format!("{text}\r\n").as_bytes());
        }
        let find = |text: &str, upwards: bool| {
            search_history(&term, RegexSearch::new(text).unwrap(), upwards, || false)
        };
        let selected = || {
            let term = term.lock();
            let range = term.selection.as_ref()?.to_range(&*term)?;
            Some((
                range.start.line.0 + term.history_size() as i32,
                range.start.column.0,
            ))
        };
        let needle = |line: usize, column| Some((line as i32, column));

        assert_eq!(find("needle", true), Some(true));
        assert_eq!(selected(), needle(SEARCH_STEP + 100, 0));
        assert_eq!(find("needle", true), Some(true));
        assert_eq!(selected(), needle(100, 2));
        assert_eq!(find("needle", true), Some(true));
        assert_eq!(selected(), needle(SEARCH_STEP + 100, 0));
        assert_eq!(find("needle", false), Some(true));
        assert_eq!(selected(), needle(100, 2));

        assert_eq!(find("thread", true), Some(false));
        let cancelled = search_history(&term, RegexSearch::new("hay").unwrap(), true, || true);
        assert_eq!(cancelled, None);
    }
}
