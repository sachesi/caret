//! Finding text in a terminal's lines a step at a time, so that the program's output is
//! read between steps of a search through a long history.

use crate::history::Line;
use crate::term::Term;
use crate::view::Place;

/// A search going round every line from where it started, upwards or down.
#[derive(Debug, Clone)]
pub struct Search {
    needle: Vec<char>,
    /// The text has no capitals, so letters match in either case.
    folded: bool,
    upwards: bool,
    /// The line to look in next.
    next: u64,
    /// In the first line, the cell a match has to start at or after, or before upwards.
    bound: Option<usize>,
    /// Lines left to look in, the first counting twice: once from where the search
    /// started, once more at the end for the rest of it.
    left: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Progress {
    /// The first and last cell of a match.
    Found(Place, Place),
    Missing,
    Going,
}

impl Search {
    /// A search for `text` from `from`: matches starting there or after it, or before it
    /// upwards. None for no text.
    #[must_use]
    pub fn new(term: &Term, text: &str, upwards: bool, from: Place) -> Option<Self> {
        if text.is_empty() {
            return None;
        }
        let folded = !text.chars().any(char::is_uppercase);
        let needle = text.chars().map(|c| fold(c, folded)).collect();
        let serials = term.serials();
        Some(Self {
            needle,
            folded,
            upwards,
            next: from.line.clamp(*serials.start(), *serials.end()),
            bound: Some(from.cell),
            left: serials.end() - serials.start() + 2,
        })
    }

    /// Looks through up to `lines` more lines.
    pub fn step(&mut self, term: &Term, lines: usize) -> Progress {
        let serials = term.serials();
        for _ in 0..lines {
            if self.left == 0 {
                return Progress::Missing;
            }
            self.left -= 1;
            if serials.contains(&self.next)
                && let Some(line) = term.line(self.next)
                && let Some((first, last)) = self.find_in(&line)
            {
                let place = |cell| Place {
                    line: self.next,
                    cell,
                };
                return Progress::Found(place(first), place(last));
            }
            self.bound = None;
            self.next = if self.upwards {
                if self.next <= *serials.start() {
                    *serials.end()
                } else {
                    self.next - 1
                }
            } else if self.next >= *serials.end() {
                *serials.start()
            } else {
                self.next + 1
            };
        }
        Progress::Going
    }

    /// The first and last cell of the match in `line` nearest the start of the search.
    fn find_in(&self, line: &Line) -> Option<(usize, usize)> {
        let mut haystack = Vec::with_capacity(line.width());
        for (cell, (text, _)) in line.cells().iter().enumerate() {
            haystack.extend(text.chars().map(|c| (fold(c, self.folded), cell)));
        }
        let length = self.needle.len();
        let mut matches = haystack
            .windows(length)
            .filter(|window| {
                window
                    .iter()
                    .map(|&(c, _)| c)
                    .eq(self.needle.iter().copied())
            })
            .map(|window| (window[0].1, window[length - 1].1));
        match (self.upwards, self.bound) {
            (false, Some(bound)) => matches.find(|&(first, _)| first >= bound),
            (false, None) => matches.next(),
            (true, Some(bound)) => matches.rfind(|&(first, _)| first < bound),
            (true, None) => matches.next_back(),
        }
    }
}

fn fold(c: char, folded: bool) -> char {
    if folded {
        c.to_lowercase().next().unwrap_or(c)
    } else {
        c
    }
}
