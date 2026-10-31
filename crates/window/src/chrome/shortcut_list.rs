//! The list of shortcuts over the window: each action and the keys that do it, by
//! section, scrolled a line at a time where the window is too short for all of it.

use super::{
    ALL_CORNERS, Drawing, GpuGlyph, Painter, Part, Quad, RADIUS, SHADOW, dialog::centred_line,
    halves, rounded,
};
use crate::fonts::HEADING;

/// Its width at most, and the room kept between it and the window's edges.
const WIDTH: f32 = 480.0;
const INSET: f32 = 12.0;
/// The room around its contents, and between the heading and the lines.
const PADDING: f32 = 24.0;
const SPACING: f32 = 10.0;
/// The height of a line, a section's title or a shortcut.
const LINE: f32 = 30.0;

#[derive(Debug, Default)]
pub struct ShortcutList {
    pub heading: String,
    /// Each section's title, and its actions with the keys that do them.
    pub sections: Vec<(String, Vec<(String, String)>)>,
    /// The first line shown, past those scrolled away.
    pub first: usize,
    /// The height of a line of text, in logical pixels.
    pub line: f32,
}

/// A line of the list: a section's title, or an action and its keys.
enum Line<'a> {
    Title(&'a str),
    Shortcut(&'a str, &'a str),
}

/// Where the list's parts are, in logical pixels from the window's corner.
struct Layout {
    card: [f32; 4],
    heading: f32,
    lines: f32,
    /// How many lines there is room for.
    shown: usize,
}

impl ShortcutList {
    fn lines(&self) -> impl Iterator<Item = Line<'_>> {
        self.sections.iter().flat_map(|(title, shortcuts)| {
            std::iter::once(Line::Title(title)).chain(
                shortcuts
                    .iter()
                    .map(|(action, keys)| Line::Shortcut(action, keys)),
            )
        })
    }

    fn count(&self) -> usize {
        self.sections
            .iter()
            .map(|(_, shortcuts)| 1 + shortcuts.len())
            .sum()
    }

    fn layout(&self, (width, height): (f32, f32)) -> Layout {
        let card_width = WIDTH.min(width - 2.0 * INSET).max(0.0);
        let around = PADDING + self.line * HEADING + SPACING + PADDING;
        let room = ((height - 2.0 * INSET - around) / LINE).floor().max(1.0) as usize;
        let shown = room.min(self.count());
        let card_height = around + shown as f32 * LINE;
        let (x, y) = (
            (width - card_width) / 2.0,
            ((height - card_height) / 2.0).max(0.0),
        );
        Layout {
            card: [x, y, card_width, card_height],
            heading: y + PADDING,
            lines: y + PADDING + self.line * HEADING + SPACING,
            shown,
        }
    }

    /// Scrolls `lines` on, back for negative ones, as far as there are lines to show in
    /// a window `size` big.
    pub fn scroll(&mut self, lines: isize, size: (u32, u32)) {
        let shown = self.layout((size.0 as f32, size.1 as f32)).shown;
        let last = self.count().saturating_sub(shown);
        self.first = self.first.saturating_add_signed(lines).min(last);
    }

    /// How many lines a page is in a window `size` big.
    pub fn page(&self, size: (u32, u32)) -> usize {
        self.layout((size.0 as f32, size.1 as f32)).shown
    }

    /// The part at `(x, y)` from the corner of a window `size` big: the list, or the
    /// window around it.
    pub fn part_at(&self, (x, y): (f64, f64), size: (u32, u32)) -> Part {
        let [left, top, width, height] = self.layout((size.0 as f32, size.1 as f32)).card;
        let (x, y) = (x as f32, y as f32);
        if (left..left + width).contains(&x) && (top..top + height).contains(&y) {
            Part::ShortcutList
        } else {
            Part::OffShortcutList
        }
    }

    /// What a screen reader reads of it: each line, all of them.
    pub fn text(&self) -> String {
        self.lines()
            .map(|line| match line {
                Line::Title(title) => format!("{title}\n"),
                Line::Shortcut(action, keys) => format!("{action}: {keys}\n"),
            })
            .collect()
    }
}

impl Painter<'_> {
    /// `list` in the middle of a window `height` high, over all else, which it dims.
    pub(super) fn shortcut_list(
        &self,
        quads: &mut Vec<Quad>,
        list: &ShortcutList,
        height: f32,
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) {
        let (chrome, scale, margin) = (self.chrome, self.scale, self.margin);
        let colors = chrome.colors();
        let corners = if chrome.floating && chrome.framed() {
            ALL_CORNERS
        } else {
            0
        };
        quads.push(Quad {
            corners,
            ..rounded(
                margin,
                margin,
                self.width,
                height,
                RADIUS * scale,
                colors.scrim,
            )
        });
        let layout = list.layout((self.width / scale, height / scale));
        let [x, y, width, card_height] = layout.card;
        let [x, y, width, card_height] = [
            (margin + x * scale).round(),
            (margin + y * scale).round(),
            (width * scale).round(),
            (card_height * scale).round(),
        ];
        quads.push(Quad {
            blur: 10.0 * scale,
            ..rounded(
                x,
                y + 2.0 * scale,
                width,
                card_height,
                RADIUS * scale,
                SHADOW,
            )
        });
        quads.push(rounded(
            x,
            y,
            width,
            card_height,
            RADIUS * scale,
            colors.dialog,
        ));
        let inset = PADDING * scale;
        let room = (width - 2.0 * inset).max(0.0);
        let heading = Drawing::Heading {
            text: list.heading.clone(),
            width: room as u32,
        };
        if let Some(glyph) = draw(&heading) {
            let top = margin + layout.heading * scale;
            quads.push(centred_line(x + width / 2.0, top, glyph, colors.foreground));
        }
        let top = margin + layout.lines * scale;
        let rule = |at: f32| Quad {
            x: x + inset,
            y: at.round(),
            width: room,
            height: scale.round().max(1.0),
            color: colors.shade,
            ..Quad::default()
        };
        // A line where more is scrolled out of sight.
        if list.first > 0 {
            quads.push(rule(top));
        }
        if list.first + layout.shown < list.count() {
            quads.push(rule(top + layout.shown as f32 * LINE * scale));
        }
        for (index, line) in list.lines().skip(list.first).take(layout.shown).enumerate() {
            let middle = top + (index as f32 + 0.5) * LINE * scale;
            self.shortcut_line(quads, &line, [x + inset, middle, room], draw);
        }
    }

    /// `line` of the list, from `start` across `room`, centred on `middle`.
    fn shortcut_line(
        &self,
        quads: &mut Vec<Quad>,
        line: &Line<'_>,
        [start, middle, room]: [f32; 3],
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) {
        let foreground = self.chrome.colors().foreground;
        let (action, keys, bold) = match *line {
            Line::Title(title) => (title, None, true),
            Line::Shortcut(action, keys) => (action, Some(keys), false),
        };
        let keys_width = keys.map_or(0.0, |keys| {
            let label = Drawing::Label {
                text: keys.to_owned(),
                width: (room / 2.0) as u32,
                bold: false,
            };
            draw(&label).map_or(0.0, |glyph| {
                let (glyph_width, _) = halves(glyph.1.size);
                let right = start + room - glyph_width as f32;
                let dim = (foreground & !0xff) | 0x8c;
                quads.push(self.left_label(right, middle, glyph, dim));
                glyph_width as f32 + PADDING * self.scale
            })
        });
        let label = Drawing::Label {
            text: action.to_owned(),
            width: (room - keys_width).max(0.0) as u32,
            bold,
        };
        if let Some(glyph) = draw(&label) {
            quads.push(self.left_label(start, middle, glyph, foreground));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list() -> ShortcutList {
        let shortcuts = |count: usize| {
            (0..count)
                .map(|index| (format!("action {index}"), format!("Ctrl+{index}")))
                .collect()
        };
        ShortcutList {
            heading: "Keyboard Shortcuts".to_owned(),
            sections: vec![
                ("Tabs".to_owned(), shortcuts(10)),
                ("Terminal".to_owned(), shortcuts(10)),
            ],
            first: 0,
            line: 20.0,
        }
    }

    #[test]
    fn a_short_window_shows_what_fits_and_scrolls_to_the_rest() {
        let mut list = list();
        let size = (600, 400);
        let shown = list.page(size);
        assert!(shown < 22, "{shown} lines");
        list.scroll(100, size);
        assert_eq!(list.first, 22 - shown);
        list.scroll(-3, size);
        assert_eq!(list.first, 22 - shown - 3);
        list.scroll(-100, size);
        assert_eq!(list.first, 0);
    }

    #[test]
    fn a_tall_window_shows_all_and_does_not_scroll() {
        let mut list = list();
        let size = (600, 1200);
        assert_eq!(list.page(size), 22);
        list.scroll(5, size);
        assert_eq!(list.first, 0);
        assert_eq!(list.part_at((5.0, 5.0), size), Part::OffShortcutList);
        assert_eq!(list.part_at((300.0, 600.0), size), Part::ShortcutList);
    }
}
