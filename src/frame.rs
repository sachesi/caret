//! A frame of the terminal: what is on screen, copied out while the terminal is locked,
//! then turned into quads for the renderer without holding the lock.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};

use crate::fonts::{self, Bitmap, CellMetrics, GlyphKey, GlyphText};
use std::rc::Rc;

use crate::links::Span;
use crate::palette::Palette;
use crate::renderer::{AtlasFull, Kind, Quads, Renderer};

pub struct ScreenCell {
    pub line: usize,
    pub column: usize,
    pub glyph: Option<GlyphKey>,
    pub fg: Rgb,
    pub bg: Rgb,
    pub underline: Rgb,
    pub flags: Flags,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenCursor {
    pub line: usize,
    pub column: usize,
    pub shape: CursorShape,
    pub wide: bool,
}

/// The selection in lines of the screen, first and last cell included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScreenSelection {
    pub start: Point<usize>,
    pub end: Point<usize>,
    pub block: bool,
}

/// The visible part of the terminal. Only cells with something to draw are kept: a glyph,
/// a background of their own or a line through or under them.
#[derive(Default)]
pub struct Screen {
    pub columns: usize,
    pub lines: usize,
    pub cells: Vec<ScreenCell>,
    pub cursor: Option<ScreenCursor>,
    pub cursor_blinks: bool,
    pub selection: Option<ScreenSelection>,
    pub background: Rgb,
    pub cursor_color: Rgb,
    pub mode: TermMode,
}

impl Screen {
    /// `link` is underlined, as the one a click would open.
    pub fn capture<T: EventListener>(
        &mut self,
        term: &Term<T>,
        palette: &Palette,
        link: Option<&Span>,
    ) {
        let content = term.renderable_content();
        let colors = content.colors;
        let offset = content.display_offset as i32;
        self.columns = term.columns();
        self.lines = term.screen_lines();
        self.mode = content.mode;
        self.background = palette.resolve(Color::Named(NamedColor::Background), colors);
        self.cursor_color = palette.resolve(Color::Named(NamedColor::Cursor), colors);
        self.cursor_blinks = term.cursor_style().blinking;

        let lines = self.lines as i32;
        let cursor_line = content.cursor.point.line.0 + offset;
        self.cursor = (content.cursor.shape != CursorShape::Hidden
            && (0..lines).contains(&cursor_line))
        .then_some(ScreenCursor {
            line: cursor_line as usize,
            column: content.cursor.point.column.0,
            shape: content.cursor.shape,
            wide: false,
        });

        let last_column = self.columns.saturating_sub(1);
        self.selection = content.selection.and_then(|range| {
            let start_line = range.start.line.0 + offset;
            let end_line = range.end.line.0 + offset;
            if end_line < 0 || start_line >= lines {
                return None;
            }
            let start = if start_line < 0 {
                Point::new(
                    0,
                    if range.is_block {
                        range.start.column
                    } else {
                        Column(0)
                    },
                )
            } else {
                Point::new(start_line as usize, range.start.column)
            };
            let end = if end_line >= lines {
                let column = if range.is_block {
                    range.end.column
                } else {
                    Column(last_column)
                };
                Point::new(self.lines - 1, column)
            } else {
                Point::new(end_line as usize, range.end.column)
            };
            Some(ScreenSelection {
                start,
                end,
                block: range.is_block,
            })
        });

        self.cells.clear();
        for indexed in content.display_iter {
            let cell = indexed.cell;
            let line = (indexed.point.line.0 + offset) as usize;
            let column = indexed.point.column.0;
            let mut flags = cell.flags;
            if !flags.intersects(Flags::ALL_UNDERLINES)
                && link.is_some_and(|link| link.covers(indexed.point, cell))
            {
                flags |= Flags::UNDERLINE;
            }

            let mut fg = palette.resolve(cell.fg, colors);
            let mut bg = palette.resolve(cell.bg, colors);
            if flags.contains(Flags::DIM) {
                fg = palette.dim(fg);
            }
            if flags.contains(Flags::INVERSE) {
                std::mem::swap(&mut fg, &mut bg);
            }
            let underline = cell
                .underline_color()
                .map_or(fg, |color| palette.resolve(color, colors));

            let spacer =
                flags.intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
            let text = match cell.zerowidth() {
                _ if spacer || flags.contains(Flags::HIDDEN) => None,
                Some(marks) if !marks.is_empty() => Some(GlyphText::Cluster(
                    std::iter::once(cell.c)
                        .chain(marks.iter().copied())
                        .collect(),
                )),
                _ if cell.c == ' ' || cell.c == '\t' || cell.c == '\0' => None,
                _ => Some(GlyphText::Char(cell.c)),
            };
            let decorated = flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT);
            if let Some(cursor) = &mut self.cursor
                && cursor.line == line
                && cursor.column == column
            {
                cursor.wide = flags.contains(Flags::WIDE_CHAR);
            }
            if text.is_none() && bg == self.background && !decorated {
                continue;
            }
            let mut style = 0;
            if flags.contains(Flags::BOLD) {
                style |= fonts::BOLD;
            }
            if flags.contains(Flags::ITALIC) {
                style |= fonts::ITALIC;
            }
            self.cells.push(ScreenCell {
                line,
                column,
                glyph: text.map(|text| GlyphKey {
                    text,
                    style,
                    wide: flags.contains(Flags::WIDE_CHAR),
                }),
                fg,
                bg,
                underline,
                flags,
            });
        }
    }
}

/// How the frame looks beyond the terminal's own state.
pub struct Look {
    pub selection: Rgb,
    pub focused: bool,
    /// The blinking cursor is in its visible phase.
    pub cursor_on: bool,
    /// Room around the grid, in device pixels.
    pub padding: i32,
    pub scale: f64,
    /// Symbols the font joins are drawn joined.
    pub ligatures: bool,
}

fn rgba(color: Rgb) -> [u8; 4] {
    [color.r, color.g, color.b, 255]
}

/// Fills `quads` in drawing order: backgrounds, the selection and a block cursor, glyphs,
/// lines under and through text, then the other cursors. Glyphs not in the atlas yet come
/// from `rasterize`, which may have none to give yet, and are then left out. With
/// `partial`, so are glyphs the atlas has no room for, instead of failing the frame.
/// `ligatures` divides a run of symbols of one style into the lengths of its glyphs.
#[allow(clippy::too_many_arguments)]
pub fn build(
    screen: &Screen,
    look: &Look,
    metrics: CellMetrics,
    ligatures: &impl Fn(&str, u8) -> Rc<[u8]>,
    rasterize: &mut impl FnMut(&GlyphKey) -> Option<Option<Bitmap>>,
    renderer: &mut Renderer,
    quads: &mut Quads,
    partial: bool,
) -> Result<(), AtlasFull> {
    quads.clear();
    let cell = metrics;
    let x_of = |column: usize| (look.padding + column as i32 * cell.width) as f32;
    let y_of = |line: usize| (look.padding + line as i32 * cell.height) as f32;
    let (width, height) = (cell.width as f32, cell.height as f32);

    // Runs of cells with the same background along a line, as one rectangle each.
    let mut run: Option<(usize, usize, usize, Rgb)> = None;
    let flush = |run: Option<(usize, usize, usize, Rgb)>, quads: &mut Quads| {
        if let Some((line, first, last, bg)) = run {
            let cells = (last - first + 1) as f32;
            quads.rect(
                [x_of(first), y_of(line), cells * width, height],
                rgba(bg),
                Kind::Solid,
            );
        }
    };
    for cell in &screen.cells {
        if cell.bg == screen.background {
            continue;
        }
        match &mut run {
            Some((line, _, last, bg))
                if *line == cell.line && *last + 1 == cell.column && *bg == cell.bg =>
            {
                *last = cell.column;
            }
            _ => {
                flush(run.take(), quads);
                run = Some((cell.line, cell.column, cell.column, cell.bg));
            }
        }
    }
    flush(run, quads);

    if let Some(selection) = screen.selection {
        for line in selection.start.line..=selection.end.line {
            let (first, last) = if selection.block {
                (selection.start.column.0, selection.end.column.0)
            } else {
                (
                    if line == selection.start.line {
                        selection.start.column.0
                    } else {
                        0
                    },
                    if line == selection.end.line {
                        selection.end.column.0
                    } else {
                        screen.columns - 1
                    },
                )
            };
            if last >= first {
                let cells = (last - first + 1) as f32;
                quads.rect(
                    [x_of(first), y_of(line), cells * width, height],
                    rgba(look.selection),
                    Kind::Solid,
                );
            }
        }
    }

    let cursor = screen.cursor;
    let block = cursor
        .filter(|cursor| look.focused && look.cursor_on && cursor.shape == CursorShape::Block);
    if let Some(cursor) = block {
        let cells = if cursor.wide { 2.0 } else { 1.0 };
        quads.rect(
            [
                x_of(cursor.column),
                y_of(cursor.line),
                cells * width,
                height,
            ],
            rgba(screen.cursor_color),
            Kind::Solid,
        );
    }

    let under_cursor = |cell: &ScreenCell| {
        block.is_some_and(|cursor| cursor.line == cell.line && cursor.column == cell.column)
    };
    let mut draw = |key: &GlyphKey, cell: &ScreenCell| {
        let glyph = match renderer.glyph(key) {
            Some(glyph) => glyph,
            None => match rasterize(key) {
                Some(bitmap) => match renderer.insert_glyph(key, bitmap) {
                    Ok(glyph) => glyph,
                    Err(_) if partial => return Ok(()),
                    Err(full) => return Err(full),
                },
                None => return Ok(()),
            },
        };
        let Some(glyph) = glyph else {
            return Ok(());
        };
        let color = if under_cursor(cell) { cell.bg } else { cell.fg };
        quads.push(
            [
                x_of(cell.column) + glyph.left as f32,
                y_of(cell.line) + glyph.top as f32,
                glyph.width as f32,
                glyph.height as f32,
            ],
            [
                glyph.x as f32,
                glyph.y as f32,
                glyph.width as f32,
                glyph.height as f32,
            ],
            rgba(color),
            if glyph.color { Kind::Color } else { Kind::Mask },
        );
        Ok(())
    };
    if look.ligatures {
        // The glyph under a block cursor takes another colour, so it stays its own.
        glyphs(&screen.cells, &under_cursor, ligatures, &mut draw)?;
    } else {
        for cell in &screen.cells {
            if let Some(key) = &cell.glyph {
                draw(key, cell)?;
            }
        }
    }

    for cell in &screen.cells {
        decorate(cell, &metrics, x_of(cell.column), y_of(cell.line), quads);
    }

    if let Some(cursor) = cursor.filter(|_| look.cursor_on || !look.focused) {
        let (x, y) = (x_of(cursor.column), y_of(cursor.line));
        let cells = if cursor.wide { 2.0 } else { 1.0 };
        let color = rgba(screen.cursor_color);
        let line = look.scale.round().max(1.0) as f32;
        match cursor.shape {
            CursorShape::Block | CursorShape::HollowBlock
                if !look.focused || cursor.shape == CursorShape::HollowBlock =>
            {
                let w = cells * width;
                quads.rect([x, y, w, line], color, Kind::Solid);
                quads.rect([x, y + height - line, w, line], color, Kind::Solid);
                quads.rect([x, y, line, height], color, Kind::Solid);
                quads.rect([x + w - line, y, line, height], color, Kind::Solid);
            }
            CursorShape::Beam => {
                let beam = (1.5 * look.scale).round().max(1.0) as f32;
                quads.rect([x, y, beam, height], color, Kind::Solid);
            }
            CursorShape::Underline => {
                let thickness = (2.0 * look.scale).round().max(1.0) as f32;
                quads.rect(
                    [x, y + height - thickness, cells * width, thickness],
                    color,
                    Kind::Solid,
                );
            }
            _ => {}
        }
    }
    Ok(())
}

/// Gives `draw` each glyph of `cells` and the cell it starts in, with symbols the font
/// joins as one glyph: `ligatures` divides a run of symbols of one style into the lengths
/// of its glyphs. Cells `apart` holds for stay glyphs of their own.
fn glyphs<E>(
    cells: &[ScreenCell],
    apart: &impl Fn(&ScreenCell) -> bool,
    ligatures: &impl Fn(&str, u8) -> Rc<[u8]>,
    draw: &mut impl FnMut(&GlyphKey, &ScreenCell) -> Result<(), E>,
) -> Result<(), E> {
    let symbol = |cell: &ScreenCell| match &cell.glyph {
        Some(GlyphKey {
            text: GlyphText::Char(c),
            style,
            wide: false,
        }) if c.is_ascii_punctuation() => Some((*c, *style)),
        _ => None,
    };
    let mut run = String::new();
    let mut index = 0;
    while index < cells.len() {
        let cell = &cells[index];
        let Some(key) = &cell.glyph else {
            index += 1;
            continue;
        };
        // Symbols side by side in one colour and style, which the font may join.
        run.clear();
        if let Some((first, style)) = symbol(cell) {
            run.push(first);
            while let Some(next) = cells.get(index + run.len())
                && next.line == cell.line
                && next.column == cell.column + run.len()
                && next.fg == cell.fg
                && let Some((c, next_style)) = symbol(next)
                && next_style == style
            {
                run.push(c);
            }
        }
        if run.len() < 2 {
            draw(key, cell)?;
            index += 1;
            continue;
        }
        let mut at = index;
        for &length in ligatures(&run, key.style).iter() {
            let part = &cells[at..at + usize::from(length)];
            if length > 1 && !part.iter().any(apart) {
                let joined = GlyphKey {
                    text: GlyphText::Cluster(run[at - index..][..part.len()].into()),
                    style: key.style,
                    wide: false,
                };
                draw(&joined, &part[0])?;
            } else {
                for cell in part {
                    if let Some(key) = &cell.glyph {
                        draw(key, cell)?;
                    }
                }
            }
            at += part.len();
        }
        index = at;
    }
    Ok(())
}

/// Underlines of every style and the line through struck-out text.
fn decorate(cell: &ScreenCell, metrics: &CellMetrics, x: f32, y: f32, quads: &mut Quads) {
    let flags = cell.flags;
    if !flags.intersects(Flags::ALL_UNDERLINES | Flags::STRIKEOUT) {
        return;
    }
    let width = metrics.width as f32;
    let height = metrics.height;
    let thickness = metrics.underline_thickness;
    let color = rgba(cell.underline);
    if flags.contains(Flags::DOUBLE_UNDERLINE) {
        let top = metrics.underline.min(height - 3 * thickness).max(0);
        for offset in [0, 2 * thickness] {
            let line = y + (top + offset) as f32;
            quads.rect([x, line, width, thickness as f32], color, Kind::Solid);
        }
    } else if flags.contains(Flags::UNDERCURL) {
        let band = (3 * thickness).max(3);
        let top = y + (height - band) as f32;
        quads.rect([x, top, width, band as f32], color, Kind::Undercurl);
    } else if flags.intersects(Flags::DOTTED_UNDERLINE | Flags::DASHED_UNDERLINE) {
        let kind = if flags.contains(Flags::DOTTED_UNDERLINE) {
            Kind::Dotted
        } else {
            Kind::Dashed
        };
        let line = y + metrics.underline as f32;
        quads.rect([x, line, width, thickness as f32], color, kind);
    } else if flags.contains(Flags::UNDERLINE) {
        let line = y + metrics.underline as f32;
        quads.rect([x, line, width, thickness as f32], color, Kind::Solid);
    }
    if flags.contains(Flags::STRIKEOUT) {
        let line = y + metrics.strikeout as f32;
        let thickness = metrics.strikeout_thickness as f32;
        quads.rect([x, line, width, thickness], rgba(cell.fg), Kind::Solid);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHITE: Rgb = Rgb {
        r: 255,
        g: 255,
        b: 255,
    };
    const RED: Rgb = Rgb { r: 255, g: 0, b: 0 };

    fn cell(column: usize, c: char) -> ScreenCell {
        ScreenCell {
            line: 0,
            column,
            glyph: (c != ' ').then_some(GlyphKey {
                text: GlyphText::Char(c),
                style: 0,
                wide: false,
            }),
            fg: WHITE,
            bg: Rgb::default(),
            underline: WHITE,
            flags: Flags::empty(),
        }
    }

    fn line(text: &str) -> Vec<ScreenCell> {
        text.chars()
            .enumerate()
            .map(|(column, c)| cell(column, c))
            .collect()
    }

    /// The glyphs of `cells` as text, each with its column, where a font joins `->` and
    /// `!=` and the cell in column `apart` stays its own.
    fn drawn(cells: &[ScreenCell], apart: Option<usize>) -> Vec<(usize, String)> {
        let ligatures = |run: &str, _style: u8| -> Rc<[u8]> {
            let mut parts = Vec::new();
            let mut rest = run;
            while !rest.is_empty() {
                let length = if rest.starts_with("->") || rest.starts_with("!=") {
                    2
                } else {
                    1
                };
                parts.push(length);
                rest = &rest[usize::from(length)..];
            }
            parts.into()
        };
        let mut drawn = Vec::new();
        glyphs(
            cells,
            &|cell: &ScreenCell| Some(cell.column) == apart,
            &ligatures,
            &mut |key: &GlyphKey, cell: &ScreenCell| {
                let text = match &key.text {
                    GlyphText::Char(c) => c.to_string(),
                    GlyphText::Cluster(text) => text.to_string(),
                };
                drawn.push((cell.column, text));
                Ok::<(), ()>(())
            },
        )
        .unwrap();
        drawn
    }

    fn texts(drawn: &[(usize, String)]) -> Vec<(usize, &str)> {
        drawn
            .iter()
            .map(|(column, text)| (*column, text.as_str()))
            .collect()
    }

    #[test]
    fn symbols_the_font_joins_are_one_glyph_in_their_first_cell() {
        let drawn = drawn(&line("a->b !=;"), None);
        assert_eq!(
            texts(&drawn),
            [(0, "a"), (1, "->"), (3, "b"), (5, "!="), (7, ";")]
        );
    }

    #[test]
    fn letters_and_lone_symbols_are_not_asked_about() {
        let asked = std::cell::Cell::new(0);
        let cells = line("ab - c");
        glyphs(
            &cells,
            &|_: &ScreenCell| false,
            &|run: &str, _| {
                asked.set(asked.get() + 1);
                vec![1; run.len()].into()
            },
            &mut |_: &GlyphKey, _: &ScreenCell| Ok::<(), ()>(()),
        )
        .unwrap();
        assert_eq!(asked.get(), 0);
    }

    #[test]
    fn a_cell_kept_apart_breaks_its_ligature() {
        let drawn = drawn(&line("->"), Some(1));
        assert_eq!(texts(&drawn), [(0, "-"), (1, ">")]);
    }

    #[test]
    fn another_colour_style_or_line_ends_a_run() {
        let mut coloured = line("->");
        coloured[1].fg = RED;
        assert_eq!(texts(&drawn(&coloured, None)), [(0, "-"), (1, ">")]);

        let mut bold = line("->");
        bold[1].glyph.as_mut().unwrap().style = fonts::BOLD;
        assert_eq!(texts(&drawn(&bold, None)), [(0, "-"), (1, ">")]);

        let mut wrapped = line("->");
        wrapped[1].line = 1;
        wrapped[1].column = 0;
        assert_eq!(texts(&drawn(&wrapped, None)), [(0, "-"), (0, ">")]);
    }

    #[test]
    fn cells_that_are_not_side_by_side_are_not_joined() {
        // Cells without anything to draw are left out of a screen's cells.
        let mut cells = line("->");
        cells[1].column = 2;
        assert_eq!(texts(&drawn(&cells, None)), [(0, "-"), (2, ">")]);
    }
}
