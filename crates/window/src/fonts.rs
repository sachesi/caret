//! Fonts found, shaped and rasterised in Rust: the faces of the terminal's family, the
//! size of a cell, and each glyph's pixels.

use std::collections::HashMap;
use std::sync::Arc;

use caret_protocol::{BOLD, Glyph, GlyphText, ITALIC};
use swash::FontRef;
use swash::scale::image::{Content, Image};
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Format, Transform, Vector};

/// Families tried in order when none is set or the one set is not installed.
const FAMILIES: &[&str] = &[
    "Adwaita Mono",
    "Source Code Pro",
    "DejaVu Sans Mono",
    "Noto Sans Mono",
    "Liberation Mono",
    "Cascadia Mono",
    "JetBrains Mono",
    "Fira Mono",
    "Ubuntu Mono",
];

/// Families tried first for a character the terminal's font lacks, before every other
/// face installed.
const FALLBACKS: &[&str] = &[
    "Noto Color Emoji",
    "Symbols Nerd Font Mono",
    "Noto Sans Symbols 2",
    "Noto Sans Symbols",
    "Noto Sans Mono CJK SC",
    "Noto Sans CJK SC",
    "DejaVu Sans",
    "Noto Sans",
];

/// Families tried in order for the window's own text, before any sans-serif face.
const LABEL_FAMILIES: &[&str] = &["Adwaita Sans", "Cantarell", "Noto Sans", "DejaVu Sans"];

/// The size of the window's own text, and of a heading against the rest.
const LABEL_POINTS: f32 = 11.0;
pub const HEADING: f32 = 1.36;

/// How far a face without italics is slanted to stand in for one, as a tangent.
const SLANT: f32 = 0.2;

#[derive(Debug, thiserror::Error)]
pub enum FontError {
    #[error("no monospaced font is installed")]
    NoFont,
}

/// Sizes in device pixels, measured from the cell's top.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CellMetrics {
    pub width: i32,
    pub height: i32,
    pub baseline: i32,
    pub underline: i32,
    pub underline_thickness: i32,
    pub strikeout: i32,
    pub strikeout_thickness: i32,
}

/// A glyph's pixels and where they go, from the top left of its cell or line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Raster {
    pub width: u32,
    pub height: u32,
    pub left: i32,
    pub top: i32,
    /// Premultiplied RGBA.
    pub pixels: Vec<u8>,
    /// Coloured, an emoji; otherwise white, a coverage mask the text colour is applied to.
    pub color: bool,
}

/// A glyph of shaped text, its place in pixels from where the text starts.
struct Shaped {
    id: u16,
    x: f32,
    y: f32,
    /// Where the pen was before it, and how far it moves the pen.
    pen: f32,
    advance: f32,
}

#[derive(Clone)]
struct Face {
    data: Arc<dyn AsRef<[u8]> + Send + Sync>,
    index: u32,
    /// Drawn heavier, standing in for a bold face the family lacks.
    embolden: bool,
    /// The weight a variable face is drawn at, from its own axis.
    weight: Option<f32>,
    /// Slanted, standing in for an italic face the family lacks.
    slant: bool,
}

impl Face {
    fn bytes(&self) -> &[u8] {
        (*self.data).as_ref()
    }

    fn font(&self) -> Option<FontRef<'_>> {
        FontRef::from_index(self.bytes(), self.index as usize)
    }

    fn has(&self, c: char) -> bool {
        self.font().is_some_and(|font| font.charmap().map(c) != 0)
    }

    /// Draws the face at `weight`: from its weight axis if it has one that reaches it,
    /// else heavier than it is. Drawing heavier would garble the overlapping outlines
    /// variable faces have.
    fn weigh(&mut self, weight: fontdb::Weight) {
        let weight = f32::from(weight.0);
        let reaches = self.font().is_some_and(|font| {
            font.variations()
                .any(|axis| axis.tag() == WEIGHT_AXIS && axis.max_value() >= weight)
        });
        if reaches {
            self.weight = Some(weight);
        } else {
            self.embolden = true;
        }
    }
}

const WEIGHT_AXIS: swash::Tag = swash::tag_from_bytes(b"wght");

pub struct Fonts {
    database: fontdb::Database,
    /// Regular, bold, italic, bold italic: indexed by style.
    styles: [Face; 4],
    /// The size in pixels to the em.
    size: f32,
    cell: CellMetrics,
    context: ScaleContext,
    fallbacks: HashMap<char, Option<Face>>,
    /// The sans-serif faces of the window's own text, regular and bold, their size in
    /// pixels to the em, and their line's ascent and height.
    labels: [Option<Face>; 2],
    label_size: f32,
    label_ascent: i32,
    label_height: i32,
}

impl Fonts {
    /// The faces of `family`, or of the first installed family of those Caret prefers, at
    /// `points` on a screen of `scale` device pixels to the logical one.
    ///
    /// # Errors
    ///
    /// When no monospaced font is installed.
    pub fn new(family: Option<&str>, points: f32, scale: f32) -> Result<Self, FontError> {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        let family = choose_family(&database, family).ok_or(FontError::NoFont)?;
        let mut face = |weight, style| face(&mut database, &family, weight, style);
        let styles = [
            face(fontdb::Weight::NORMAL, fontdb::Style::Normal),
            face(fontdb::Weight::BOLD, fontdb::Style::Normal),
            face(fontdb::Weight::NORMAL, fontdb::Style::Italic),
            face(fontdb::Weight::BOLD, fontdb::Style::Italic),
        ];
        let [Some(regular), Some(bold), Some(italic), Some(bold_italic)] = styles else {
            return Err(FontError::NoFont);
        };
        // CSS pixels: 96 to the inch, 72 points.
        let size = points * 96.0 / 72.0 * scale;
        let cell = measure(&regular, size).ok_or(FontError::NoFont)?;
        let label_size = LABEL_POINTS * 96.0 / 72.0 * scale;
        let labels = [
            label_face(&mut database, fontdb::Weight::NORMAL),
            label_face(&mut database, fontdb::Weight::BOLD),
        ];
        let (label_ascent, label_height) = labels[1]
            .as_ref()
            .and_then(|face| {
                let metrics = face.font()?.metrics(&[]).scale(label_size);
                let ascent = metrics.ascent.ceil() as i32;
                Some((ascent, ascent + metrics.descent.ceil() as i32))
            })
            .unwrap_or_default();
        Ok(Self {
            database,
            styles: [regular, bold, italic, bold_italic],
            size,
            cell,
            context: ScaleContext::new(),
            fallbacks: HashMap::new(),
            labels,
            label_size,
            label_ascent,
            label_height,
        })
    }

    pub fn cell(&self) -> CellMetrics {
        self.cell
    }

    /// The height of a line of the window's own text, in pixels.
    pub fn label_height(&self) -> i32 {
        self.label_height
    }

    /// How far `text` moves the pen in the window's own face, bold or not, in pixels.
    pub fn label_advance(&self, text: &str, bold: bool) -> f32 {
        self.labels[usize::from(bold)]
            .as_ref()
            .and_then(|face| shape(face, text, self.label_size))
            .map_or(0.0, |(_, advance)| advance)
    }

    /// `text` in the window's own face, bold or not, from the top of its line, cut short
    /// with an ellipsis to fit `width` pixels; none if nothing in it draws.
    pub fn label(&mut self, text: &str, width: u32, bold: bool) -> Option<Raster> {
        let face = self.labels[usize::from(bold)].clone()?;
        self.line(&face, text, width, self.label_size, self.label_ascent)
    }

    /// `text` as a heading: a bold label [`HEADING`] times as large.
    pub fn heading(&mut self, text: &str, width: u32) -> Option<Raster> {
        let face = self.labels[1].clone()?;
        let ascent = (self.label_ascent as f32 * HEADING).ceil() as i32;
        self.line(&face, text, width, self.label_size * HEADING, ascent)
    }

    fn line(
        &mut self,
        face: &Face,
        text: &str,
        width: u32,
        size: f32,
        ascent: i32,
    ) -> Option<Raster> {
        let (mut glyphs, advance) = shape(face, text, size)?;
        if advance > width as f32 {
            let (dots, dots_advance) = shape(face, "\u{2026}", size)?;
            let room = width as f32 - dots_advance;
            let kept = glyphs
                .iter()
                .position(|glyph| glyph.pen + glyph.advance > room)
                .unwrap_or(glyphs.len());
            glyphs.truncate(kept);
            let pen = glyphs.last().map_or(0.0, |glyph| glyph.pen + glyph.advance);
            glyphs.extend(dots.into_iter().map(|dot| Shaped {
                x: dot.x + pen,
                pen: dot.pen + pen,
                ..dot
            }));
        }
        let placed = self.render(face, size, &glyphs, ascent);
        compose(&placed)
    }

    /// The pixels of `glyph`, or none for one that draws nothing, such as a space.
    pub fn raster(&mut self, glyph: &Glyph) -> Option<Raster> {
        let text = match &glyph.text {
            GlyphText::Char(c) => c.to_string(),
            GlyphText::Cluster(text) => text.to_string(),
        };
        let cells = if glyph.wide { 2 } else { 1 };
        if let GlyphText::Char(c) = glyph.text
            && let Some(raster) = self.drawn(c, cells)
        {
            return Some(raster);
        }
        let first = text.chars().next()?;
        let style = usize::from(glyph.style & (BOLD | ITALIC));
        let face = self.styles[style].clone();
        let face = if face.has(first) {
            face
        } else {
            let mut fallback = self.fallback(first)?;
            if glyph.style & BOLD != 0 {
                fallback.weigh(fontdb::Weight::BOLD);
            }
            fallback.slant = glyph.style & ITALIC != 0;
            fallback
        };
        let placed = self.shape_and_render(&face, &text)?;
        let mut raster = compose(&placed)?;
        // Colour bitmaps come at the size they were drawn at; they are fitted to their
        // cells, keeping their proportions.
        if raster.color {
            let room = (self.cell.width * cells) as f32;
            let fit =
                (room / raster.width as f32).min(self.cell.height as f32 / raster.height as f32);
            if fit < 1.0 {
                raster = shrink(&raster, fit);
                raster.left = (self.cell.width * cells - raster.width as i32) / 2;
                raster.top = (self.cell.height - raster.height as i32) / 2;
            }
        }
        Some(raster)
    }

    /// A box-drawing or block character, drawn to fill its cells.
    fn drawn(&self, c: char, cells: i32) -> Option<Raster> {
        let width = self.cell.width * cells;
        let light = (self.cell.width / 8)
            .max(self.cell.underline_thickness)
            .max(1);
        let cover = crate::box_drawing::coverage(
            c,
            width.unsigned_abs(),
            self.cell.height.unsigned_abs(),
            light.unsigned_abs(),
        )?;
        Some(Raster {
            width: width.unsigned_abs(),
            height: self.cell.height.unsigned_abs(),
            left: 0,
            top: 0,
            pixels: cover.iter().flat_map(|&a| [a, a, a, a]).collect(),
            color: false,
        })
    }

    /// Each glyph `text` shapes to in `face`, rendered and placed against the baseline.
    fn shape_and_render(&mut self, face: &Face, text: &str) -> Option<Vec<(i32, i32, Image)>> {
        let (glyphs, _) = shape(face, text, self.size)?;
        Some(self.render(face, self.size, &glyphs, self.cell.baseline))
    }

    /// `glyphs` of `face` rendered at `size`, placed against a baseline `baseline` pixels
    /// down.
    fn render(
        &mut self,
        face: &Face,
        size: f32,
        glyphs: &[Shaped],
        baseline: i32,
    ) -> Vec<(i32, i32, Image)> {
        let Some(font) = face.font() else {
            return Vec::new();
        };
        let mut scaler = self
            .context
            .builder(font)
            .size(size)
            .hint(true)
            .variations(face.weight.map(|weight| ("wght", weight)))
            .build();
        let mut render = Render::new(&[
            Source::ColorOutline(0),
            Source::ColorBitmap(StrikeWith::BestFit),
            Source::Outline,
        ]);
        render.format(Format::Alpha);
        if face.embolden {
            render.embolden(size / 24.0);
        }
        if face.slant {
            render.transform(Some(Transform::skew(
                swash::zeno::Angle::from_radians(0.0),
                swash::zeno::Angle::from_radians(-SLANT.atan()),
            )));
        }
        let mut placed = Vec::new();
        for glyph in glyphs {
            let whole = glyph.x.floor();
            render.offset(Vector::new(glyph.x - whole, 0.0));
            let Some(image) = render.render(&mut scaler, glyph.id) else {
                continue;
            };
            if image.placement.width == 0 || image.placement.height == 0 {
                continue;
            }
            let left = whole as i32 + image.placement.left;
            let top = baseline - glyph.y.round() as i32 - image.placement.top;
            placed.push((left, top, image));
        }
        placed
    }

    /// A face with `c`, for a character the terminal's font lacks.
    fn fallback(&mut self, c: char) -> Option<Face> {
        if let Some(found) = self.fallbacks.get(&c) {
            return found.clone();
        }
        let regular = |info: &fontdb::FaceInfo| {
            info.style == fontdb::Style::Normal && info.weight == fontdb::Weight::NORMAL
        };
        let named = FALLBACKS.iter().find_map(|family| {
            self.database.faces().find(|info| {
                regular(info)
                    && info
                        .families
                        .iter()
                        .any(|(name, _)| name.eq_ignore_ascii_case(family))
                    && face_has(&self.database, info.id, c)
            })
        });
        let id = named
            .or_else(|| {
                self.database
                    .faces()
                    .find(|info| regular(info) && face_has(&self.database, info.id, c))
            })
            .map(|info| info.id);
        let found = id.and_then(|id| shared(&mut self.database, id));
        self.fallbacks.insert(c, found.clone());
        found
    }
}

/// `text` shaped in `face` at `size` pixels to the em, and how far it moves the pen.
fn shape(face: &Face, text: &str, size: f32) -> Option<(Vec<Shaped>, f32)> {
    let mut shaper = rustybuzz::Face::from_slice(face.bytes(), face.index)?;
    if let Some(weight) = face.weight {
        shaper.set_variations(&[rustybuzz::Variation {
            tag: rustybuzz::ttf_parser::Tag::from_bytes(b"wght"),
            value: weight,
        }]);
    }
    let units = f32::from(u16::try_from(shaper.units_per_em()).ok()?);
    let to_pixels = size / units;
    let mut buffer = rustybuzz::UnicodeBuffer::new();
    buffer.push_str(text);
    let shaped = rustybuzz::shape(&shaper, &[], buffer);
    let mut pen = 0.0f32;
    let mut glyphs = Vec::new();
    for (info, position) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
        let advance = position.x_advance as f32 * to_pixels;
        if let Ok(id) = u16::try_from(info.glyph_id) {
            glyphs.push(Shaped {
                id,
                x: pen + position.x_offset as f32 * to_pixels,
                y: position.y_offset as f32 * to_pixels,
                pen,
                advance,
            });
        }
        pen += advance;
    }
    Some((glyphs, pen))
}

/// The face at `weight` of the first of [`LABEL_FAMILIES`] installed, else of the
/// sans-serif family.
fn label_face(database: &mut fontdb::Database, weight: fontdb::Weight) -> Option<Face> {
    let mut families: Vec<fontdb::Family> = LABEL_FAMILIES
        .iter()
        .map(|&name| fontdb::Family::Name(name))
        .collect();
    families.push(fontdb::Family::SansSerif);
    let id = database.query(&fontdb::Query {
        families: &families,
        weight,
        style: fontdb::Style::Normal,
        stretch: fontdb::Stretch::Normal,
    })?;
    let light = database.face(id)?.weight < fontdb::Weight::SEMIBOLD;
    let mut face = shared(database, id)?;
    if light && weight >= fontdb::Weight::SEMIBOLD {
        face.weigh(weight);
    }
    Some(face)
}

/// The family to use: the one asked for if it is installed, else the first of
/// [`FAMILIES`] that is, else the first monospaced one.
fn choose_family(database: &fontdb::Database, wanted: Option<&str>) -> Option<String> {
    let installed = |name: &str| {
        database.faces().any(|face| {
            face.families
                .iter()
                .any(|(family, _)| family.eq_ignore_ascii_case(name))
        })
    };
    wanted
        .into_iter()
        .chain(FAMILIES.iter().copied())
        .find(|name| installed(name))
        .map(str::to_owned)
        .or_else(|| {
            let face = database.faces().find(|face| face.monospaced)?;
            Some(face.families.first()?.0.clone())
        })
}

/// The face of `family` closest to `weight` and `style`, marked to be drawn heavier or
/// slanted where the family has no face that is.
fn face(
    database: &mut fontdb::Database,
    family: &str,
    weight: fontdb::Weight,
    style: fontdb::Style,
) -> Option<Face> {
    let id = database.query(&fontdb::Query {
        families: &[fontdb::Family::Name(family)],
        weight,
        style,
        stretch: fontdb::Stretch::Normal,
    })?;
    let info = database.face(id)?;
    if !info
        .families
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(family))
    {
        return None;
    }
    let embolden = weight >= fontdb::Weight::SEMIBOLD && info.weight < fontdb::Weight::SEMIBOLD;
    let slant = style != fontdb::Style::Normal && info.style == fontdb::Style::Normal;
    let mut face = shared(database, id)?;
    if embolden {
        face.weigh(weight);
    }
    face.slant = slant;
    Some(face)
}

/// The face `id`, its file mapped and shared with the other faces in it.
#[expect(unsafe_code, reason = "fonts are mapped from their files")]
fn shared(database: &mut fontdb::Database, id: fontdb::ID) -> Option<Face> {
    // SAFETY: a font file rewritten while it is mapped can crash the program reading
    // it, a risk every program that maps fonts takes; nothing here writes to it.
    let (data, index) = unsafe { database.make_shared_face_data(id) }?;
    Some(Face {
        data,
        index,
        embolden: false,
        weight: None,
        slant: false,
    })
}

/// Whether face `id` has a glyph for `c`, read without keeping its file mapped.
fn face_has(database: &fontdb::Database, id: fontdb::ID, c: char) -> bool {
    database
        .with_face_data(id, |data, index| {
            FontRef::from_index(data, index as usize).is_some_and(|font| font.charmap().map(c) != 0)
        })
        .unwrap_or(false)
}

fn measure(face: &Face, size: f32) -> Option<CellMetrics> {
    let font = face.font()?;
    let metrics = font.metrics(&[]).scale(size);
    let advances = font.glyph_metrics(&[]).scale(size);
    let width = advances
        .advance_width(font.charmap().map('M'))
        .round()
        .max(1.0) as i32;
    let ascent = metrics.ascent.ceil() as i32;
    let descent = metrics.descent.ceil() as i32;
    let height = (ascent + descent).max(1);
    let thickness = metrics.stroke_size.round().max(1.0) as i32;
    // Offsets are above the baseline, to the top edge of the stroke.
    let underline = (ascent - metrics.underline_offset.round() as i32).min(height - thickness);
    let strikeout = ascent - metrics.strikeout_offset.round() as i32;
    Some(CellMetrics {
        width,
        height,
        baseline: ascent,
        underline,
        underline_thickness: thickness,
        strikeout,
        strikeout_thickness: thickness,
    })
}

/// The glyph images of one cell drawn into one, colour over coverage.
fn compose(placed: &[(i32, i32, Image)]) -> Option<Raster> {
    let left = placed.iter().map(|(x, _, _)| *x).min()?;
    let top = placed.iter().map(|(_, y, _)| *y).min()?;
    let right = placed
        .iter()
        .map(|(x, _, image)| x + image.placement.width as i32)
        .max()?;
    let bottom = placed
        .iter()
        .map(|(_, y, image)| y + image.placement.height as i32)
        .max()?;
    let (width, height) = ((right - left) as u32, (bottom - top) as u32);
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    let mut color = false;
    for (x, y, image) in placed {
        let (image_width, image_height) = (image.placement.width, image.placement.height);
        for row in 0..image_height {
            for column in 0..image_width {
                let source = (row * image_width + column) as usize;
                let rgba = match image.content {
                    Content::Mask => {
                        let a = image.data[source];
                        [a, a, a, a]
                    }
                    Content::Color => {
                        color = true;
                        let [r, g, b, a] = image.data[source * 4..source * 4 + 4] else {
                            continue;
                        };
                        let premultiply = |c: u8| (u16::from(c) * u16::from(a) / 255) as u8;
                        [premultiply(r), premultiply(g), premultiply(b), a]
                    }
                    Content::SubpixelMask => {
                        let a = image.data[source * 4 + 3];
                        [a, a, a, a]
                    }
                };
                let target_x = (x - left) as u32 + column;
                let target_y = (y - top) as u32 + row;
                let target = ((target_y * width + target_x) * 4) as usize;
                over(&mut pixels[target..target + 4], rgba);
            }
        }
    }
    Some(Raster {
        width,
        height,
        left,
        top,
        pixels,
        color,
    })
}

/// Premultiplied `source` drawn over `target`.
fn over(target: &mut [u8], source: [u8; 4]) {
    let keep = 255 - u16::from(source[3]);
    for (target, source) in target.iter_mut().zip(source) {
        *target = (u16::from(source) + u16::from(*target) * keep / 255).min(255) as u8;
    }
}

/// `raster` scaled down by `factor`, each pixel the average of those it covers.
fn shrink(raster: &Raster, factor: f32) -> Raster {
    let width = ((raster.width as f32 * factor).round() as u32).max(1);
    let height = ((raster.height as f32 * factor).round() as u32).max(1);
    let mut pixels = vec![0u8; (width * height * 4) as usize];
    for y in 0..height {
        let (y0, y1) = span(y, height, raster.height);
        for x in 0..width {
            let (x0, x1) = span(x, width, raster.width);
            let mut sum = [0u32; 4];
            for source_y in y0..y1 {
                for source_x in x0..x1 {
                    let source = ((source_y * raster.width + source_x) * 4) as usize;
                    for (total, value) in sum.iter_mut().zip(&raster.pixels[source..source + 4]) {
                        *total += u32::from(*value);
                    }
                }
            }
            let count = (y1 - y0) * (x1 - x0);
            let target = ((y * width + x) * 4) as usize;
            for (pixel, total) in pixels[target..target + 4].iter_mut().zip(sum) {
                *pixel = (total / count.max(1)) as u8;
            }
        }
    }
    Raster {
        width,
        height,
        left: raster.left,
        top: raster.top,
        pixels,
        color: raster.color,
    }
}

/// The source pixels, first and past the last, that target pixel `index` of `count`
/// covers out of `source` pixels.
fn span(index: u32, count: u32, source: u32) -> (u32, u32) {
    let start = index * source / count;
    let end = ((index + 1) * source / count).max(start + 1).min(source);
    (start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coverage_drawn_over_colour_adds_up_and_saturates() {
        let mut pixel = [100, 0, 0, 100];
        over(&mut pixel, [255, 255, 255, 255]);
        assert_eq!(pixel, [255, 255, 255, 255]);
        let mut pixel = [0, 0, 0, 0];
        over(&mut pixel, [64, 64, 64, 64]);
        assert_eq!(pixel, [64, 64, 64, 64]);
    }

    #[test]
    fn shrinking_averages_the_pixels_each_one_covers() {
        let raster = Raster {
            width: 2,
            height: 2,
            left: 0,
            top: 0,
            pixels: [
                [255, 0, 0, 255],
                [0, 0, 0, 0],
                [0, 0, 0, 0],
                [255, 0, 0, 255],
            ]
            .concat(),
            color: true,
        };
        let small = shrink(&raster, 0.5);
        assert_eq!((small.width, small.height), (1, 1));
        assert_eq!(small.pixels, [127, 0, 0, 127]);
    }

    #[test]
    fn a_cell_holds_its_letters_where_a_monospaced_font_is_installed() {
        let Ok(mut fonts) = Fonts::new(None, 11.0, 1.0) else {
            return;
        };
        let cell = fonts.cell();
        assert!(cell.width > 3 && cell.height > cell.width);
        assert!(cell.underline < cell.height && cell.strikeout < cell.baseline);
        let letter = |c| Glyph {
            text: GlyphText::Char(c),
            style: 0,
            wide: false,
        };
        let m = fonts.raster(&letter('M')).expect("M has ink");
        assert!(!m.color && m.left >= 0 && m.top >= 0);
        assert!(m.width as i32 <= cell.width + 1 && m.top + m.height as i32 <= cell.baseline + 1);
        assert_eq!(fonts.raster(&letter(' ')), None);
    }

    #[test]
    fn a_variable_face_is_made_bold_by_its_weight_axis() {
        let mut database = fontdb::Database::new();
        database.load_system_fonts();
        let bold = face(
            &mut database,
            "Adwaita Sans",
            fontdb::Weight::BOLD,
            fontdb::Style::Normal,
        );
        let Some(bold) = bold else {
            return;
        };
        assert_eq!((bold.weight, bold.embolden), (Some(700.0), false));
    }
}
