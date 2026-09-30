//! A frame of the terminal: what is on screen, copied out while the terminal is locked,
//! then turned into quads for the renderer without holding the lock.

use alacritty_terminal::event::EventListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Point};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, NamedColor, Rgb};

use crate::fonts::{self, Bitmap, CellMetrics, GlyphKey, GlyphText};
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
}

fn rgba(color: Rgb) -> [u8; 4] {
    [color.r, color.g, color.b, 255]
}

/// Fills `quads` in drawing order: backgrounds, the selection and a block cursor, glyphs,
/// lines under and through text, then the other cursors. Glyphs not in the atlas yet come
/// from `rasterize`, which may have none to give yet, and are then left out. With
/// `partial`, so are glyphs the atlas has no room for, instead of failing the frame.
pub fn build(
    screen: &Screen,
    look: &Look,
    metrics: CellMetrics,
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

    for cell in &screen.cells {
        let Some(key) = &cell.glyph else {
            continue;
        };
        let glyph = match renderer.glyph(key) {
            Some(glyph) => glyph,
            None => match rasterize(key) {
                Some(bitmap) => match renderer.insert_glyph(key, bitmap) {
                    Ok(glyph) => glyph,
                    Err(_) if partial => continue,
                    Err(full) => return Err(full),
                },
                None => continue,
            },
        };
        let Some(glyph) = glyph else {
            continue;
        };
        let under_cursor =
            block.is_some_and(|cursor| cursor.line == cell.line && cursor.column == cell.column);
        let color = if under_cursor { cell.bg } else { cell.fg };
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
