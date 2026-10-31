//! Fonts found, shaped and rasterised in Rust: the faces of the terminal's family, the
//! size of a cell, and each glyph's pixels.

use std::cell::RefCell;
use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::ops::Range;
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use caret_protocol::{BOLD, Glyph, GlyphText, ITALIC};
use harfrust::{
    Direction, Feature, Script, ShapeOptions, ShapePlan, ShaperData, ShaperInstance, Tag,
    UnicodeBuffer, Variation,
};
use swash::FontRef;
use swash::scale::image::{Content, Image};
use swash::scale::{Render, ScaleContext, Source, StrikeWith};
use swash::zeno::{Format, Transform, Vector};

use crate::config::FontChoice;

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

/// How tall a line of the window's own text is, against its ascent.
const LINE_TO_ASCENT: f32 = 1.25;

/// Families tried in order for the window's own text, before any sans-serif face.
const LABEL_FAMILIES: &[&str] = &["Adwaita Sans", "Cantarell", "Noto Sans", "DejaVu Sans"];

/// The size of the window's own text, and of a heading against the rest.
const LABEL_POINTS: f32 = 11.0;
pub const HEADING: f32 = 1.36;

/// How far a face without italics is slanted to stand in for one, as a tangent.
const SLANT: f32 = 0.2;

/// Symbols in a row beyond this are left apart: a ruler of dashes as one glyph for each
/// of its lengths would fill the atlas.
const LONGEST_LIGATURE: usize = 12;

/// Runs remembered before the table is emptied.
const LIGATURE_RUNS: usize = 4096;

/// Label advances kept, before they are measured anew.
const LABEL_ADVANCES: usize = 1024;

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
    /// The byte of the text it starts at.
    cluster: usize,
    x: f32,
    y: f32,
    /// Where the pen was before it, and how far it moves the pen.
    pen: f32,
    advance: f32,
}

/// Part of a line of text, shaped in the face that draws it.
type FaceRun = (Face, Vec<Shaped>);

#[derive(Clone)]
struct Face {
    data: Arc<dyn AsRef<[u8]> + Send + Sync>,
    index: u32,
    /// Names the face to the scaler's cache of hinting, which otherwise takes each
    /// [`FontRef`] made for a glyph for a face it has not seen and sets hinting up again.
    key: u64,
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

    /// The face as the shaper reads it.
    fn tables(&self) -> Option<harfrust::FontRef<'_>> {
        harfrust::FontRef::from_index(self.bytes(), self.index).ok()
    }

    fn has(&self, c: char) -> bool {
        self.font().is_some_and(|font| font.charmap().map(c) != 0)
    }

    /// Whether it draws glyphs in colours of its own.
    fn colored(&self) -> bool {
        colored(self.bytes(), self.index)
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
    /// Regular, bold, italic, bold italic: indexed by style.
    styles: [Face; 4],
    /// The size in pixels to the em.
    size: f32,
    cell: CellMetrics,
    context: ScaleContext,
    fallbacks: RefCell<Fallbacks>,
    /// The OpenType features the terminal's text is shaped with.
    features: Vec<Feature>,
    ligatures: bool,
    /// How runs of symbols divide into ligatures, by style.
    divided: HashMap<(Box<str>, u8), Rc<[u8]>>,
    /// What shaping takes from each face, at each weight it is drawn at.
    shapings: RefCell<HashMap<(u64, Option<u32>), Shaping>>,
    /// How far texts move the pen in the window's own face, regular and bold, as the
    /// window measures its menus and dialogs again for every frame.
    advances: RefCell<[HashMap<Box<str>, f32>; 2]>,
    /// The sans-serif faces of the window's own text, regular and bold, their size in
    /// pixels to the em, and their line's ascent and height.
    labels: [Option<Face>; 2],
    label_size: f32,
    label_ascent: i32,
    label_height: i32,
}

/// Where a character a face lacks is drawn from, among all the fonts installed.
struct Fallbacks {
    database: fontdb::Database,
    /// The face found for each character, and whether it had to be of one colour.
    faces: HashMap<(char, bool), Option<Face>>,
    /// The faces of the families tried first for a missing character, in their order,
    /// once looked up; then the faces found among all others before, tried before all
    /// are searched again; and the faces mapped for missing characters.
    preferred: Option<Vec<fontdb::ID>>,
    found: Vec<fontdb::ID>,
    mapped: HashMap<fontdb::ID, Face>,
}

impl Fallbacks {
    /// A face with `c`; one drawing it in the text's own colour if `plain`.
    fn face(&mut self, c: char, plain: bool) -> Option<Face> {
        if let Some(found) = self.faces.get(&(c, plain)) {
            return found.clone();
        }
        let preferred = self
            .preferred
            .take()
            .unwrap_or_else(|| preferred_faces(&self.database));
        let found = std::mem::take(&mut self.found);
        let known = preferred.iter().chain(&found).find_map(|&id| {
            self.mapped(id)
                .filter(|face| face.has(c) && !(plain && face.colored()))
        });
        self.preferred = Some(preferred);
        self.found = found;
        let found = known.or_else(|| {
            let id = self
                .database
                .faces()
                .find(|info| regular(info) && face_has(&self.database, info.id, c, plain))?
                .id;
            self.found.push(id);
            self.mapped(id)
        });
        self.faces.insert((c, plain), found.clone());
        found
    }

    /// The face `id`, mapped the first time it is asked for.
    fn mapped(&mut self, id: fontdb::ID) -> Option<Face> {
        if let Some(face) = self.mapped.get(&id) {
            return Some(face.clone());
        }
        let face = shared(&mut self.database, id)?;
        self.mapped.insert(id, face.clone());
        Some(face)
    }
}

/// The fonts installed, found by reading every font file's names.
pub fn installed() -> fontdb::Database {
    let mut database = fontdb::Database::new();
    database.load_system_fonts();
    database
}

impl Fonts {
    /// The faces of the family `choice` names among `database`'s, or of the first
    /// installed family of those Caret prefers, on a screen of `scale` device pixels to
    /// the logical one.
    ///
    /// # Errors
    ///
    /// When no monospaced font is installed.
    pub fn new(
        mut database: fontdb::Database,
        choice: &FontChoice,
        scale: f32,
    ) -> Result<Self, FontError> {
        let family = choose_family(&database, choice.family.as_deref()).ok_or(FontError::NoFont)?;
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
        let size = choice.points * 96.0 / 72.0 * scale;
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
            styles: [regular, bold, italic, bold_italic],
            size,
            cell,
            context: ScaleContext::new(),
            fallbacks: RefCell::new(Fallbacks {
                database,
                faces: HashMap::new(),
                preferred: None,
                found: Vec::new(),
                mapped: HashMap::new(),
            }),
            features: choice.features.clone(),
            ligatures: choice.ligatures,
            divided: HashMap::new(),
            shapings: RefCell::default(),
            advances: RefCell::default(),
            labels,
            label_size,
            label_ascent,
            label_height,
        })
    }

    /// The faces `choice` names at `scale`, among the fonts these were found among.
    ///
    /// # Errors
    ///
    /// When no monospaced font is installed.
    pub fn at(&self, choice: &FontChoice, scale: f32) -> Result<Self, FontError> {
        Self::new(self.fallbacks.borrow().database.clone(), choice, scale)
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
        let known = self.advances.borrow()[usize::from(bold)].get(text).copied();
        if let Some(advance) = known {
            return advance;
        }
        let advance = self.labels[usize::from(bold)]
            .as_ref()
            .and_then(|face| self.shape_line(face, bold, text, self.label_size))
            .map_or(0.0, |(_, advance)| advance);
        let advances = &mut self.advances.borrow_mut()[usize::from(bold)];
        if advances.len() >= LABEL_ADVANCES {
            advances.clear();
        }
        advances.insert(text.into(), advance);
        advance
    }

    /// `text` in the window's own face, bold or not, from the top of its line, cut short
    /// with an ellipsis to fit `width` pixels; none if nothing in it draws.
    pub fn label(&mut self, text: &str, width: u32, bold: bool) -> Option<Raster> {
        let face = self.labels[usize::from(bold)].clone()?;
        self.line(&face, bold, text, width, self.label_size, self.label_ascent)
    }

    /// `text` as a heading: a bold label [`HEADING`] times as large.
    pub fn heading(&mut self, text: &str, width: u32) -> Option<Raster> {
        let face = self.labels[1].clone()?;
        let ascent = (self.label_ascent as f32 * HEADING).ceil() as i32;
        self.line(&face, true, text, width, self.label_size * HEADING, ascent)
    }

    fn line(
        &mut self,
        face: &Face,
        bold: bool,
        text: &str,
        width: u32,
        size: f32,
        ascent: i32,
    ) -> Option<Raster> {
        let (mut runs, advance) = self.shape_line(face, bold, text, size)?;
        if advance > width as f32 {
            let (dots, dots_advance) = self.shape(face, "\u{2026}", size, false)?;
            let room = width as f32 - dots_advance;
            for (_, glyphs) in &mut runs {
                let kept = glyphs
                    .iter()
                    .position(|glyph| glyph.pen + glyph.advance > room)
                    .unwrap_or(glyphs.len());
                glyphs.truncate(kept);
            }
            let pen = runs
                .iter()
                .flat_map(|(_, glyphs)| glyphs)
                .next_back()
                .map_or(0.0, |glyph| glyph.pen + glyph.advance);
            runs.push((face.clone(), moved(dots, pen, 0)));
        }
        let height = (ascent as f32 * LINE_TO_ASCENT).ceil() as u32;
        let mut placed = Vec::new();
        for (face, glyphs) in runs {
            let (ours, theirs): (Vec<_>, Vec<_>) = glyphs
                .into_iter()
                .partition(|glyph| glyph.id == 0 && missing_drawn(text, glyph).is_some());
            placed.extend(
                ours.iter()
                    .filter_map(|glyph| drawn_in_line(text, glyph, height)),
            );
            placed.extend(
                self.render(&face, size, &theirs, ascent)
                    .into_iter()
                    .map(|glyph| plain(glyph, size, ascent)),
            );
        }
        compose(&placed)
    }

    /// `text` shaped in `face`, with what `face` lacks in faces that have it, each
    /// stretch placed after the one before; and how far it all moves the pen.
    fn shape_line(
        &self,
        face: &Face,
        bold: bool,
        text: &str,
        size: f32,
    ) -> Option<(Vec<FaceRun>, f32)> {
        let mut runs = Vec::new();
        let mut pen = 0.0;
        for (face, range) in self.stretches(face, bold, text) {
            let start = range.start;
            let (glyphs, advance) = self.shape(&face, &text[range], size, false)?;
            runs.push((face, moved(glyphs, pen, start)));
            pen += advance;
        }
        Some((runs, pen))
    }

    /// The parts of `text` that each take one face: `face` for all it has, and for what
    /// no font has, whose box at least shows something is there.
    fn stretches(&self, face: &Face, bold: bool, text: &str) -> Vec<(Face, Range<usize>)> {
        let mut parts: Vec<(Face, Range<usize>)> = Vec::new();
        for (at, c) in text.char_indices() {
            let end = at + c.len_utf8();
            let last = parts.last_mut();
            // These change how the character before them looks, in that character's face.
            if matches!(c, '\u{200D}' | '\u{FE00}'..='\u{FE0F}')
                && let Some((_, range)) = last
            {
                range.end = end;
                continue;
            }
            let wanted = if face.has(c) {
                face.clone()
            } else if let Some((known, _)) = last.filter(|(known, _)| known.has(c)) {
                known.clone()
            } else {
                self.label_fallback(c, bold).unwrap_or_else(|| face.clone())
            };
            match parts.last_mut() {
                Some((known, range)) if known.key == wanted.key => range.end = end,
                _ => parts.push((wanted, at..end)),
            }
        }
        parts
    }

    /// A face for `c`, which the window's own face lacks: one that draws it in the text's
    /// colour where there is one, as a label is of one colour.
    fn label_fallback(&self, c: char, bold: bool) -> Option<Face> {
        let mut fallbacks = self.fallbacks.borrow_mut();
        let mut face = fallbacks
            .face(c, true)
            .or_else(|| fallbacks.face(c, false))?;
        if bold {
            face.weigh(fontdb::Weight::BOLD);
        }
        Some(face)
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
            let mut fallback = self.fallbacks.borrow_mut().face(first, false)?;
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
        let (glyphs, _) = self.shape(face, text, self.size, true)?;
        Some(self.render(face, self.size, &glyphs, self.cell.baseline))
    }

    /// How a run of ASCII symbols of `style` divides, as the lengths of its parts in
    /// order: more than one for symbols the font draws differently together than each
    /// alone, which are then one glyph, a [`GlyphText::Cluster`]. None while ligatures
    /// are off.
    pub fn ligatures(&mut self, run: &str, style: u8) -> Option<Rc<[u8]>> {
        if !self.ligatures {
            return None;
        }
        let style = style & (BOLD | ITALIC);
        if let Some(parts) = self.divided.get(&(run.into(), style)) {
            return Some(parts.clone());
        }
        let face = &self.styles[usize::from(style)];
        let glyphs = |text: &str| {
            self.shape(face, text, self.size, true)
                .map(|(glyphs, _)| glyphs)
                .unwrap_or_default()
        };
        let together = glyphs(run);
        let alone: Vec<_> = (0..run.len())
            .map(|index| glyphs(&run[index..=index]))
            .collect();
        let parts: Rc<[u8]> = divide(&joined(&together, &alone)).into();
        if self.divided.len() >= LIGATURE_RUNS {
            self.divided.clear();
        }
        self.divided.insert((run.into(), style), parts.clone());
        Some(parts)
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
            .builder_with_id(font, [face.key, 0])
            .size(size)
            .hint(true)
            // The context keeps the last face's coordinates, which a face drawn at its
            // default weight would otherwise take.
            .normalized_coords(std::iter::empty::<swash::NormalizedCoord>())
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
            let image = crate::color_glyph::paint(face.bytes(), face.index, glyph.id, size)
                .or_else(|| render.render(&mut scaler, glyph.id));
            let Some(image) = image else {
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
}

fn regular(info: &fontdb::FaceInfo) -> bool {
    info.style == fontdb::Style::Normal && info.weight == fontdb::Weight::NORMAL
}

/// The regular faces of the families tried first for a missing character, in order.
fn preferred_faces(database: &fontdb::Database) -> Vec<fontdb::ID> {
    FALLBACKS
        .iter()
        .flat_map(|family| {
            database
                .faces()
                .filter(move |info| {
                    regular(info)
                        && info
                            .families
                            .iter()
                            .any(|(name, _)| name.eq_ignore_ascii_case(family))
                })
                .map(|info| info.id)
        })
        .collect()
}

/// What a plan for shaping is made for: the terminal's features or none, for text of a
/// direction and script.
type PlanKey = (bool, Direction, Option<Script>);

/// What shaping in a face at a weight reads from the font once.
struct Shaping {
    data: ShaperData,
    instance: Option<ShaperInstance>,
    /// Plans take longer to make than shaping a line with.
    plans: HashMap<PlanKey, ShapePlan>,
}

impl Shaping {
    fn new(face: &Face) -> Option<Self> {
        let font = face.tables()?;
        let instance = face.weight.map(|value| {
            let tag = Tag::new(b"wght");
            ShaperInstance::from_variations(&font, [Variation { tag, value }])
        });
        Some(Self {
            data: ShaperData::new(&font),
            instance,
            plans: HashMap::new(),
        })
    }
}

impl Fonts {
    /// `text` shaped in `face` at `size` pixels to the em, with the terminal's features
    /// or none, and how far it moves the pen.
    fn shape(
        &self,
        face: &Face,
        text: &str,
        size: f32,
        terminal: bool,
    ) -> Option<(Vec<Shaped>, f32)> {
        let features: &[Feature] = if terminal { &self.features } else { &[] };
        let mut shapings = self.shapings.borrow_mut();
        let shaping = match shapings.entry((face.key, face.weight.map(f32::to_bits))) {
            Entry::Occupied(known) => known.into_mut(),
            Entry::Vacant(place) => place.insert(Shaping::new(face)?),
        };
        let font = face.tables()?;
        let shaper = shaping
            .data
            .shaper(&font)
            .instance(shaping.instance.as_ref())
            .build();
        let units = f32::from(u16::try_from(shaper.units_per_em()).ok()?);
        let to_pixels = size / units;
        let mut buffer = UnicodeBuffer::new();
        buffer.push_str(text);
        buffer.guess_segment_properties();
        // Text of no script of its own, such as symbols, has none, which the plan must
        // be made for as well.
        let script = Some(buffer.script()).filter(|&script| script != harfrust::script::UNKNOWN);
        let direction = buffer.direction();
        let plan = shaping
            .plans
            .entry((terminal, direction, script))
            .or_insert_with(|| ShapePlan::new(&shaper, direction, script, None, features));
        let options = ShapeOptions::new().plan(Some(plan)).features(features);
        let shaped = shaper.shape(buffer, options);
        let mut pen = 0.0f32;
        let mut glyphs = Vec::new();
        for (info, position) in shaped.glyph_infos().iter().zip(shaped.glyph_positions()) {
            let advance = position.x_advance as f32 * to_pixels;
            if let Ok(id) = u16::try_from(info.glyph_id) {
                glyphs.push(Shaped {
                    id,
                    cluster: info.cluster as usize,
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
}

/// For each character of a run, whether the font draws it differently `together` with
/// its neighbours than `alone`.
fn joined(together: &[Shaped], alone: &[Vec<Shaped>]) -> Vec<bool> {
    let same = |a: f32, b: f32| (a - b).abs() < 0.01;
    alone
        .iter()
        .enumerate()
        .map(|(index, alone)| {
            let mut here = together.iter().filter(|glyph| glyph.cluster == index);
            !matches!(
                (here.next(), here.next(), alone.as_slice()),
                (Some(glyph), None, [only])
                    if glyph.id == only.id
                        && same(glyph.advance, only.advance)
                        && same(glyph.x - glyph.pen, only.x - only.pen)
                        && same(glyph.y, only.y)
            )
        })
        .collect()
}

/// The lengths of a run's glyphs: characters joined to their neighbours make one, unless
/// there are more than [`LONGEST_LIGATURE`] of them.
fn divide(joined: &[bool]) -> Vec<u8> {
    let mut parts = Vec::with_capacity(joined.len());
    let mut index = 0;
    while index < joined.len() {
        let length = joined[index..].iter().take_while(|&&joined| joined).count();
        if (2..=LONGEST_LIGATURE).contains(&length) {
            parts.push(length as u8);
        } else {
            parts.extend(std::iter::repeat_n(1, length.max(1)));
        }
        index += length.max(1);
    }
    parts
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
    static KEYS: AtomicU64 = AtomicU64::new(1);
    // SAFETY: a font file rewritten while it is mapped can crash the program reading
    // it, a risk every program that maps fonts takes; nothing here writes to it.
    let (data, index) = unsafe { database.make_shared_face_data(id) }?;
    Some(Face {
        data,
        index,
        key: KEYS.fetch_add(1, Ordering::Relaxed),
        embolden: false,
        weight: None,
        slant: false,
    })
}

/// Whether face `id` has a glyph for `c`, and draws it in the text's colour if `plain`,
/// read without keeping its file mapped.
fn face_has(database: &fontdb::Database, id: fontdb::ID, c: char, plain: bool) -> bool {
    database
        .with_face_data(id, |data, index| {
            FontRef::from_index(data, index as usize).is_some_and(|font| font.charmap().map(c) != 0)
                && !(plain && colored(data, index))
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

/// Whether the font at `index` of `data` draws glyphs in colours of its own.
fn colored(data: &[u8], index: u32) -> bool {
    harfrust::FontRef::from_index(data, index).is_ok_and(|font| {
        [b"COLR", b"CBDT", b"sbix"]
            .iter()
            .any(|tag| font.table_data(Tag::new(tag)).is_some())
    })
}

/// `glyphs` of a part of a text that begins at byte `start`, placed `pen` pixels further
/// along.
fn moved(glyphs: Vec<Shaped>, pen: f32, start: usize) -> Vec<Shaped> {
    glyphs
        .into_iter()
        .map(|glyph| Shaped {
            cluster: glyph.cluster + start,
            x: glyph.x + pen,
            pen: glyph.pen + pen,
            ..glyph
        })
        .collect()
}

/// The character of `text` at `glyph` if no font has it and it can be drawn without one.
fn missing_drawn(text: &str, glyph: &Shaped) -> Option<char> {
    let c = text.get(glyph.cluster..)?.chars().next()?;
    crate::box_drawing::coverage(c, 1, 1, 1).map(|_| c)
}

/// What [`missing_drawn`] found for `glyph`, in the room the glyph took, `height` tall.
fn drawn_in_line(text: &str, glyph: &Shaped, height: u32) -> Option<(i32, i32, Image)> {
    let c = missing_drawn(text, glyph)?;
    let width = glyph.advance.round().max(1.0) as u32;
    let mut image = Image::new();
    image.placement.width = width;
    image.placement.height = height;
    image.data = crate::box_drawing::coverage(c, width, height, 1)?;
    Some((glyph.pen.round() as i32, 0, image))
}

/// A glyph of a label as a mask for the label's one colour, no higher than `size`:
/// a colour bitmap comes at the size it was drawn at.
fn plain(glyph: (i32, i32, Image), size: f32, ascent: i32) -> (i32, i32, Image) {
    if !matches!(glyph.2.content, Content::Color) {
        return glyph;
    }
    let Some(mut raster) = compose(std::slice::from_ref(&glyph)) else {
        return glyph;
    };
    if raster.height as f32 > size {
        raster = shrink(&raster, size / raster.height as f32);
        raster.top = ascent - (raster.height * 4 / 5) as i32;
    }
    let mut image = Image::new();
    image.placement.width = raster.width;
    image.placement.height = raster.height;
    image.data = raster
        .pixels
        .chunks_exact(4)
        .map(|pixel| pixel[3])
        .collect();
    (raster.left, raster.top, image)
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

    fn choice(family: Option<&str>, ligatures: bool) -> FontChoice {
        FontChoice {
            family: family.map(str::to_owned),
            points: 11.0,
            features: Vec::new(),
            ligatures,
        }
    }

    fn glyph(cluster: usize, id: u16) -> Shaped {
        Shaped {
            id,
            cluster,
            x: 0.0,
            y: 0.0,
            pen: 0.0,
            advance: 8.0,
        }
    }

    #[test]
    fn a_label_draws_what_its_face_lacks_from_another_and_the_rest_from_its_own() {
        let Ok(fonts) = Fonts::new(installed(), &choice(None, true), 1.0) else {
            return;
        };
        let Some(face) = fonts.labels[1].clone() else {
            return;
        };
        let star = '\u{2733}';
        if face.has(star) || fonts.label_fallback(star, true).is_none() {
            return;
        }
        let parts = fonts.stretches(&face, true, "\u{2733} Vulkan");
        let ranges: Vec<_> = parts.iter().map(|(_, range)| range.clone()).collect();
        assert_eq!(ranges, [0..3, 3..10]);
        assert_ne!(parts[0].0.key, face.key);
        assert_eq!(parts[1].0.key, face.key);
    }

    #[test]
    fn a_label_draws_a_braille_spinner_no_font_may_have() {
        let Ok(mut fonts) = Fonts::new(installed(), &choice(None, true), 1.0) else {
            return;
        };
        let lit = |raster: &Raster| raster.pixels.chunks_exact(4).filter(|p| p[3] > 0).count();
        let one = fonts.label("\u{2801}", 400, true).expect("drawn");
        let all = fonts.label("\u{28ff}", 400, true).expect("drawn");
        assert_eq!(lit(&all), 8 * lit(&one));
    }

    #[test]
    fn a_label_with_an_emoji_stays_of_one_colour_and_of_its_line_height() {
        let Ok(mut fonts) = Fonts::new(installed(), &choice(None, true), 1.0) else {
            return;
        };
        let Some(raster) = fonts.label("\u{1F600} done", 400, false) else {
            return;
        };
        assert!(!raster.color);
        assert!(raster.height as f32 <= fonts.label_height() as f32 * 1.5);
    }

    #[test]
    fn an_emoji_drawn_from_colr_comes_in_colour_where_such_a_font_is_installed() {
        let Ok(mut fonts) = Fonts::new(installed(), &choice(None, true), 1.0) else {
            return;
        };
        let smile = Glyph {
            text: GlyphText::Char('\u{1F600}'),
            style: 0,
            wide: true,
        };
        let Some(raster) = fonts.raster(&smile).filter(|raster| raster.color) else {
            return;
        };
        let cell = fonts.cell();
        assert!(raster.width as i32 <= cell.width * 2 && raster.height as i32 <= cell.height);
        assert!(
            raster
                .pixels
                .chunks_exact(4)
                .any(|p| p[0] > 200 && p[2] < 100 && p[3] > 200)
        );
    }

    #[test]
    fn symbols_drawn_otherwise_together_are_joined() {
        let together = [glyph(0, 90), glyph(2, 7)];
        let alone = [vec![glyph(0, 5)], vec![glyph(0, 6)], vec![glyph(0, 7)]];
        assert_eq!(joined(&together, &alone), [true, true, false]);
    }

    #[test]
    fn joined_neighbours_make_one_part() {
        assert_eq!(divide(&[false, true, true, false]), [1, 2, 1]);
        assert_eq!(divide(&[true, true, true]), [3]);
        assert_eq!(divide(&[false, false]), [1, 1]);
        assert_eq!(divide(&[]), [0u8; 0]);
        // One changed on its own, a contextual form, stays a part of one.
        assert_eq!(divide(&[true, false, true]), [1, 1, 1]);
        assert_eq!(
            divide(&[true; LONGEST_LIGATURE + 1]),
            [1; LONGEST_LIGATURE + 1]
        );
    }

    #[test]
    fn a_font_with_ligatures_joins_an_arrow_while_they_are_on() {
        let database = installed();
        let families = ["Fira Code", "JetBrains Mono", "Cascadia Code"]
            .into_iter()
            .filter(|&family| choose_family(&database, Some(family)).as_deref() == Some(family));
        for family in families {
            let mut fonts = Fonts::new(database.clone(), &choice(Some(family), true), 1.0)
                .expect("it is installed");
            assert_eq!(fonts.ligatures("->", 0).as_deref(), Some(&[2][..]));
            assert_eq!(fonts.ligatures("()", 0).as_deref(), Some(&[1, 1][..]));
            let mut off = fonts
                .at(&choice(Some(family), false), 1.0)
                .expect("it loaded on");
            assert_eq!(off.ligatures("->", 0), None);
        }
    }

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
        let Ok(mut fonts) = Fonts::new(installed(), &choice(None, true), 1.0) else {
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
