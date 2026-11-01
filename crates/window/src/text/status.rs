//! The bar under the panes: which session and tab this is, and the keys there are.

use super::canvas::{Canvas, Pen};
use caret_protocol::GlyphText;

/// Text written with one pen.
pub type Span = (String, Pen);

/// How many columns `spans` take.
fn width(spans: &[Span]) -> usize {
    spans.iter().map(|(text, _)| text.chars().count()).sum()
}

/// `hints`, each a key and what it does and never split, in as many lines of at most
/// `columns` as they need, the first starting with `lead`.
pub fn wrapped(lead: Vec<Span>, hints: Vec<[Span; 2]>, columns: usize) -> Vec<Vec<Span>> {
    let mut lines = vec![lead];
    for hint in hints {
        let last = lines.last_mut().expect("the lead's line is always there");
        if !last.is_empty() && width(last) + width(&hint) > columns {
            lines.push(Vec::new());
        }
        lines
            .last_mut()
            .expect("the lead's line is always there")
            .extend(hint);
    }
    lines
}

/// Writes `spans` from column `x` of line `y`, as far as the line goes.
pub fn put(canvas: &mut Canvas, mut x: i32, y: i32, spans: &[Span]) {
    for (text, pen) in spans {
        for c in text.chars() {
            canvas.put(x, y, Some(GlyphText::Char(c)), *pen, false);
            x += 1;
        }
    }
}

/// Fills line `y` with `pen`.
pub fn fill(canvas: &mut Canvas, y: i32, pen: Pen) {
    let (columns, _) = canvas.size();
    for x in 0..columns {
        canvas.put(i32::try_from(x).unwrap_or(i32::MAX), y, None, pen, false);
    }
}

/// `title`, a character short of `most` and marked as cut when it is longer.
pub fn shortened(title: &str, most: usize) -> String {
    if title.chars().count() <= most {
        return title.to_owned();
    }
    let mut short: String = title.chars().take(most.saturating_sub(1)).collect();
    short.push('…');
    short
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hint(key: &str, action: &str) -> [Span; 2] {
        [
            (key.to_owned(), Pen::default()),
            (action.to_owned(), Pen::default()),
        ]
    }

    fn text(line: &[Span]) -> String {
        line.iter().map(|(text, _)| text.as_str()).collect()
    }

    #[test]
    fn hints_go_on_to_another_line_rather_than_being_cut() {
        let lead = vec![("mode ".to_owned(), Pen::default())];
        let hints = vec![hint("a ", "one "), hint("b ", "two "), hint("c ", "three ")];
        let lines = wrapped(lead, hints, 12);
        let lines: Vec<String> = lines.iter().map(|line| text(line)).collect();
        assert_eq!(lines, ["mode a one ", "b two ", "c three "]);
    }

    #[test]
    fn a_hint_wider_than_the_screen_still_takes_one_line() {
        let lines = wrapped(Vec::new(), vec![hint("long key ", "long action")], 5);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn long_titles_are_cut_with_a_mark() {
        assert_eq!(shortened("short", 5), "short");
        assert_eq!(shortened("longer", 5), "long…");
    }
}
