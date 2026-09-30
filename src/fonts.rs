//! The font at the size of the screen's pixels: cell metrics, and glyphs rasterised to
//! bitmaps for the atlas, with Pango choosing fallback fonts and cairo drawing them.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::mpsc;

use crate::box_drawing;
use crate::gtk::pango::prelude::*;
use crate::gtk::{cairo, pango};

pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GlyphText {
    Char(char),
    /// A character with combining marks after it.
    Cluster(Box<str>),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GlyphKey {
    pub text: GlyphText,
    /// [`BOLD`] and [`ITALIC`].
    pub style: u8,
    /// Two cells wide.
    pub wide: bool,
}

/// A glyph's pixels, premultiplied RGBA, and where they go relative to the cell's top left.
pub struct Bitmap {
    pub width: i32,
    pub height: i32,
    pub left: i32,
    pub top: i32,
    pub pixels: Vec<u8>,
    /// Coloured, an emoji; otherwise white, a coverage mask the text colour is applied to.
    pub color: bool,
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

/// Runs of symbols and the lengths of the glyphs each divides into.
type Divided = HashMap<Box<str>, Rc<[u8]>>;

pub struct Fonts {
    context: pango::Context,
    /// Regular, bold, italic, bold italic: indexed by style.
    faces: [pango::FontDescription; 4],
    /// The OpenType features chosen in the preferences.
    attributes: Option<pango::AttrList>,
    pub cell: CellMetrics,
    /// How runs of symbols divide into ligatures, by style.
    ligatures: RefCell<[Divided; 4]>,
    /// What the fonts were made from, for a thread to make the same ones.
    made_from: (pango::FontDescription, f64, cairo::FontOptions, String),
}

/// Symbols in a row beyond this are left apart: a ruler of dashes as one glyph for each
/// of its lengths would fill the atlas.
const LONGEST_LIGATURE: usize = 12;

/// Runs remembered before the table is emptied.
const LIGATURE_RUNS: usize = 4096;

impl Fonts {
    /// `font` at `dpi`, which already counts the scale of the screen, with OpenType
    /// `features` as CSS writes them.
    pub fn new(
        font: &pango::FontDescription,
        dpi: f64,
        options: &cairo::FontOptions,
        features: &str,
    ) -> Self {
        let context = pangocairo::FontMap::default().create_context();
        pangocairo::functions::context_set_resolution(&context, dpi);
        pangocairo::functions::context_set_font_options(&context, Some(options));

        // A proportional font on a grid spaces its letters apart; the desktop's monospace
        // font may not be installed, and fontconfig then falls back to a sans.
        let mut regular = font.clone();
        let monospace = context
            .load_font(font)
            .and_then(|loaded| loaded.face())
            .is_some_and(|face| face.family().is_monospace());
        if !monospace {
            regular.set_family("Monospace");
        }
        let mut bold = regular.clone();
        bold.set_weight(pango::Weight::Bold);
        let mut italic = regular.clone();
        italic.set_style(pango::Style::Italic);
        let mut bold_italic = bold.clone();
        bold_italic.set_style(pango::Style::Italic);

        let attributes = (!features.trim().is_empty()).then(|| {
            let attributes = pango::AttrList::new();
            attributes.insert(pango::AttrFontFeatures::new(features));
            attributes
        });
        let cell = measure(&context, &regular);
        Self {
            context,
            faces: [regular, bold, italic, bold_italic],
            attributes,
            cell,
            ligatures: RefCell::default(),
            made_from: (font.clone(), dpi, options.clone(), features.to_owned()),
        }
    }

    fn layout(&self, style: u8) -> pango::Layout {
        let layout = pango::Layout::new(&self.context);
        layout.set_font_description(Some(&self.faces[usize::from(style & 3)]));
        layout.set_attributes(self.attributes.as_ref());
        layout
    }

    /// How a run of ASCII symbols divides, as the lengths of its parts in order: more
    /// than one for symbols the font draws differently together than each alone, which
    /// are then one glyph, a [`GlyphText::Cluster`].
    pub fn ligatures(&self, run: &str, style: u8) -> Rc<[u8]> {
        let style = usize::from(style & 3);
        if let Some(parts) = self.ligatures.borrow()[style].get(run) {
            return parts.clone();
        }
        let layout = self.layout(style as u8);
        let together = shaped(&layout, run);
        let alone: Vec<_> = (0..run.len())
            .map(|index| shaped(&layout, &run[index..=index]))
            .collect();
        let parts = divide(&joined(&together, &alone));
        let parts: Rc<[u8]> = parts.into();
        let mut remembered = self.ligatures.borrow_mut();
        if remembered[style].len() >= LIGATURE_RUNS {
            remembered[style].clear();
        }
        remembered[style].insert(run.into(), parts.clone());
        parts
    }

    /// The glyph for `key`, or nothing when it has no ink, a space say.
    pub fn rasterize(&self, key: &GlyphKey) -> Option<Bitmap> {
        let cells = if key.wide { 2 } else { 1 };
        if let GlyphText::Char(c) = key.text
            && box_drawing::is_drawn(c)
        {
            let (width, height) = (self.cell.width * cells, self.cell.height);
            let light = (self.cell.width / 8)
                .max(self.cell.underline_thickness)
                .max(1);
            return paint(width, height, |cr| {
                box_drawing::draw(c, cr, width, height, light);
            })
            .map(|pixels| Bitmap {
                width,
                height,
                left: 0,
                top: 0,
                color: false,
                pixels,
            });
        }

        let layout = self.layout(key.style);
        match &key.text {
            GlyphText::Char(c) => layout.set_text(c.encode_utf8(&mut [0; 4])),
            GlyphText::Cluster(text) => layout.set_text(text),
        }
        let (ink, _) = layout.pixel_extents();
        if ink.width() <= 0 || ink.height() <= 0 {
            return None;
        }
        // A pixel of room around the ink, which the rounding of its extents can cut into.
        let (width, height) = (ink.width() + 2, ink.height() + 2);
        let pixels = paint(width, height, |cr| {
            cr.translate(f64::from(1 - ink.x()), f64::from(1 - ink.y()));
            cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
            pangocairo::functions::show_layout(cr, &layout);
        })?;
        // A white mask has every colour channel equal to its alpha.
        let color = pixels
            .as_chunks::<4>()
            .0
            .iter()
            .any(|p| p[0] != p[3] || p[1] != p[3] || p[2] != p[3]);
        let baseline = pango::units_to_double(layout.baseline()).round() as i32;
        Some(Bitmap {
            width,
            height,
            left: ink.x() - 1,
            top: self.cell.baseline - baseline + ink.y() - 1,
            pixels,
            color,
        })
    }
}

/// Rasterises glyphs on a thread of its own, with the same fonts, for frames that would
/// otherwise wait for many: a screenful of new CJK text takes a quarter of a second.
pub struct GlyphThread {
    requests: mpsc::Sender<GlyphKey>,
    results: async_channel::Receiver<(GlyphKey, Option<Bitmap>)>,
    /// Asked for and not yet taken back, so that each is asked for once.
    pending: HashSet<GlyphKey>,
}

impl GlyphThread {
    /// A thread rasterising with fonts made as `fonts` were. Its glyphs come back on the
    /// receiver, until the thread is dropped.
    pub fn spawn(fonts: &Fonts) -> (Self, async_channel::Receiver<(GlyphKey, Option<Bitmap>)>) {
        let (requests, requested) = mpsc::channel::<GlyphKey>();
        let (done, results) = async_channel::unbounded();
        let (font, dpi, options, features) = fonts.made_from.clone();
        let _ = std::thread::Builder::new()
            .name("glyphs".to_owned())
            .spawn(move || {
                let fonts = Fonts::new(&font, dpi, &options, &features);
                for key in requested {
                    let bitmap = fonts.rasterize(&key);
                    if done.send_blocking((key, bitmap)).is_err() {
                        break;
                    }
                }
            });
        let thread = Self {
            requests,
            results: results.clone(),
            pending: HashSet::new(),
        };
        (thread, results)
    }

    pub fn request(&mut self, key: &GlyphKey) {
        if self.pending.insert(key.clone()) {
            let _ = self.requests.send(key.clone());
        }
    }

    pub fn is_pending(&self, key: &GlyphKey) -> bool {
        self.pending.contains(key)
    }

    /// Takes back a glyph the thread sent.
    pub fn received(&mut self, key: &GlyphKey) {
        self.pending.remove(key);
    }
}

impl Drop for GlyphThread {
    fn drop(&mut self) {
        // Stops the thread at its next glyph, rather than after all it was asked for.
        self.results.close();
    }
}

/// A glyph as shaping gives it: the byte of the text it starts at, then the glyph and
/// where it sits.
type Shaped = (usize, [i32; 4]);

/// For each character of a run, whether the font draws it differently `together` with
/// its neighbours than `alone`.
fn joined(together: &[Shaped], alone: &[Vec<Shaped>]) -> Vec<bool> {
    alone
        .iter()
        .enumerate()
        .map(|(index, alone)| {
            let mut here = together.iter().filter(|(at, _)| *at == index);
            !matches!(
                (here.next(), here.next(), alone.as_slice()),
                (Some((_, glyph)), None, [(_, only)]) if glyph == only
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

/// The glyphs `text` is drawn with.
fn shaped(layout: &pango::Layout, text: &str) -> Vec<Shaped> {
    layout.set_text(text);
    let mut glyphs = Vec::with_capacity(text.len());
    let Some(line) = layout.line_readonly(0) else {
        return glyphs;
    };
    for run in line.runs() {
        let offset = run.item().offset();
        let string = run.glyph_string();
        for (info, cluster) in string.glyph_info().iter().zip(string.log_clusters()) {
            let geometry = info.geometry();
            glyphs.push((
                usize::try_from(offset + cluster).unwrap_or(0),
                [
                    info.glyph() as i32,
                    geometry.width(),
                    geometry.x_offset(),
                    geometry.y_offset(),
                ],
            ));
        }
    }
    glyphs
}

fn measure(context: &pango::Context, font: &pango::FontDescription) -> CellMetrics {
    let metrics = context.metrics(Some(font), None);
    let px = |units: i32| pango::units_to_double(units);
    let layout = pango::Layout::new(context);
    layout.set_font_description(Some(font));
    layout.set_text("M");
    let (_, logical) = layout.extents();

    let width = px(logical.width()).round().max(1.0) as i32;
    let ascent = px(metrics.ascent()).ceil() as i32;
    let descent = px(metrics.descent()).ceil() as i32;
    let height = (ascent + descent).max(1);
    let underline_thickness = px(metrics.underline_thickness()).round().max(1.0) as i32;
    let strikeout_thickness = px(metrics.strikethrough_thickness()).round().max(1.0) as i32;
    // Positions are above the baseline in Pango, and the underline's is its top edge.
    let underline = (ascent - px(metrics.underline_position()).round() as i32)
        .min(height - underline_thickness);
    let strikeout = ascent - px(metrics.strikethrough_position()).round() as i32;
    CellMetrics {
        width,
        height,
        baseline: ascent,
        underline,
        underline_thickness,
        strikeout,
        strikeout_thickness,
    }
}

/// Runs `draw` on a transparent `width` by `height` surface and returns its pixels as
/// premultiplied RGBA.
fn paint(width: i32, height: i32, draw: impl FnOnce(&cairo::Context)) -> Option<Vec<u8>> {
    let mut surface = cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).ok()?;
    {
        let cr = cairo::Context::new(&surface).ok()?;
        draw(&cr);
    }
    surface.flush();
    let stride = usize::try_from(surface.stride()).ok()?;
    let data = surface.data().ok()?;
    let row = usize::try_from(width).ok()? * 4;
    let mut pixels = Vec::with_capacity(row * usize::try_from(height).ok()?);
    for line in data.chunks(stride) {
        // Cairo's ARGB32 is a native-endian word per pixel.
        for pixel in line[..row].as_chunks::<4>().0 {
            let [b, g, r, a] = u32::from_ne_bytes(*pixel).to_le_bytes();
            pixels.extend_from_slice(&[r, g, b, a]);
        }
    }
    Some(pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glyph(at: usize, glyph: i32) -> Shaped {
        (at, [glyph, 10, 0, 0])
    }

    #[test]
    fn characters_shaped_as_they_are_alone_are_not_joined() {
        let together = [glyph(0, 5), glyph(1, 6), glyph(2, 7)];
        let alone = [vec![glyph(0, 5)], vec![glyph(0, 6)], vec![glyph(0, 7)]];
        assert_eq!(joined(&together, &alone), [false, false, false]);
    }

    #[test]
    fn other_glyphs_or_places_join_characters() {
        // A spacer and the ligature's glyph in place of the first two, as Fira Code does.
        let together = [glyph(0, 90), glyph(1, 91), glyph(2, 7)];
        let alone = [vec![glyph(0, 5)], vec![glyph(0, 6)], vec![glyph(0, 7)]];
        assert_eq!(joined(&together, &alone), [true, true, false]);

        // The same glyph moved counts too.
        let moved = [(0, [5, 10, 3, 0]), glyph(1, 6)];
        assert_eq!(joined(&moved, &alone[..2]), [true, false]);
    }

    #[test]
    fn characters_merged_into_one_glyph_are_joined() {
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
    }

    #[test]
    fn a_run_longer_than_the_longest_ligature_stays_apart() {
        assert_eq!(divide(&[true; LONGEST_LIGATURE]), [LONGEST_LIGATURE as u8]);
        assert_eq!(
            divide(&[true; LONGEST_LIGATURE + 1]),
            [1; LONGEST_LIGATURE + 1]
        );
    }
}
