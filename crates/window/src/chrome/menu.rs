//! A menu on a surface of its own: the main one under its button in the header bar, the
//! terminal's where it was clicked, or a session's list of tabs under its tab. The
//! compositor keeps it on the screen, and it scrolls where it is given too little height.

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
/// Past this a label is cut, as a long title would otherwise pass the screen's width.
const MAX_WIDTH: f32 = 560.0;
const RADIUS: f32 = 12.0;
const ITEM_RADIUS: f32 = 6.0;
/// The heading's height, the dot before a label and the room it takes, and the height
/// of a preview under its item's label.
const HEADING: f32 = 30.0;
const DOT: f32 = 8.0;
const DOT_ROOM: f32 = 18.0;
const CHECK_ROOM: f32 = 22.0;
const PREVIEW: f32 = 64.0;

#[derive(Debug, Clone, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each says one thing about how the item is drawn"
)]
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
    /// A check before the label, for what is on show.
    pub checked: bool,
    /// It names the items after it and cannot be chosen.
    pub title: bool,
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
    /// Where the compositor put its corner, once it has.
    pub placed: Option<(f32, f32)>,
    /// How high it may be, where the compositor gave it less than it asked for.
    limit: Option<f32>,
    /// How far its items are scrolled up, in logical pixels.
    scroll: f32,
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
    /// An item's height, the step a wheel's notch scrolls by.
    pub const ITEM: f32 = ITEM;

    pub fn new(sections: Vec<Vec<Item>>, at: (f32, f32), main: bool) -> Self {
        Self {
            sections: sections
                .into_iter()
                .filter(|items| !items.is_empty())
                .collect(),
            at,
            placed: None,
            limit: None,
            scroll: 0.0,
            focused: None,
            width: MIN_WIDTH,
            main,
            heading: None,
            filter: None,
        }
    }

    /// Stands where `old` stands, which the compositor tells only once for a popup.
    pub fn keep_place(&mut self, old: &Self) {
        self.placed = old.placed;
        self.limit = old.limit;
        self.scroll = old.scroll;
        self.scroll_by(0.0);
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
    pub fn under_main_button(width: u32, buttons: &super::WindowButtons) -> (f32, f32) {
        let end = super::MAIN_MENU_END + buttons.main_menu_shift();
        let right = width as f32 - end + super::FLAT_BUTTON.1 / 2.0;
        let bottom = super::HEADER as f32 - (super::HEADER as f32 - super::FLAT_BUTTON.1) / 2.0;
        (right, bottom + 2.0)
    }

    /// Where the + button's menu opens: under the button, at its left edge.
    pub fn under_new_tab_button(buttons: &super::WindowButtons) -> (f32, f32) {
        let bottom = super::HEADER as f32 - (super::HEADER as f32 - super::FLAT_BUTTON.1) / 2.0;
        (super::FLAT_BUTTON.0 + buttons.new_tab_shift(), bottom + 2.0)
    }

    pub fn items(&self) -> impl Iterator<Item = &Item> {
        self.sections.iter().flatten()
    }

    /// Fits the menu's width to its items, as `measure` gives their text's width in
    /// logical pixels, plain or bold.
    pub fn fit(&mut self, measure: impl Fn(&str, bool) -> f32) {
        let widest = self
            .items()
            .map(|item| {
                let end = if item.closes { SHORTCUT_GAP } else { 0.0 };
                let preview = if item.preview.is_some() {
                    PREVIEW * 2.5
                } else {
                    0.0
                };
                (measure(&item.label, item.current || item.title)
                    + item
                        .shortcut
                        .as_deref()
                        .map_or(0.0, |shortcut| SHORTCUT_GAP + measure(shortcut, false))
                    + if item.dot.is_some() { DOT_ROOM } else { 0.0 }
                    + self.check_room()
                    + end)
                    .max(preview)
            })
            .chain(
                self.heading_text()
                    .map(|heading| measure(&heading, true) + DOT_ROOM),
            )
            .fold(0.0, f32::max);
        // Whole pixels to spare, as the room for each label is cut to whole pixels.
        self.width =
            (widest.ceil() + 1.0 + 2.0 * (ITEM_PADDING + PADDING)).clamp(MIN_WIDTH, MAX_WIDTH);
    }

    /// The room before each label for the check any item in the menu has.
    fn check_room(&self) -> f32 {
        if self.items().any(|item| item.checked) {
            CHECK_ROOM
        } else {
            0.0
        }
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
        self.show_focused();
    }

    fn height(&self) -> f32 {
        let items: f32 = self.items().map(item_height).sum();
        let separators = self.sections.len().saturating_sub(1) as f32;
        2.0 * PADDING + self.heading_height() + items + separators * SEPARATOR
    }

    fn heading_height(&self) -> f32 {
        if self.heading.is_some() { HEADING } else { 0.0 }
    }

    /// How big it would like to be, in logical pixels.
    pub fn size(&self) -> (f32, f32) {
        (self.width, self.height())
    }

    /// How big it is shown, in logical pixels.
    pub fn shown_size(&self) -> (f32, f32) {
        let height = self.height();
        (
            self.width,
            self.limit.map_or(height, |limit| limit.min(height)),
        )
    }

    /// Keeps it within `height`, as high as the compositor gives it.
    pub fn limit_height(&mut self, height: f32) {
        self.limit = (height > 0.0).then_some(height);
        self.scroll_by(0.0);
    }

    /// Scrolls its items by `pixels`, down for positive ones.
    pub fn scroll_by(&mut self, pixels: f32) {
        let most = (self.height() - self.shown_size().1).max(0.0);
        self.scroll = (self.scroll + pixels).clamp(0.0, most);
    }

    /// Scrolls so that the focused item shows.
    fn show_focused(&mut self) {
        let Some((top, item)) = self
            .focused
            .and_then(|focused| Some((self.tops().nth(focused)?, self.items().nth(focused)?)))
        else {
            return;
        };
        let height = item_height(item);
        let shown = self.shown_size().1;
        if top - PADDING < self.scroll {
            self.scroll = (top - PADDING - self.heading_height()).max(0.0);
        } else if top + height + PADDING > self.scroll + shown {
            self.scroll = top + height + PADDING - shown;
        }
    }

    /// Its rectangle, in logical pixels from the window's corner.
    fn bounds(&self) -> [f32; 4] {
        let (x, y) = self.placed.unwrap_or(self.at);
        let (width, height) = self.shown_size();
        [x, y, width, height]
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

    /// The part at `(x, y)` from the window's corner: an item, the menu between them, or
    /// all outside it.
    pub fn part_at(&self, (x, y): (f64, f64)) -> Part {
        let [left, top, width, height] = self.bounds();
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
                let y = y - top + self.scroll;
                inside && (*item_top..item_top + item_height(item)).contains(&y)
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
            && (y - top + self.scroll - item_top - ITEM / 2.0).abs() < reach
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
    /// `menu` on its own surface, its corner at the margin. Gives how many of the quads
    /// are its box, and where the rest stay.
    pub(super) fn menu(
        &self,
        quads: &mut Vec<Quad>,
        menu: &Menu,
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) -> (usize, [f32; 4]) {
        let (chrome, scale, margin) = (self.chrome, self.scale, self.margin);
        let colors = chrome.colors();
        let (shown_width, shown_height) = menu.shown_size();
        let [width, menu_height] = [shown_width, shown_height].map(|value| (value * scale).round());
        let (x, y) = (margin, margin);
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
        let boxed = quads.len();
        let y = y - menu.scroll * scale;
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
        (boxed, [x, margin, width, menu_height])
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
        if item.title {
            let room = width - 2.0 * (inset + padding);
            self.menu_title(quads, &item.label, [start, middle, room], draw);
            return;
        }
        let check_room = menu.check_room() * scale;
        if item.checked
            && let Some(icon) = draw(&Drawing::Icon(Icon::Check))
        {
            quads.push(centred(
                start + check_room / 2.0 - 3.0 * scale,
                middle,
                icon,
                color,
            ));
        }
        start += check_room;
        if let Some(dot) = item.dot {
            quads.push(circle(start + DOT * scale / 2.0, middle, DOT * scale, dot));
            start += DOT_ROOM * scale;
        }
        let end = if item.closes { BUTTON * scale } else { 0.0 };
        let room = width - (start - x) - inset - padding - end;
        let shortcut = item.shortcut.as_ref().and_then(|shortcut| {
            draw(&Drawing::Label {
                text: shortcut.clone(),
                width: room.max(0.0) as u32,
                bold: false,
            })
        });
        let beside = shortcut.map_or(0.0, |(_, glyph)| {
            halves(glyph.size).0 as f32 + SHORTCUT_GAP * scale
        });
        let label = Drawing::Label {
            text: item.label.clone(),
            width: (room - beside).max(0.0) as u32,
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
        if let Some(glyph) = shortcut {
            let (glyph_width, _) = halves(glyph.1.size);
            let right = x + width - inset - padding - glyph_width as f32;
            let dim = (colors.foreground & !0xff) | 0x8c;
            quads.push(self.left_label(right, middle, glyph, dim));
        }
    }

    /// A line naming the items after it, from `start`, centred on `middle`.
    fn menu_title(
        &self,
        quads: &mut Vec<Quad>,
        title: &str,
        [start, middle, room]: [f32; 3],
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) {
        let label = Drawing::Label {
            text: title.to_owned(),
            width: room.max(0.0) as u32,
            bold: true,
        };
        if let Some(glyph) = draw(&label) {
            let dim = (self.chrome.colors().foreground & !0xff) | 0xb0;
            quads.push(self.left_label(start, middle, glyph, dim));
        }
    }

    /// A line of text from `x`, centred on `middle` by its line.
    pub(super) fn left_label(
        &self,
        x: f32,
        middle: f32,
        (index, glyph): (u32, GpuGlyph),
        color: u32,
    ) -> Quad {
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
        let item = |index: f32, gap: f32| 100.0 + f64::from(PADDING + index * ITEM + gap) + 5.0;
        assert_eq!(menu.part_at((110.0, item(0.0, 0.0))), Part::MenuItem(0));
        assert_eq!(menu.part_at((110.0, item(1.0, 0.0))), Part::MenuItem(1));
        assert_eq!(
            menu.part_at((110.0, item(2.0, SEPARATOR))),
            Part::MenuItem(2)
        );
        assert_eq!(menu.part_at((110.0, 102.0)), Part::Menu);
        assert_eq!(menu.part_at((50.0, 50.0)), Part::OffMenu);
    }

    #[test]
    fn a_long_title_does_not_widen_a_menu_past_its_limit() {
        let mut menu = menu();
        menu.fit(|_, _| 10_000.0);
        assert!((menu.width - MAX_WIDTH).abs() < 0.1);
    }

    #[test]
    fn items_are_found_where_the_compositor_put_the_menu() {
        let mut menu = menu();
        menu.placed = Some((10.0, 20.0));
        let y = 20.0 + f64::from(PADDING) + 5.0;
        assert_eq!(menu.part_at((20.0, y)), Part::MenuItem(0));
        assert_eq!(menu.part_at((500.0, 500.0)), Part::OffMenu);
    }

    #[test]
    fn a_menu_listed_again_stays_where_the_compositor_put_the_old_one() {
        let mut old = menu();
        old.placed = Some((10.0, 20.0));
        old.limit_height(ITEM + 2.0 * PADDING);
        old.scroll_by(ITEM);
        let mut again = menu();
        again.keep_place(&old);
        assert_eq!(again.placed, Some((10.0, 20.0)));
        assert!((again.shown_size().1 - old.shown_size().1).abs() < 0.1);
        assert!((again.scroll - old.scroll).abs() < 0.1);
    }

    #[test]
    fn a_menu_given_too_little_height_scrolls_to_its_focus() {
        let mut menu = menu();
        let whole = menu.size().1;
        menu.limit_height(ITEM + 2.0 * PADDING);
        assert!((menu.shown_size().1 - (ITEM + 2.0 * PADDING)).abs() < 0.1);
        menu.step(1);
        menu.step(1);
        assert_eq!(menu.focused, Some(2));
        assert!(menu.scroll > 0.0);
        menu.scroll_by(10_000.0);
        assert!((menu.scroll - (whole - menu.shown_size().1)).abs() < 0.1);
        menu.scroll_by(-10_000.0);
        assert!(menu.scroll.abs() < 0.1);
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
