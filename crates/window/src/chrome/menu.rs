//! A menu over the window: the main one under its button in the header bar, the
//! terminal's where it was clicked, or a session's list of tabs under its tab. It stays
//! within the window, turning to the other side of the point it opens at where it would
//! not fit.

use super::{
    BUTTON, Drawing, GpuGlyph, Icon, OUTLINE, Painter, Part, Quad, SHADOW, centred, circle, entry,
    halves, offset, rounded,
};

/// The room around the items, their height, the room either side of their text, and
/// between a label and its shortcut; a section's separator takes `SEPARATOR` in all.
const PADDING: f32 = 6.0;
const ITEM: f32 = 32.0;
const ITEM_PADDING: f32 = 12.0;
const SHORTCUT_GAP: f32 = 24.0;
const SEPARATOR: f32 = 13.0;
const MIN_WIDTH: f32 = 160.0;
const RADIUS: f32 = 12.0;
const ITEM_RADIUS: f32 = 6.0;
/// The heading's height, the dot before a label and the room it takes, and the height
/// of a preview under its item's label.
const HEADING: f32 = 30.0;
const DOT: f32 = 8.0;
const DOT_ROOM: f32 = 18.0;
const PREVIEW: f32 = 64.0;

#[derive(Debug, Clone, Default)]
pub struct Item {
    pub label: String,
    /// The keys that do the same, to show beside the label.
    pub shortcut: Option<String>,
    pub enabled: bool,
    /// A dot before the label in this colour, such as for a tab that wants attention.
    pub dot: Option<u32>,
    /// It stands for what is in use, such as the tab shown: its label is bold.
    pub current: bool,
    /// A button at its end closes what it stands for.
    pub closes: bool,
    pub preview: Option<Preview>,
}

/// A small picture of what an item stands for: a colour for each block of cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preview {
    pub columns: usize,
    pub rows: usize,
    /// Row by row, `columns` in each.
    pub colors: Vec<u32>,
}

#[derive(Debug)]
pub struct Menu {
    /// Items in sections, with a line between each.
    pub sections: Vec<Vec<Item>>,
    /// The point it opens at, in logical pixels from the window's corner.
    pub at: (f32, f32),
    /// The item the keys move to and choose, counted across sections.
    pub focused: Option<usize>,
    /// Its width, which its widest item sets, in logical pixels.
    pub width: f32,
    /// It is the main menu, opened from the header bar.
    pub main: bool,
    /// A line over the items saying what they are, and a colour beside it.
    pub heading: Option<(String, Option<u32>)>,
    /// Text typed to narrow the items down, shown in the heading; none where typing does
    /// not.
    pub filter: Option<String>,
}

impl Menu {
    pub fn new(sections: Vec<Vec<Item>>, at: (f32, f32), main: bool) -> Self {
        Self {
            sections: sections
                .into_iter()
                .filter(|items| !items.is_empty())
                .collect(),
            at,
            focused: None,
            width: MIN_WIDTH,
            main,
            heading: None,
            filter: None,
        }
    }

    /// The line over its items, with the filter typed after it.
    fn heading_text(&self) -> Option<String> {
        let (heading, _) = self.heading.as_ref()?;
        Some(
            match self.filter.as_deref().filter(|filter| !filter.is_empty()) {
                Some(filter) => format!("{heading} — “{filter}”"),
                None => heading.clone(),
            },
        )
    }

    /// Where the main menu opens: under its button, its right edge at the button's, in
    /// a window `width` wide.
    pub fn under_main_button(width: u32) -> (f32, f32) {
        let right = width as f32 - super::MAIN_MENU_END + super::FLAT_BUTTON.1 / 2.0;
        let bottom = super::HEADER as f32 - (super::HEADER as f32 - super::FLAT_BUTTON.1) / 2.0;
        (right, bottom + 2.0)
    }

    /// Where the + button's menu opens: under the button, at its left edge.
    pub fn under_new_tab_button() -> (f32, f32) {
        let bottom = super::HEADER as f32 - (super::HEADER as f32 - super::FLAT_BUTTON.1) / 2.0;
        (super::FLAT_BUTTON.0, bottom + 2.0)
    }

    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.sections.iter().flatten()
    }

    /// Fits the menu's width to its items, as `measure` gives their text's width in
    /// logical pixels.
    pub fn fit(&mut self, measure: impl Fn(&str) -> f32) {
        let widest = self
            .items()
            .map(|item| {
                let end = if item.closes { SHORTCUT_GAP } else { 0.0 };
                let preview = if item.preview.is_some() {
                    PREVIEW * 2.5
                } else {
                    0.0
                };
                (measure(&item.label)
                    + item
                        .shortcut
                        .as_deref()
                        .map_or(0.0, |shortcut| SHORTCUT_GAP + measure(shortcut))
                    + if item.dot.is_some() { DOT_ROOM } else { 0.0 }
                    + end)
                    .max(preview)
            })
            .chain(
                self.heading_text()
                    .map(|heading| measure(&heading) + DOT_ROOM),
            )
            .fold(0.0, f32::max);
        // Whole pixels to spare, as the room for each label is cut to whole pixels.
        self.width = (widest.ceil() + 1.0 + 2.0 * (ITEM_PADDING + PADDING)).max(MIN_WIDTH);
    }

    /// Moves the focus `steps` items on, past those that cannot be chosen.
    pub fn step(&mut self, steps: isize) {
        let enabled: Vec<usize> = self
            .items()
            .enumerate()
            .filter(|(_, item)| item.enabled)
            .map(|(index, _)| index)
            .collect();
        let Ok(count) = isize::try_from(enabled.len()) else {
            return;
        };
        if count == 0 {
            return;
        }
        let now = self
            .focused
            .and_then(|focused| enabled.iter().position(|&index| index == focused))
            .and_then(|position| isize::try_from(position).ok());
        let next = match now {
            Some(position) => (position + steps).rem_euclid(count),
            None if steps < 0 => count - 1,
            None => 0,
        };
        self.focused = Some(enabled[next.unsigned_abs()]);
    }

    fn height(&self) -> f32 {
        let items: f32 = self.items().map(item_height).sum();
        let separators = self.sections.len().saturating_sub(1) as f32;
        2.0 * PADDING + self.heading_height() + items + separators * SEPARATOR
    }

    fn heading_height(&self) -> f32 {
        if self.heading.is_some() { HEADING } else { 0.0 }
    }

    /// Its rectangle in a window `size` big, in logical pixels.
    fn bounds(&self, (width, height): (f32, f32)) -> [f32; 4] {
        let (menu_width, menu_height) = (self.width.min(width), self.height().min(height));
        let (x, y) = self.at;
        let x = if x + menu_width <= width && !self.main {
            x
        } else {
            (x - menu_width).max(0.0)
        };
        let y = if y + menu_height <= height {
            y
        } else {
            (y - menu_height).max(0.0)
        };
        [x, y, menu_width, menu_height]
    }

    /// Each item's top, in logical pixels from the menu's.
    fn tops(&self) -> impl Iterator<Item = f32> + '_ {
        self.sections
            .iter()
            .enumerate()
            .flat_map(|(section, items)| items.iter().map(move |item| (section, item)))
            .scan(
                (PADDING + self.heading_height(), None),
                |(top, last), (section, item)| {
                    if last.is_some_and(|last| last != section) {
                        *top += SEPARATOR;
                    }
                    *last = Some(section);
                    let this = *top;
                    *top += item_height(item);
                    Some(this)
                },
            )
    }

    /// The part at `(x, y)` from the corner of a window `size` big: an item, the menu
    /// between them, or all the window outside it.
    pub fn part_at(&self, (x, y): (f64, f64), size: (u32, u32)) -> Part {
        let [left, top, width, height] = self.bounds((size.0 as f32, size.1 as f32));
        let (x, y) = (x as f32, y as f32);
        if !((left..left + width).contains(&x) && (top..top + height).contains(&y)) {
            return Part::OffMenu;
        }
        let inside = (PADDING..width - PADDING).contains(&(x - left));
        let Some((index, item_top)) = self
            .tops()
            .zip(self.items())
            .enumerate()
            .find(|(_, (item_top, item))| {
                inside && (*item_top..item_top + item_height(item)).contains(&(y - top))
            })
            .map(|(index, (item_top, _))| (index, item_top))
        else {
            return Part::Menu;
        };
        let closes = self.items().nth(index).is_some_and(|item| item.closes);
        let close = width - PADDING - ITEM_PADDING - BUTTON / 2.0 + ITEM_PADDING / 2.0;
        let reach = BUTTON / 2.0;
        if closes
            && (x - left - close).abs() < reach
            && (y - top - item_top - ITEM / 2.0).abs() < reach
        {
            Part::MenuClose(index)
        } else {
            Part::MenuItem(index)
        }
    }
}

fn item_height(item: &Item) -> f32 {
    if item.preview.is_some() {
        ITEM + PREVIEW
    } else {
        ITEM
    }
}

impl Painter<'_> {
    /// `menu` over all else in a window `height` high.
    pub(super) fn menu(
        &self,
        quads: &mut Vec<Quad>,
        menu: &Menu,
        height: f32,
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) {
        let (chrome, scale, margin) = (self.chrome, self.scale, self.margin);
        let colors = chrome.colors();
        let bounds = menu.bounds((self.width / scale, height / scale));
        let [x, y, width, menu_height] = bounds.map(|value| (value * scale).round());
        let (x, y) = (x + margin, y + margin);
        quads.push(Quad {
            blur: 8.0 * scale,
            ..rounded(x, y + scale, width, menu_height, RADIUS * scale, SHADOW)
        });
        quads.push(rounded(
            x - scale,
            y - scale,
            width + 2.0 * scale,
            menu_height + 2.0 * scale,
            (RADIUS + 1.0) * scale,
            OUTLINE,
        ));
        quads.push(rounded(
            x,
            y,
            width,
            menu_height,
            RADIUS * scale,
            colors.dialog,
        ));
        let (inset, padding) = (PADDING * scale, ITEM_PADDING * scale);
        if let Some(heading) = menu.heading_text() {
            let middle = y + (PADDING + HEADING / 2.0) * scale;
            let mut start = x + inset + padding;
            if let Some(color) = menu.heading.as_ref().and_then(|(_, color)| *color) {
                quads.push(circle(
                    start + DOT * scale / 2.0,
                    middle,
                    DOT * scale,
                    color,
                ));
                start += DOT_ROOM * scale;
            }
            let label = Drawing::Label {
                text: heading,
                width: (width - (start - x) - inset - padding).max(0.0) as u32,
                bold: true,
            };
            if let Some(glyph) = draw(&label) {
                let dim = (colors.foreground & !0xff) | 0xb0;
                quads.push(self.left_label(start, middle, glyph, dim));
            }
        }
        let tops: Vec<f32> = menu.tops().collect();
        let mut index = 0;
        for (section, items) in menu.sections.iter().enumerate() {
            if section > 0 {
                let line = tops[index] - SEPARATOR / 2.0;
                quads.push(Quad {
                    x: x + inset,
                    y: (y + line * scale).round(),
                    width: width - 2.0 * inset,
                    height: scale.round().max(1.0),
                    color: colors.shade,
                    ..Quad::default()
                });
            }
            for item in items {
                let top = (y + tops[index] * scale).round();
                self.menu_item(quads, menu, item, index, [x, top, width], draw);
                index += 1;
            }
        }
    }

    /// The menu's item at `index`, its top left at `[x, top]` of a menu `width` wide.
    fn menu_item(
        &self,
        quads: &mut Vec<Quad>,
        menu: &Menu,
        item: &Item,
        index: usize,
        [x, top, width]: [f32; 3],
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) {
        let (chrome, scale) = (self.chrome, self.scale);
        let colors = chrome.colors();
        let (inset, padding) = (PADDING * scale, ITEM_PADDING * scale);
        let item_height = self::item_height(item) * scale;
        let part = Part::MenuItem(index);
        let lit = item.enabled
            && (menu.focused == Some(index)
                || chrome.over(part, false)
                || chrome.over(Part::MenuClose(index), false));
        if lit {
            let shade = colors.flat[usize::from(chrome.over(part, true))];
            quads.push(rounded(
                x + inset,
                top,
                width - 2.0 * inset,
                item_height,
                ITEM_RADIUS * scale,
                shade,
            ));
        }
        let color = if item.enabled {
            colors.foreground
        } else {
            (colors.foreground & !0xff) | 0x60
        };
        let middle = top + ITEM * scale / 2.0;
        let mut start = x + inset + padding;
        if let Some(dot) = item.dot {
            quads.push(circle(start + DOT * scale / 2.0, middle, DOT * scale, dot));
            start += DOT_ROOM * scale;
        }
        let end = if item.closes { BUTTON * scale } else { 0.0 };
        let room = width - (start - x) - inset - padding - end;
        let label = Drawing::Label {
            text: item.label.clone(),
            width: room.max(0.0) as u32,
            bold: item.current,
        };
        if let Some(glyph) = draw(&label) {
            quads.push(self.left_label(start, middle, glyph, color));
        }
        if item.closes {
            let centre = x + width - inset - padding - BUTTON * scale / 2.0 + padding / 2.0;
            let close = Part::MenuClose(index);
            if chrome.over(close, false) {
                let shade = colors.flat[usize::from(chrome.over(close, true))];
                quads.push(circle(centre, middle, BUTTON * scale, shade));
            }
            if lit && let Some(icon) = draw(&Drawing::Icon(Icon::Close)) {
                quads.push(centred(centre, middle, icon, colors.foreground));
            }
        }
        if let Some(preview) = &item.preview {
            preview_quads(
                quads,
                preview,
                scale,
                [
                    x + inset + padding,
                    top + ITEM * scale,
                    width - 2.0 * (inset + padding),
                    (PREVIEW - PADDING) * scale,
                ],
            );
        }
        if let Some(shortcut) = &item.shortcut {
            let label = Drawing::Label {
                text: shortcut.clone(),
                width: room.max(0.0) as u32,
                bold: false,
            };
            if let Some(glyph) = draw(&label) {
                let (glyph_width, _) = halves(glyph.1.size);
                let right = x + width - inset - padding - glyph_width as f32;
                let dim = (colors.foreground & !0xff) | 0x8c;
                quads.push(self.left_label(right, middle, glyph, dim));
            }
        }
    }

    /// A line of text from `x`, centred on `middle` by its line.
    fn left_label(&self, x: f32, middle: f32, (index, glyph): (u32, GpuGlyph), color: u32) -> Quad {
        let (_, top) = offset(glyph);
        let y = (middle - self.line as f32 / 2.0).floor() + top as f32;
        entry(x.round(), y, index, glyph, color)
    }
}

/// `preview` fitted into `[x, y, width, height]`, keeping the blocks' shape, outlined
/// so a terminal of the menu's colour still stands apart from it.
fn preview_quads(
    quads: &mut Vec<Quad>,
    preview: &Preview,
    scale: f32,
    [x, y, width, height]: [f32; 4],
) {
    if preview.columns == 0 || preview.rows == 0 {
        return;
    }
    // Cells are about twice as high as wide.
    let block = (width / preview.columns as f32).min(height / preview.rows as f32 / 2.0);
    let (block_width, block_height) = (block, 2.0 * block);
    quads.push(Quad {
        x: x.round() - scale,
        y: y.round() - scale,
        width: (preview.columns as f32 * block_width).ceil() + 2.0 * scale,
        height: (preview.rows as f32 * block_height).ceil() + 2.0 * scale,
        color: OUTLINE,
        ..Quad::default()
    });
    for (index, &color) in preview.colors.iter().enumerate() {
        let (row, column) = (index / preview.columns, index % preview.columns);
        quads.push(Quad {
            x: (x + column as f32 * block_width).round(),
            y: (y + row as f32 * block_height).round(),
            width: block_width.ceil(),
            height: block_height.ceil(),
            color,
            ..Quad::default()
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(label: &str, enabled: bool) -> Item {
        Item {
            label: label.to_owned(),
            enabled,
            ..Item::default()
        }
    }

    fn menu() -> Menu {
        Menu::new(
            vec![
                vec![item("Copy", false), item("Paste", true)],
                vec![item("New Tab", true)],
            ],
            (100.0, 100.0),
            false,
        )
    }

    #[test]
    fn items_are_found_by_where_they_are_and_the_rest_closes_the_menu() {
        let menu = menu();
        let size = (800, 600);
        let item = |index: f32, gap: f32| 100.0 + f64::from(PADDING + index * ITEM + gap) + 5.0;
        assert_eq!(
            menu.part_at((110.0, item(0.0, 0.0)), size),
            Part::MenuItem(0)
        );
        assert_eq!(
            menu.part_at((110.0, item(1.0, 0.0)), size),
            Part::MenuItem(1)
        );
        assert_eq!(
            menu.part_at((110.0, item(2.0, SEPARATOR)), size),
            Part::MenuItem(2)
        );
        assert_eq!(menu.part_at((110.0, 102.0), size), Part::Menu);
        assert_eq!(menu.part_at((50.0, 50.0), size), Part::OffMenu);
    }

    #[test]
    fn near_the_window_s_corner_it_opens_the_other_way() {
        let menu = Menu::new(menu().sections, (790.0, 590.0), false);
        let [x, y, width, height] = menu.bounds((800.0, 600.0));
        assert!(x + width <= 790.0 && y + height <= 590.0);
    }

    #[test]
    fn the_keys_move_past_items_that_cannot_be_chosen() {
        let mut menu = menu();
        menu.step(1);
        assert_eq!(menu.focused, Some(1));
        menu.step(1);
        assert_eq!(menu.focused, Some(2));
        menu.step(1);
        assert_eq!(menu.focused, Some(1));
        menu.step(-1);
        assert_eq!(menu.focused, Some(2));
    }
}
