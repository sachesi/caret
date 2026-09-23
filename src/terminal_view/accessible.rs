//! The screen as text for screen readers: what is shown, one line per row, the caret at the
//! cursor, and the selection.

use std::ops::Range;

use alacritty_terminal::index::Point;

use super::*;
use crate::fonts::GlyphText;
use crate::frame::ScreenSelection;

/// A character of the text and the cells it stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Place {
    line: usize,
    column: usize,
    /// Cells wide: one, two for a wide character, none for the end of a line.
    width: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ScreenText {
    chars: Vec<char>,
    places: Vec<Place>,
    caret: usize,
    selection: Option<Range<usize>>,
}

impl ScreenText {
    pub fn new(screen: &Screen) -> Self {
        let mut chars = Vec::new();
        let mut places = Vec::new();
        let mut cells = screen.cells.iter().peekable();
        for line in 0..screen.lines {
            let mut column = 0;
            while let Some(cell) = cells.next_if(|cell| cell.line == line) {
                let Some(glyph) = &cell.glyph else { continue };
                // Cells with nothing to draw are not kept; they read as spaces.
                while column < cell.column {
                    chars.push(' ');
                    places.push(Place {
                        line,
                        column,
                        width: 1,
                    });
                    column += 1;
                }
                let width = if glyph.wide { 2 } else { 1 };
                let place = Place {
                    line,
                    column: cell.column,
                    width,
                };
                match &glyph.text {
                    GlyphText::Char(c) => {
                        chars.push(*c);
                        places.push(place);
                    }
                    GlyphText::Cluster(text) => {
                        for c in text.chars() {
                            chars.push(c);
                            places.push(place);
                        }
                    }
                }
                column = cell.column + width;
            }
            if line + 1 < screen.lines {
                chars.push('\n');
                places.push(Place {
                    line,
                    column,
                    width: 0,
                });
            }
        }

        let mut text = Self {
            chars,
            places,
            caret: 0,
            selection: None,
        };
        text.caret = screen
            .cursor
            .map_or(0, |cursor| text.offset_of(cursor.line, cursor.column));
        text.selection = screen.selection.map(|selection| text.range_of(selection));
        text
    }

    /// The offset of the character at a cell; right of a row's text, of the row's end.
    fn offset_of(&self, line: usize, column: usize) -> usize {
        self.places
            .iter()
            .position(|place| {
                place.line > line
                    || (place.line == line
                        && (place.width == 0 || column < place.column + place.width))
            })
            .unwrap_or(self.chars.len())
    }

    fn range_of(&self, selection: ScreenSelection) -> Range<usize> {
        let start = self.offset_of(selection.start.line, selection.start.column.0);
        let end = self.offset_of(selection.end.line, selection.end.column.0 + 1);
        start..end.max(start)
    }

    fn clamp(&self, offset: u32) -> usize {
        usize::try_from(offset).map_or(self.chars.len(), |offset| offset.min(self.chars.len()))
    }

    fn bytes(&self, range: Range<usize>) -> glib::Bytes {
        let text: String = self.chars[range].iter().collect();
        glib::Bytes::from_owned(text.into_bytes())
    }

    /// The range of the unit of `granularity` around `offset`.
    fn unit_at(&self, offset: usize, granularity: gtk::AccessibleTextGranularity) -> Range<usize> {
        let len = self.chars.len();
        if len == 0 {
            return 0..0;
        }
        let offset = offset.min(len - 1);
        let bounded = |is_boundary: &dyn Fn(char) -> bool| {
            let start = self.chars[..offset]
                .iter()
                .rposition(|&c| is_boundary(c))
                .map_or(0, |index| index + 1);
            let end = self.chars[offset..]
                .iter()
                .position(|&c| is_boundary(c))
                .map_or(len, |index| offset + index + 1);
            start..end
        };
        match granularity {
            gtk::AccessibleTextGranularity::Character => offset..offset + 1,
            gtk::AccessibleTextGranularity::Word => {
                let mut range = bounded(&|c: char| c.is_whitespace());
                // A word does not carry the space after it into what is read.
                if range.end > range.start && self.chars[range.end - 1].is_whitespace() {
                    range.end -= 1;
                }
                range
            }
            // Lines, sentences and paragraphs are all rows of the screen.
            _ => bounded(&|c: char| c == '\n'),
        }
    }

    /// The first differing character of two texts, and where the differences end in each.
    fn difference(&self, other: &Self) -> Option<(usize, usize, usize)> {
        if self.chars == other.chars {
            return None;
        }
        let prefix = self
            .chars
            .iter()
            .zip(&other.chars)
            .take_while(|(a, b)| a == b)
            .count();
        let room = self.chars.len().min(other.chars.len()) - prefix;
        let suffix = self
            .chars
            .iter()
            .rev()
            .zip(other.chars.iter().rev())
            .take(room)
            .take_while(|(a, b)| a == b)
            .count();
        Some((
            prefix,
            self.chars.len() - suffix,
            other.chars.len() - suffix,
        ))
    }
}

impl TerminalView {
    /// Tells assistive technologies what changed on the screen since the last frame.
    pub(super) fn update_accessible_text(&self, screen: &Screen) {
        let imp = self.imp();
        let text = ScreenText::new(screen);
        if *imp.text.borrow() == text {
            return;
        }
        let difference = imp.text.borrow().difference(&text);
        let (caret, selection) = {
            let old = imp.text.borrow();
            (old.caret != text.caret, old.selection != text.selection)
        };
        // A removal is announced while the removed text can still be read.
        if let Some((start, removed, _)) = difference
            && removed > start
        {
            self.update_contents(
                gtk::AccessibleTextContentChange::Remove,
                start as u32,
                removed as u32,
            );
        }
        imp.text.replace(text);
        if let Some((start, _, inserted)) = difference
            && inserted > start
        {
            self.update_contents(
                gtk::AccessibleTextContentChange::Insert,
                start as u32,
                inserted as u32,
            );
        }
        if caret {
            self.update_caret_position();
        }
        if selection {
            self.update_selection_bound();
        }
    }

    /// The rectangle around the cells of `places` in the widget.
    fn cells_rect(&self, places: &[Place]) -> Option<graphene::Rect> {
        let cell = self.imp().fonts.borrow().as_ref()?.cell;
        let first = places.first()?;
        let last = places.last()?;
        let scale = self.scale();
        let padding = f64::from(padding(scale));
        let x = |column: usize| (padding + column as f64 * f64::from(cell.width)) / scale;
        let y = |line: usize| (padding + line as f64 * f64::from(cell.height)) / scale;
        let left = places.iter().map(|place| place.column).min()?;
        let right = places
            .iter()
            .map(|place| place.column + place.width)
            .max()?;
        Some(graphene::Rect::new(
            x(left) as f32,
            y(first.line) as f32,
            (x(right) - x(left)) as f32,
            (y(last.line + 1) - y(first.line)) as f32,
        ))
    }
}

impl gtk::subclass::accessible_text::AccessibleTextImpl for imp::TerminalView {
    fn contents(&self, start: u32, end: u32) -> Option<glib::Bytes> {
        let text = self.text.borrow();
        let (start, end) = (text.clamp(start), text.clamp(end));
        Some(text.bytes(start..end.max(start)))
    }

    fn contents_at(
        &self,
        offset: u32,
        granularity: gtk::AccessibleTextGranularity,
    ) -> Option<(u32, u32, glib::Bytes)> {
        let text = self.text.borrow();
        let range = text.unit_at(text.clamp(offset), granularity);
        let bytes = text.bytes(range.clone());
        Some((range.start as u32, range.end as u32, bytes))
    }

    fn caret_position(&self) -> u32 {
        self.text.borrow().caret as u32
    }

    fn selection(&self) -> Vec<gtk::AccessibleTextRange> {
        self.text
            .borrow()
            .selection
            .iter()
            .map(|range| gtk::AccessibleTextRange::new(range.start, range.len()))
            .collect()
    }

    fn attributes(
        &self,
        _offset: u32,
    ) -> Vec<(gtk::AccessibleTextRange, glib::GString, glib::GString)> {
        Vec::new()
    }

    fn default_attributes(&self) -> Vec<(glib::GString, glib::GString)> {
        Vec::new()
    }

    fn extents(&self, start: u32, end: u32) -> Option<graphene::Rect> {
        let text = self.text.borrow();
        let (start, end) = (text.clamp(start), text.clamp(end));
        let places = &text.places[start..end.max(start + 1).min(text.places.len())];
        self.obj().cells_rect(places)
    }

    fn offset(&self, point: &graphene::Point) -> Option<u32> {
        let (Point { line, column }, _) = self
            .obj()
            .cell_at(f64::from(point.x()), f64::from(point.y()))?;
        let offset = self.text.borrow().offset_of(line, column.0);
        u32::try_from(offset).ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fonts::GlyphKey;
    use crate::frame::{ScreenCell, ScreenCursor};
    use alacritty_terminal::index::Column;
    use alacritty_terminal::term::cell::Flags;

    fn screen(lines: &[&str], cursor: (usize, usize)) -> Screen {
        let mut screen = Screen {
            lines: lines.len(),
            columns: 20,
            ..Screen::default()
        };
        for (line, text) in lines.iter().enumerate() {
            let mut column = 0;
            for c in text.chars() {
                let wide = c == '日';
                if c != ' ' {
                    screen.cells.push(ScreenCell {
                        line,
                        column,
                        glyph: Some(GlyphKey {
                            text: GlyphText::Char(c),
                            style: 0,
                            wide,
                        }),
                        fg: Rgb::default(),
                        bg: Rgb::default(),
                        underline: Rgb::default(),
                        flags: Flags::empty(),
                    });
                }
                column += if wide { 2 } else { 1 };
            }
        }
        screen.cursor = Some(ScreenCursor {
            line: cursor.0,
            column: cursor.1,
            shape: CursorShape::Block,
            wide: false,
        });
        screen
    }

    fn string(text: &ScreenText) -> String {
        text.chars.iter().collect()
    }

    #[test]
    fn rows_become_lines_with_their_inner_spaces() {
        let text = ScreenText::new(&screen(&["$ ls  -l", "", "日本 x"], (2, 7)));
        assert_eq!(string(&text), "$ ls  -l\n\n日本 x");
        assert_eq!(text.caret, text.chars.len());
        let text = ScreenText::new(&screen(&["$ ls", "x"], (0, 9)));
        assert_eq!(text.chars[text.caret], '\n');
        let text = ScreenText::new(&screen(&["$ ls  -l", "", "日本 x"], (2, 2)));
        assert_eq!(text.chars[text.caret], '本');
    }

    #[test]
    fn selections_cover_the_characters_of_their_cells() {
        let mut screen = screen(&["one two", "three"], (1, 5));
        screen.selection = Some(ScreenSelection {
            start: Point::new(0, Column(4)),
            end: Point::new(1, Column(2)),
            block: false,
        });
        let text = ScreenText::new(&screen);
        let range = text.selection.clone().unwrap();
        assert_eq!(text.chars[range].iter().collect::<String>(), "two\nthr");
    }

    #[test]
    fn units_are_words_and_rows() {
        let text = ScreenText::new(&screen(&["cargo build --release", "ok"], (1, 2)));
        let word = text.unit_at(8, gtk::AccessibleTextGranularity::Word);
        assert_eq!(text.chars[word].iter().collect::<String>(), "build");
        let line = text.unit_at(3, gtk::AccessibleTextGranularity::Line);
        assert_eq!(
            text.chars[line].iter().collect::<String>(),
            "cargo build --release\n"
        );
    }

    #[test]
    fn a_change_is_the_part_between_what_stayed() {
        let before = ScreenText::new(&screen(&["$ ls", ""], (0, 4)));
        let after = ScreenText::new(&screen(&["$ ls", "a b"], (1, 0)));
        assert_eq!(before.difference(&after), Some((5, 5, 8)));
        assert_eq!(before.difference(&before), None);
    }
}
