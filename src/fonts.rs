//! The font at the size of the screen's pixels: cell metrics, and glyphs rasterised to
//! bitmaps for the atlas, with Pango choosing fallback fonts and cairo drawing them.

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

pub struct Fonts {
    context: pango::Context,
    /// Regular, bold, italic, bold italic: indexed by style.
    faces: [pango::FontDescription; 4],
    pub cell: CellMetrics,
}

impl Fonts {
    /// `font` at `dpi`, which already counts the scale of the screen.
    pub fn new(font: &pango::FontDescription, dpi: f64, options: &cairo::FontOptions) -> Self {
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

        let cell = measure(&context, &regular);
        Self {
            context,
            faces: [regular, bold, italic, bold_italic],
            cell,
        }
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

        let layout = pango::Layout::new(&self.context);
        layout.set_font_description(Some(&self.faces[usize::from(key.style & 3)]));
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
            .chunks_exact(4)
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
        for pixel in line[..row].chunks_exact(4) {
            let [b, g, r, a] =
                u32::from_ne_bytes([pixel[0], pixel[1], pixel[2], pixel[3]]).to_le_bytes();
            pixels.extend_from_slice(&[r, g, b, a]);
        }
    }
    Some(pixels)
}
