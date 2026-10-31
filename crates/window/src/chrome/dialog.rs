//! A dialog over the window: asking before it does what the user may not mean, such as
//! stopping programs or running each line of a paste, or for a line of text.

use super::{
    ACCENT, ALL_CORNERS, Drawing, FLAT_RADIUS, Field, GpuGlyph, Painter, Part, Quad, RADIUS,
    SHADOW, entry, halves, offset, rounded,
};
use crate::fonts::HEADING;

/// The dialog's width at most, and the room kept between it and the window's sides.
const WIDTH: f32 = 340.0;
const INSET: f32 = 12.0;
/// The room around its contents, and between the heading and the body.
const PADDING: f32 = 24.0;
const SPACING: f32 = 10.0;
/// The room around its contents where they would not fit otherwise.
const TIGHT_PADDING: f32 = 8.0;
/// Its buttons' and field's height, and the room between the buttons.
const BUTTON: f32 = 34.0;
const BUTTON_SPACING: f32 = 12.0;
/// The ring around the button the keys press: its width, and how far it stands out.
const FOCUS: f32 = 2.0;
/// The size of a colour to pick, and the room the ring around the picked one leaves.
const SWATCH: f32 = 24.0;
const SWATCH_GAP: f32 = 3.0;
/// What shades a coloured button, hovered and then pressed: its white text lightens
/// it, as text of the window's colour shades other buttons.
const WARNING_SHADES: [u32; 2] = [0xffff_ff1a, 0x0000_0633];

#[derive(Debug)]
pub struct Dialog {
    pub heading: String,
    pub body: String,
    /// The buttons' labels: the first declines, the last does what was asked, and one
    /// between does what it says.
    pub responses: Vec<String>,
    pub appearance: Appearance,
    /// The button Enter presses; none while the field has the keys.
    pub focused: Option<usize>,
    /// The line of text it asks for.
    pub field: Option<Field>,
    /// The colours it offers to pick from under the field, and the one picked.
    pub swatches: Vec<u32>,
    pub picked: Option<usize>,
    /// The body cut into lines that fit, and the height of one, in logical pixels.
    pub lines: Vec<String>,
    pub line: f32,
}

/// How the button that does what was asked looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Appearance {
    /// It stops or runs something, so it is coloured to warn.
    Destructive,
    /// It is what the dialog is for.
    Suggested,
}

/// Where a dialog's parts are, in logical pixels from the window's corner.
struct Layout {
    card: [f32; 4],
    /// The window is too low for the body, which is left out.
    tight: bool,
    heading: f32,
    body: f32,
    field: [f32; 4],
    swatches: Vec<[f32; 4]>,
    buttons: Vec<[f32; 4]>,
}

impl Dialog {
    /// A dialog that asks to confirm, Enter declining.
    pub fn new(heading: &str, body: &str, responses: [&str; 2]) -> Self {
        Self {
            heading: heading.to_owned(),
            body: body.to_owned(),
            responses: responses.map(str::to_owned).to_vec(),
            appearance: Appearance::Destructive,
            focused: Some(0),
            field: None,
            swatches: Vec::new(),
            picked: None,
            lines: Vec::new(),
            line: 0.0,
        }
    }

    /// A dialog that asks for a line of text, starting as `text` selected, Enter accepting
    /// it.
    pub fn asking(heading: &str, body: &str, responses: [&str; 2], text: &str) -> Self {
        Self {
            appearance: Appearance::Suggested,
            focused: None,
            field: Some(Field::holding_selected(text)),
            ..Self::new(heading, body, responses)
        }
    }

    /// Whether the button at `index` is the one that does what was asked.
    pub fn accepts(&self, index: usize) -> bool {
        index + 1 == self.responses.len()
    }

    /// How wide the body's lines may be in a window `width` wide.
    pub fn text_width(width: f32) -> f32 {
        card_width(width) - 2.0 * PADDING
    }

    fn layout(&self, (width, height): (f32, f32)) -> Layout {
        let card_width = card_width(width);
        let heading = self.line * HEADING;
        let body = self.lines.len() as f32 * self.line;
        let field = if self.field.is_some() {
            PADDING / 2.0 + BUTTON
        } else {
            0.0
        };
        let swatches = if self.swatches.is_empty() {
            0.0
        } else {
            PADDING / 2.0 + SWATCH
        };
        let roomy =
            PADDING + heading + SPACING + body + field + swatches + PADDING + BUTTON + PADDING;
        let tight = roomy > height;
        // Its buttons stay in reach in the lowest window, where it says less.
        let (padding, spacing, body, field, swatches) = if tight {
            let field = if self.field.is_some() { BUTTON } else { 0.0 };
            let swatches = if self.swatches.is_empty() {
                0.0
            } else {
                TIGHT_PADDING + SWATCH
            };
            (TIGHT_PADDING, 0.0, 0.0, field, swatches)
        } else {
            (PADDING, SPACING, body, field, swatches)
        };
        let card_height =
            2.0 * padding + heading + spacing + body + field + swatches + padding + BUTTON;
        let (x, y) = (
            (width - card_width) / 2.0,
            ((height - card_height) / 2.0).max(0.0),
        );
        let count = self.responses.len() as f32;
        let button_width = (card_width - 2.0 * PADDING - (count - 1.0) * BUTTON_SPACING) / count;
        let button_y = y + card_height - padding - BUTTON;
        let row = card_width - 2.0 * PADDING;
        let step = (row - SWATCH) / (self.swatches.len().max(2) - 1) as f32;
        let field_y = if tight {
            y + padding + heading
        } else {
            y + PADDING + heading + SPACING + body + PADDING / 2.0
        };
        let swatch_y = if tight {
            field_y + field + TIGHT_PADDING
        } else {
            y + PADDING + heading + SPACING + body + field + PADDING / 2.0
        };
        Layout {
            card: [x, y, card_width, card_height],
            tight,
            heading: y + padding,
            body: y + padding + heading + spacing,
            field: [x + PADDING, field_y, card_width - 2.0 * PADDING, BUTTON],
            swatches: (0..self.swatches.len())
                .map(|index| [x + PADDING + index as f32 * step, swatch_y, SWATCH, SWATCH])
                .collect(),
            buttons: (0..self.responses.len())
                .map(|index| {
                    [
                        x + PADDING + index as f32 * (button_width + BUTTON_SPACING),
                        button_y,
                        button_width,
                        BUTTON,
                    ]
                })
                .collect(),
        }
    }

    /// The part at `(x, y)` from the corner of a window `size` big: a button, a colour,
    /// or the dialog and all it covers.
    pub fn part_at(&self, (x, y): (f64, f64), size: (u32, u32)) -> Part {
        let layout = self.layout((size.0 as f32, size.1 as f32));
        let (x, y) = (x as f32, y as f32);
        let within = |[left, top, width, height]: &[f32; 4]| {
            (*left..left + width).contains(&x) && (*top..top + height).contains(&y)
        };
        if let Some(index) = layout.swatches.iter().position(within) {
            return Part::Swatch(index);
        }
        layout
            .buttons
            .iter()
            .position(within)
            .map_or(Part::Dialog, Part::Response)
    }
}

fn card_width(width: f32) -> f32 {
    WIDTH.min(width - 2.0 * INSET).max(0.0)
}

/// `text` cut between words into lines no wider than `width` as `measure` measures them;
/// a word wider alone has a line of its own.
pub fn wrap(text: &str, width: f32, measure: impl Fn(&str) -> f32) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        if line.is_empty() {
            line.push_str(word);
            continue;
        }
        let longer = format!("{line} {word}");
        if measure(&longer) > width {
            lines.push(std::mem::replace(&mut line, word.to_owned()));
        } else {
            line = longer;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

impl Painter<'_> {
    /// `dialog` in the middle of a window `height` high, over all else, which it dims.
    pub(super) fn dialog(
        &self,
        quads: &mut Vec<Quad>,
        dialog: &Dialog,
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
        let layout = dialog.layout((self.width / scale, height / scale));
        let place = |[x, y, width, height]: [f32; 4]| {
            [
                (margin + x * scale).round(),
                (margin + y * scale).round(),
                (width * scale).round(),
                (height * scale).round(),
            ]
        };
        let [x, y, width, card_height] = place(layout.card);
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
        let centre = x + width / 2.0;
        let room = (width - 2.0 * PADDING * scale).max(0.0) as u32;
        let heading = Drawing::Heading {
            text: dialog.heading.clone(),
            width: room,
        };
        if let Some(glyph) = draw(&heading) {
            let top = margin + layout.heading * scale;
            quads.push(centred_line(centre, top, glyph, colors.foreground));
        }
        let lines = if layout.tight {
            &[][..]
        } else {
            &dialog.lines[..]
        };
        for (index, line) in lines.iter().enumerate() {
            let label = Drawing::Label {
                text: line.clone(),
                width: room,
                bold: false,
            };
            if let Some(glyph) = draw(&label) {
                let top = margin + (layout.body + index as f32 * dialog.line) * scale;
                quads.push(centred_line(centre, top, glyph, colors.foreground));
            }
        }
        if let Some(field) = &dialog.field {
            let [x, y, width, height] = place(layout.field);
            let ring = FOCUS * scale;
            let radius = FLAT_RADIUS * scale;
            quads.push(rounded(
                x - ring,
                y - ring,
                width + 2.0 * ring,
                height + 2.0 * ring,
                radius + ring,
                colors.focus,
            ));
            quads.push(rounded(x, y, width, height, radius, colors.dialog));
            let color = colors.foreground;
            self.field(quads, field, [x, y, width, height], ("", color), true, draw);
        }
        for (index, rect) in layout.swatches.iter().enumerate() {
            self.swatch(quads, dialog, index, place(*rect));
        }
        for index in 0..dialog.responses.len() {
            let button = place(layout.buttons[index]);
            self.response(quads, dialog, index, button, draw);
        }
    }

    /// The dialog's colour at `index`, at `[x, y, size, size]` in device pixels, ringed
    /// when picked.
    fn swatch(
        &self,
        quads: &mut Vec<Quad>,
        dialog: &Dialog,
        index: usize,
        [x, y, size, _]: [f32; 4],
    ) {
        let (chrome, scale) = (self.chrome, self.scale);
        let colors = chrome.colors();
        if dialog.picked == Some(index) {
            let ring = FOCUS * scale;
            let gap = SWATCH_GAP * scale;
            let outer = size / 2.0 + gap + ring;
            quads.push(rounded(
                x - gap - ring,
                y - gap - ring,
                size + 2.0 * (gap + ring),
                size + 2.0 * (gap + ring),
                outer,
                colors.focus,
            ));
            quads.push(rounded(
                x - gap,
                y - gap,
                size + 2.0 * gap,
                size + 2.0 * gap,
                size / 2.0 + gap,
                colors.dialog,
            ));
        }
        quads.push(rounded(
            x,
            y,
            size,
            size,
            size / 2.0,
            dialog.swatches[index],
        ));
        let part = Part::Swatch(index);
        if chrome.over(part, false) {
            let shade = WARNING_SHADES[usize::from(chrome.over(part, true))];
            quads.push(rounded(x, y, size, size, size / 2.0, shade));
        }
    }

    /// The dialog's button at `index`, at `[x, y, width, height]` in device pixels.
    fn response(
        &self,
        quads: &mut Vec<Quad>,
        dialog: &Dialog,
        index: usize,
        [x, y, width, height]: [f32; 4],
        draw: &mut impl FnMut(&Drawing) -> Option<(u32, GpuGlyph)>,
    ) {
        let (chrome, scale) = (self.chrome, self.scale);
        let colors = chrome.colors();
        let radius = FLAT_RADIUS * scale;
        if dialog.focused == Some(index) {
            let ring = FOCUS * scale;
            quads.push(rounded(
                x - ring,
                y - ring,
                width + 2.0 * ring,
                height + 2.0 * ring,
                radius + ring,
                colors.focus,
            ));
            quads.push(rounded(x, y, width, height, radius, colors.dialog));
        }
        let (fill, text, shades) = match dialog.appearance {
            _ if !dialog.accepts(index) => (colors.button[0], colors.foreground, colors.flat),
            Appearance::Destructive => (colors.destructive, 0xffff_ffff, WARNING_SHADES),
            Appearance::Suggested => (ACCENT, 0xffff_ffff, WARNING_SHADES),
        };
        quads.push(rounded(x, y, width, height, radius, fill));
        let part = Part::Response(index);
        if chrome.over(part, false) {
            let shade = shades[usize::from(chrome.over(part, true))];
            quads.push(rounded(x, y, width, height, radius, shade));
        }
        let label = Drawing::Label {
            text: dialog.responses[index].clone(),
            width: (width - 2.0 * radius).max(0.0) as u32,
            bold: false,
        };
        if let Some(glyph) = draw(&label) {
            quads.push(self.label(x + width / 2.0, y + height / 2.0, glyph, text));
        }
    }
}

/// A line of text centred on `x` whose line starts at `top`.
pub(super) fn centred_line(x: f32, top: f32, (index, glyph): (u32, GpuGlyph), color: u32) -> Quad {
    let (width, _) = halves(glyph.size);
    let (_, y) = offset(glyph);
    entry(
        (x - width as f32 / 2.0).floor(),
        top.floor() + y as f32,
        index,
        glyph,
        color,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_wraps_between_words_to_fit() {
        let measure = |text: &str| text.chars().count() as f32;
        assert_eq!(
            wrap("a program is still running", 12.0, measure),
            ["a program is", "still", "running"]
        );
        assert_eq!(
            wrap("unbreakable word", 4.0, measure),
            ["unbreakable", "word"]
        );
        assert!(wrap("  ", 4.0, measure).is_empty());
    }

    #[test]
    fn the_buttons_take_the_dialog_s_bottom_and_the_rest_covers_the_window() {
        let mut dialog = Dialog::new("Close Tab?", "", ["Cancel", "Close"]);
        dialog.lines = vec!["one".into(), "two".into()];
        dialog.line = 20.0;
        let size = (800, 600);
        let layout = dialog.layout((800.0, 600.0));
        let [x, y, width, height] = layout.buttons[1];
        let inside = (f64::from(x + width / 2.0), f64::from(y + height / 2.0));
        assert_eq!(dialog.part_at(inside, size), Part::Response(1));
        let [x, ..] = layout.buttons[0];
        assert_eq!(
            dialog.part_at((f64::from(x + 1.0), inside.1), size),
            Part::Response(0)
        );
        assert_eq!(dialog.part_at((5.0, 5.0), size), Part::Dialog);
    }

    #[test]
    fn in_the_lowest_window_a_dialog_s_field_and_buttons_stay_in_it() {
        let mut dialog = Dialog::asking("Rename", "", ["Cancel", "Rename"], "name");
        dialog.line = 20.0;
        dialog.lines = vec!["a line".to_owned(), "another".to_owned()];
        dialog.swatches = vec![0; 9];
        let layout = dialog.layout((360.0, 200.0));
        assert!(layout.tight);
        let bottom = |[_, y, _, height]: [f32; 4]| y + height;
        assert!(bottom(layout.buttons[1]) <= 200.0);
        assert!(bottom(layout.field) <= layout.swatches[0][1]);
        assert!(bottom(layout.swatches[0]) <= layout.buttons[0][1]);
        assert!(layout.card[2] <= WIDTH);
    }

    #[test]
    fn colours_sit_in_a_row_between_the_field_and_the_buttons() {
        let mut dialog = Dialog::asking("Rename Session", "", ["Cancel", "Rename"], "");
        dialog.swatches = vec![0xff; 9];
        let size = (800, 600);
        let layout = dialog.layout((800.0, 600.0));
        let [x, y, width, height] = layout.swatches[8];
        let centre = (f64::from(x + width / 2.0), f64::from(y + height / 2.0));
        assert_eq!(dialog.part_at(centre, size), Part::Swatch(8));
        assert!(y > layout.field[1] + layout.field[3]);
        assert!(y + height < layout.buttons[0][1]);
        let [field_x, _, field_width, _] = layout.field;
        assert!((x + width - (field_x + field_width)).abs() < 0.01);
    }
}
