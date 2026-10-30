//! What one client sees of a terminal: how far into the history it scrolled, what it
//! selected, and the rows that leaves on its screen. Each client has its own, so that one
//! can read the history while another follows the output.

use std::borrow::Cow;

use crate::grid::Row;
use crate::history::Line;
use crate::links::{address_at, continues_address, openable};
use crate::style::{LinkId, Style};
use crate::term::Term;

/// A cell of the text: the serial number of its line, and its place in the line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Place {
    pub line: u64,
    pub cell: usize,
}

/// A row a view shows, and the place of its first cell.
#[derive(Debug)]
pub struct Shown<'a> {
    pub row: Cow<'a, Row>,
    pub place: Place,
}

/// What a selection takes in around the cells it was dragged over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unit {
    Cell,
    Word,
    Line,
}

#[derive(Debug, Clone, Copy)]
struct Selection {
    unit: Unit,
    /// Where it started and where it was dragged to, each with whether it is on the
    /// right half of its cell.
    anchor: (Place, bool),
    end: (Place, bool),
}

/// An address and the cells it is in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    pub uri: String,
    pub span: Span,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Span {
    /// Every cell a program gave this link.
    Program(LinkId),
    /// The cells from the first to the last, where the address is written out.
    Written(Place, Place),
}

impl Span {
    /// Whether the cell at `place`, which has link `link`, is part of it.
    #[must_use]
    pub fn covers(&self, place: Place, link: LinkId) -> bool {
        match *self {
            Self::Program(id) => id == link,
            Self::Written(first, last) => (first..=last).contains(&place),
        }
    }
}

/// Lines an address is followed through, up and down from the cell.
const LONGEST_ADDRESS: usize = 64;

/// Characters that end a word, besides spaces.
const WORD_ENDS: &str = ",│`|:\"'()[]{}<>";

/// Where a row a view shows comes from.
#[derive(Clone, Copy)]
enum Source {
    /// A history line by index, and a row of it wrapped.
    History(usize, usize),
    Screen(usize),
}

#[derive(Debug, Clone, Default)]
pub struct View {
    /// The history line the top row is in, by serial number, and the row of that line;
    /// None while the view follows the output.
    top: Option<(u64, usize)>,
    selection: Option<Selection>,
}

impl View {
    /// The history line the top row is in, by index, and the row of it; None at the
    /// bottom. A line gone from the history leaves the top at the oldest there is.
    fn anchor(&self, term: &Term) -> Option<(usize, usize)> {
        let (serial, row) = self.top?;
        let history = term.history();
        if term.alt_screen() || history.is_empty() {
            return None;
        }
        let index = match history.index_of(serial) {
            Some(index) => index,
            None if serial < history.serial(0) => return Some((0, 0)),
            None => return None,
        };
        let rows = history.line(index).rows(term.columns());
        Some((index, row.min(rows - 1)))
    }

    fn sources(&self, term: &Term) -> Vec<Source> {
        let lines = term.lines();
        let mut sources = Vec::with_capacity(lines);
        if let Some((mut index, mut row)) = self.anchor(term) {
            let history = term.history();
            while sources.len() < lines && index < history.len() {
                let rows = history.line(index).rows(term.columns());
                while row < rows && sources.len() < lines {
                    sources.push(Source::History(index, row));
                    row += 1;
                }
                index += 1;
                row = 0;
            }
        }
        let screen = lines - sources.len();
        sources.extend((0..screen).map(Source::Screen));
        sources
    }

    /// The place of the first cell of each row shown.
    fn places(&self, term: &Term) -> Vec<Place> {
        let screen = term.screen_places();
        let history = term.history();
        let mut starts: Option<(usize, Vec<usize>)> = None;
        self.sources(term)
            .into_iter()
            .map(|source| match source {
                Source::Screen(row) => Place {
                    line: screen[row].0,
                    cell: screen[row].1,
                },
                Source::History(index, row) => {
                    if starts.as_ref().is_none_or(|(at, _)| *at != index) {
                        let line = history.line(index);
                        starts = Some((index, line.row_starts(term.columns())));
                    }
                    let cell = starts.as_ref().map_or(0, |(_, starts)| starts[row]);
                    Place {
                        line: history.serial(index),
                        cell,
                    }
                }
            })
            .collect()
    }

    /// The rows shown, top first: the history's where the view is scrolled up, then the
    /// screen's.
    #[must_use]
    pub fn shown<'a>(&self, term: &'a Term) -> Vec<Shown<'a>> {
        let screen = term.screen_places();
        let history = term.history();
        let columns = term.columns();
        let mut wrapped: Option<(usize, Vec<Row>, Vec<usize>)> = None;
        self.sources(term)
            .into_iter()
            .map(|source| match source {
                Source::Screen(row) => Shown {
                    row: Cow::Borrowed(term.screen().row(row)),
                    place: Place {
                        line: screen[row].0,
                        cell: screen[row].1,
                    },
                },
                Source::History(index, row) => {
                    if wrapped.as_ref().is_none_or(|(at, ..)| *at != index) {
                        let line = history.line(index);
                        wrapped = Some((
                            index,
                            line.wrap(columns, Style::default()),
                            line.row_starts(columns),
                        ));
                    }
                    let (row, cell) = wrapped.as_mut().map_or_else(
                        || (Row::new(columns, Style::default()), 0),
                        |(_, rows, starts)| (std::mem::take(&mut rows[row]), starts[row]),
                    );
                    Shown {
                        row: Cow::Owned(row),
                        place: Place {
                            line: history.serial(index),
                            cell,
                        },
                    }
                }
            })
            .collect()
    }

    /// The place of the cell at `line` and `column` of the view.
    #[must_use]
    pub fn place_at(&self, term: &Term, line: usize, column: usize) -> Place {
        let places = self.places(term);
        let first = places[line.min(places.len() - 1)];
        Place {
            line: first.line,
            cell: first.cell + column,
        }
    }

    // Scrolling.

    /// Whether it follows the output.
    #[must_use]
    pub fn at_bottom(&self, term: &Term) -> bool {
        self.anchor(term).is_none()
    }

    /// How many rows above the screen's top the view's top is.
    #[must_use]
    pub fn offset(&self, term: &Term) -> usize {
        self.anchor(term).map_or(0, |(index, row)| {
            let history = term.history();
            history.rows() - history.row_of(index) - row
        })
    }

    /// Puts the top `offset` rows above the screen's, as far up as the history goes.
    pub fn set_offset(&mut self, term: &Term, offset: usize) {
        let history = term.history();
        let offset = offset.min(history.rows());
        self.top = if offset == 0 || term.alt_screen() {
            None
        } else {
            history
                .at_row(history.rows() - offset)
                .map(|(index, row)| (history.serial(index), row))
        };
    }

    /// Scrolls `rows` towards the history, or back down for negative ones.
    pub fn scroll(&mut self, term: &Term, rows: isize) {
        let offset = self.offset(term).saturating_add_signed(rows);
        self.set_offset(term, offset);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.top = None;
    }

    /// Scrolls to the line the previous or next prompt starts in; past the last, to the
    /// bottom.
    pub fn scroll_to_prompt(&mut self, term: &Term, up: bool) {
        if term.alt_screen() {
            return;
        }
        let top = self.places(term)[0];
        let history = term.history();
        let mut prompts = (0..history.len())
            .filter(|&index| history.line(index).prompt)
            .map(|index| history.serial(index));
        let found = if up {
            prompts.rfind(|&serial| serial < top.line || (serial == top.line && top.cell > 0))
        } else {
            prompts.find(|&serial| serial > top.line)
        };
        match found {
            Some(serial) => self.top = Some((serial, 0)),
            None if up => {}
            None => self.top = None,
        }
    }

    /// Scrolls so that the row with `place` is shown, if it is not.
    pub fn reveal(&mut self, term: &Term, place: Place) {
        let places = self.places(term);
        let columns = term.columns();
        let shown = places.iter().any(|first| {
            first.line == place.line && (first.cell..first.cell + columns).contains(&place.cell)
        });
        if shown {
            return;
        }
        let history = term.history();
        self.top = history.index_of(place.line).map(|index| {
            let starts = history.line(index).row_starts(columns);
            let row = starts.partition_point(|&start| start <= place.cell).max(1) - 1;
            (place.line, row)
        });
    }

    // Selecting.

    /// Starts a selection at `line` and `column` of the view, on the right half of the
    /// cell or the left.
    pub fn select(&mut self, term: &Term, unit: Unit, line: usize, column: usize, right: bool) {
        let place = self.place_at(term, line, column);
        self.selection = Some(Selection {
            unit,
            anchor: (place, right),
            end: (place, right),
        });
    }

    /// Moves the end of the selection to `line` and `column`, or starts one there.
    pub fn extend(&mut self, term: &Term, line: usize, column: usize, right: bool) {
        let place = self.place_at(term, line, column);
        match &mut self.selection {
            Some(selection) => selection.end = (place, right),
            None => {
                self.selection = Some(Selection {
                    unit: Unit::Cell,
                    anchor: (place, right),
                    end: (place, right),
                });
            }
        }
    }

    /// Selects the cells from `first` to `last`.
    pub fn select_cells(&mut self, first: Place, last: Place) {
        self.selection = Some(Selection {
            unit: Unit::Cell,
            anchor: (first, false),
            end: (last, true),
        });
    }

    /// Selects every line there is, up to the last with text.
    pub fn select_all(&mut self, term: &Term) {
        let serials = term.serials();
        let mut last = *serials.end();
        let width = |serial| term.line(serial).map_or(0, |line| line.width());
        while last > *serials.start() && width(last) == 0 {
            last -= 1;
        }
        let first = Place {
            line: *serials.start(),
            cell: 0,
        };
        let last = Place {
            line: last,
            cell: width(last).saturating_sub(1),
        };
        self.select_cells(first, last);
    }

    pub fn clear_selection(&mut self) {
        self.selection = None;
    }

    /// Lets go of the selection if output wrote to its lines, from the line with serial
    /// number `from` down.
    pub fn written(&mut self, from: u64) {
        if let Some(selection) = self.selection
            && selection.anchor.0.line.max(selection.end.0.line) >= from
        {
            self.selection = None;
        }
    }

    /// The first and last cell selected, with words and lines taken in whole as the
    /// selection asks; None when nothing is.
    #[must_use]
    pub fn selected(&self, term: &Term) -> Option<(Place, Place)> {
        let selection = self.selection?;
        let (low, high) = if selection.anchor <= selection.end {
            (selection.anchor, selection.end)
        } else {
            (selection.end, selection.anchor)
        };
        let (mut first, mut last) = (low.0, high.0);
        match selection.unit {
            Unit::Cell => {
                // The half of a cell the pointer is over says whether it is taken in.
                if low.1 {
                    first.cell += 1;
                }
                if !high.1 {
                    if last.cell == 0 {
                        if last.line == first.line {
                            return None;
                        }
                        last = Place {
                            line: last.line - 1,
                            cell: usize::MAX,
                        };
                    } else {
                        last.cell -= 1;
                    }
                }
                if first.line == last.line && first.cell > last.cell {
                    return None;
                }
            }
            Unit::Word => {
                first.cell = word_edge(term, first, false);
                last.cell = word_edge(term, last, true);
            }
            Unit::Line => {
                first.cell = 0;
                last.cell = usize::MAX;
            }
        }
        Some((first, last))
    }

    /// The text selected, a line of it to each line of the terminal.
    #[must_use]
    pub fn selected_text(&self, term: &Term) -> Option<String> {
        let (first, last) = self.selected(term)?;
        let mut text = String::new();
        for serial in first.line..=last.line {
            if serial > first.line {
                text.push('\n');
            }
            let Some(line) = term.line(serial) else {
                continue;
            };
            let cells = line.cells();
            let from = if serial == first.line { first.cell } else { 0 };
            let to = if serial == last.line {
                last.cell.saturating_add(1)
            } else {
                usize::MAX
            };
            let start = text.len();
            for (cell, _) in cells
                .get(from.min(cells.len())..to.min(cells.len()))
                .unwrap_or_default()
            {
                text.push_str(cell);
            }
            let kept = text[start..].trim_end().len();
            text.truncate(start + kept);
        }
        Some(text)
    }

    /// The first and last cell selected that are shown, as lines and columns of the view.
    #[must_use]
    pub fn selected_cells(&self, term: &Term) -> Option<((usize, usize), (usize, usize))> {
        let (first, last) = self.selected(term)?;
        let columns = term.columns();
        let places = self.places(term);
        let ends_after = |row: &Place, place: Place| {
            row.line > place.line || (row.line == place.line && row.cell + columns > place.cell)
        };
        let starts_before = |row: &Place, place: Place| {
            row.line < place.line || (row.line == place.line && row.cell <= place.cell)
        };
        let top = places.iter().position(|row| ends_after(row, first))?;
        let bottom = places.iter().rposition(|row| starts_before(row, last))?;
        if top > bottom {
            return None;
        }
        let start = if places[top].line == first.line {
            first.cell.saturating_sub(places[top].cell)
        } else {
            0
        };
        let row = places[bottom];
        let end = if row.line == last.line {
            (last.cell - row.cell).min(columns - 1)
        } else {
            columns - 1
        };
        Some(((top, start), (bottom, end)))
    }

    /// Where a search starts: next to the selection, or at the edge of the view it goes
    /// away from.
    #[must_use]
    pub fn search_from(&self, term: &Term, upwards: bool) -> Place {
        match self.selected(term) {
            Some((first, _)) if upwards => first,
            Some((_, last)) => Place {
                line: last.line,
                cell: last.cell.saturating_add(1),
            },
            None if upwards => Place {
                line: self.places(term).last().map_or(0, |place| place.line),
                cell: usize::MAX,
            },
            None => self.places(term)[0],
        }
    }

    // Links.

    /// The address at `line` and `column` of the view: the link a program gave the text
    /// there, or one written out in it.
    #[must_use]
    pub fn link_at(&self, term: &Term, line: usize, column: usize) -> Option<Link> {
        // A cell hovered before the terminal shrank may be outside it now.
        if line >= term.lines() || column >= term.columns() {
            return None;
        }
        let place = self.place_at(term, line, column);
        let text = term.line(place.line)?;
        let &(_, style) = text.cells().get(place.cell)?;
        if let Some(uri) = term.link(style.link)
            && openable(uri)
        {
            return Some(Link {
                uri: uri.to_owned(),
                span: Span::Program(style.link),
            });
        }

        // A program that breaks lines itself may fill a line with an address to the last
        // column and go on with it in the next.
        let columns = term.columns();
        let goes_on = |line: &Line, next: &Line| {
            let width = line.width();
            let last = line
                .cells()
                .last()
                .and_then(|(text, _)| text.chars().next());
            let first = next
                .cells()
                .first()
                .and_then(|(text, _)| text.chars().next());
            width > 0
                && width.is_multiple_of(columns)
                && last.is_some_and(continues_address)
                && first.is_some_and(continues_address)
        };
        let serials = term.serials();
        let mut lines = vec![(place.line, text)];
        while lines.len() < LONGEST_ADDRESS && lines[0].0 > *serials.start() {
            let serial = lines[0].0 - 1;
            match term.line(serial) {
                Some(above) if goes_on(&above, &lines[0].1) => lines.insert(0, (serial, above)),
                _ => break,
            }
        }
        while lines.len() < 2 * LONGEST_ADDRESS {
            let (serial, last) = &lines[lines.len() - 1];
            let serial = serial + 1;
            if serial > *serials.end() {
                break;
            }
            match term.line(serial) {
                Some(below) if goes_on(last, &below) => lines.push((serial, below)),
                _ => break,
            }
        }

        let mut chars = Vec::new();
        let mut places = Vec::new();
        for (serial, line) in &lines {
            for (cell, (text, _)) in line.cells().iter().enumerate() {
                chars.push(text.chars().next().unwrap_or('\0'));
                places.push(Place {
                    line: *serial,
                    cell,
                });
            }
        }
        let index = places.iter().position(|&at| at == place)?;
        let range = address_at(&chars, index)?;
        Some(Link {
            uri: chars[range.clone()]
                .iter()
                .filter(|&&c| c != '\0')
                .collect(),
            span: Span::Written(places[range.start], places[range.end - 1]),
        })
    }
}

/// The first cell of the word `place` is in, or the last with `forwards`. A cell that is
/// not part of a word is a word of its own.
fn word_edge(term: &Term, place: Place, forwards: bool) -> usize {
    let Some(line) = term.line(place.line) else {
        return place.cell;
    };
    let cells = line.cells();
    let in_word = |cell: usize| {
        cells.get(cell).is_some_and(|(text, _)| {
            // The second half of a wide character goes with its first.
            text.is_empty()
                || !text
                    .chars()
                    .next()
                    .is_some_and(|c| c.is_whitespace() || WORD_ENDS.contains(c))
        })
    };
    let mut cell = place.cell;
    if !in_word(cell) {
        return cell;
    }
    if forwards {
        while in_word(cell + 1) {
            cell += 1;
        }
    } else {
        while cell > 0 && in_word(cell - 1) {
            cell -= 1;
        }
    }
    cell
}

#[cfg(test)]
mod tests;
