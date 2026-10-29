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
/// Its buttons' and field's height, and the room between the buttons.
const BUTTON: f32 = 34.0;
const BUTTON_SPACING: f32 = 12.0;
/// The ring around the button the keys press: its width, and how far it stands out.
const FOCUS: f32 = 2.0;
/// What shades a coloured button, hovered and then pressed: its white text lightens
/// it, as text of the window's colour shades other buttons.
const WARNING_SHADES: [u32; 2] = [0xffff_ff1a, 0x0000_0633];

#[derive(Debug)]
pub struct Dialog {
    pub heading: String,
    pub body: String,
    /// The buttons' labels: the first declines, the second does what was asked.
    pub responses: [String; 2],
    pub appearance: Appearance,
    /// The button Enter presses; none while the field has the keys.
    pub focused: Option<usize>,
    /// The line of text it asks for.
    pub field: Option<Field>,
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
    heading: f32,
    body: f32,
    field: [f32; 4],
    buttons: [[f32; 4]; 2],
}

impl Dialog {
    /// A dialog that asks to confirm, Enter declining.
    pub fn new(heading: &str, body: &str, responses: [&str; 2]) -> Self {
        Self {
            heading: heading.to_owned(),
            body: body.to_owned(),
            responses: responses.map(str::to_owned),
            appearance: Appearance::Destructive,
            focused: Some(0),
            field: None,
            lines: Vec::new(),
            line: 0.0,
        }
    }

    /// A dialog that asks for a line of text, starting as `text`, Enter accepting it.
    pub fn asking(heading: &str, body: &str, responses: [&str; 2], text: &str) -> Self {
        Self {
            appearance: Appearance::Suggested,
            focused: None,
            field: Some(Field::holding(text)),
            ..Self::new(heading, body, responses)
        }
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
        let card_height = PADDING + heading + SPACING + body + field + PADDING + BUTTON + PADDING;
        let (x, y) = (
            (width - card_width) / 2.0,
            ((height - card_height) / 2.0).max(0.0),
        );
        let button_width = (card_width - 2.0 * PADDING - BUTTON_SPACING) / 2.0;
        let button_y = y + card_height - PADDING - BUTTON;
        Layout {
            card: [x, y, card_width, card_height],
            heading: y + PADDING,
            body: y + PADDING + heading + SPACING,
            field: [
                x + PADDING,
                y + PADDING + heading + SPACING + body + PADDING / 2.0,
                card_width - 2.0 * PADDING,
                BUTTON,
            ],
            buttons: [0.0, 1.0].map(|index| {
                [
                    x + PADDING + index * (button_width + BUTTON_SPACING),
                    button_y,
                    button_width,
                    BUTTON,
                ]
            }),
        }
    }

    /// The part at `(x, y)` from the corner of a window `size` big: a button, or the
    /// dialog and all it covers.
    pub fn part_at(&self, (x, y): (f64, f64), size: (u32, u32)) -> Part {
        let layout = self.layout((size.0 as f32, size.1 as f32));
        let (x, y) = (x as f32, y as f32);
        layout
            .buttons
            .iter()
            .position(|[left, top, width, height]| {
                (*left..left + width).contains(&x) && (*top..top + height).contains(&y)
            })
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
        for (index, line) in dialog.lines.iter().enumerate() {
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
        for index in 0..dialog.responses.len() {
            let button = place(layout.buttons[index]);
            self.response(quads, dialog, index, button, draw);
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
            _ if index == 0 => (colors.button[0], colors.foreground, colors.flat),
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
fn centred_line(x: f32, top: f32, (index, glyph): (u32, GpuGlyph), color: u32) -> Quad {
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
        assert!(layout.card[2] <= WIDTH);
    }
}
