//! Glyphs drawn from the font's `COLRv1` table, as most colour emoji now are. The scaler
//! draws only older colour tables, so these are painted here, on the CPU, once for each
//! glyph that goes into the atlas.

use skrifa::color::{
    Brush, ColorGlyphFormat, ColorPainter, ColorStop, CompositeMode, Extend, Transform,
};
use skrifa::instance::{LocationRef, Size};
use skrifa::outline::{DrawSettings, OutlinePen};
use skrifa::raw::TableProvider;
use skrifa::raw::types::BoundingBox;
use skrifa::{FontRef, GlyphId, MetadataProvider};
use swash::scale::Source;
use swash::scale::image::{Content, Image};
use swash::zeno::{Command, Format, Mask, Placement, Vector};

/// The most pixels a side of a glyph takes, so that a font's wrong clip box cannot ask
/// for gigabytes of layers.
const LARGEST: u32 = 512;

/// A colour the palette index stands for: the text's own, which a glyph kept in the atlas
/// for any text cannot know.
const FOREGROUND: u16 = 0xFFFF;

type Pixel = [f32; 4];

/// `glyph` at `size` pixels to the em, if the face draws it from `COLRv1`; none if the face
/// does not, or nothing shows.
pub fn paint(data: &[u8], index: u32, glyph: u16, size: f32) -> Option<Image> {
    let font = FontRef::from_index(data, index).ok()?;
    let colored = font
        .color_glyphs()
        .get_with_format(GlyphId::new(u32::from(glyph)), ColorGlyphFormat::ColrV1)?;
    let units = f32::from(font.head().ok()?.units_per_em());
    let scale = size / units;
    let bounds = colored
        .bounding_box(LocationRef::default(), Size::new(size))
        .unwrap_or(BoundingBox {
            x_min: -size / 4.0,
            y_min: -size / 4.0,
            x_max: size * 1.25,
            y_max: size * 1.25,
        });
    let (left, top) = (bounds.x_min.floor(), bounds.y_max.ceil());
    let width = (bounds.x_max.ceil() - left) as u32;
    let height = (top - bounds.y_min.floor()) as u32;
    if width == 0 || height == 0 || width > LARGEST || height > LARGEST {
        return None;
    }
    let palettes = font.color_palettes();
    let palette = palettes.get(0)?;
    let colors: Vec<Pixel> = palette
        .colors()
        .iter()
        .map(|color| {
            [color.red, color.green, color.blue, color.alpha].map(|c| f32::from(c) / 255.0)
        })
        .collect();
    let pixels = (width * height) as usize;
    let mut painter = Painter {
        font: font.clone(),
        colors,
        width,
        height,
        transforms: vec![Transform {
            xx: scale,
            yy: -scale,
            dx: -left,
            dy: top,
            ..Transform::default()
        }],
        clips: vec![vec![255; pixels]],
        layers: vec![vec![[0.0; 4]; pixels]],
        modes: Vec::new(),
    };
    colored.paint(LocationRef::default(), &mut painter).ok()?;
    let canvas = painter.layers.first()?;
    if canvas.iter().all(|pixel| pixel[3] <= 0.0) {
        return None;
    }
    Some(Image {
        source: Source::ColorOutline(0),
        content: Content::Color,
        placement: Placement {
            left: left as i32,
            top: top as i32,
            width,
            height,
        },
        data: canvas.iter().flat_map(|&pixel| straight(pixel)).collect(),
    })
}

/// A pixel with its colour multiplied by its alpha, as bytes with the two apart.
fn straight([r, g, b, a]: Pixel) -> [u8; 4] {
    if a <= 0.0 {
        return [0; 4];
    }
    let byte = |c: f32| (c * 255.0).round().clamp(0.0, 255.0) as u8;
    [byte(r / a), byte(g / a), byte(b / a), byte(a)]
}

struct Painter<'a> {
    font: FontRef<'a>,
    /// The palette, each colour with its components apart from its alpha.
    colors: Vec<Pixel>,
    width: u32,
    height: u32,
    /// From font units, or what the glyph has transformed them to, to pixels.
    transforms: Vec<Transform>,
    /// The coverage of each clip, already narrowed by those under it.
    clips: Vec<Vec<u8>>,
    layers: Vec<Vec<Pixel>>,
    modes: Vec<CompositeMode>,
}

impl Painter<'_> {
    fn transform(&self) -> Transform {
        self.transforms.last().copied().unwrap_or_default()
    }

    fn clip_to(&mut self, path: &[Command]) {
        let mut coverage = vec![0; (self.width * self.height) as usize];
        Mask::new(path)
            .format(Format::Alpha)
            .size(self.width, self.height)
            .render_into(&mut coverage, None);
        if let Some(under) = self.clips.last() {
            for (cover, &under) in coverage.iter_mut().zip(under) {
                *cover = (u16::from(*cover) * u16::from(under) / 255) as u8;
            }
        }
        self.clips.push(coverage);
    }

    fn color(&self, index: u16, alpha: f32) -> Pixel {
        let [r, g, b, a] = if index == FOREGROUND {
            [0.0, 0.0, 0.0, 1.0]
        } else {
            self.colors
                .get(usize::from(index))
                .copied()
                .unwrap_or_default()
        };
        let a = a * alpha;
        [r * a, g * a, b * a, a]
    }

    fn stops(&self, stops: &[ColorStop]) -> Vec<(f32, Pixel)> {
        stops
            .iter()
            .map(|stop| (stop.offset, self.color(stop.palette_index, stop.alpha)))
            .collect()
    }

    /// Paints each pixel the clip lets through with what `at` gives for where it lands in
    /// the coordinates of the brush.
    fn shade(&mut self, at: impl Fn(f32, f32) -> Option<Pixel>) {
        let Some(inverse) = invert(self.transform()) else {
            return;
        };
        let (Some(clip), Some(layer)) = (self.clips.last(), self.layers.last_mut()) else {
            return;
        };
        let width = self.width as usize;
        for (index, (pixel, &cover)) in layer.iter_mut().zip(clip).enumerate() {
            if cover == 0 {
                continue;
            }
            let x = (index % width) as f32 + 0.5;
            let y = (index / width) as f32 + 0.5;
            let (x, y) = apply(inverse, x, y);
            if let Some(source) = at(x, y) {
                let cover = f32::from(cover) / 255.0;
                *pixel = over(*pixel, source.map(|c| c * cover));
            }
        }
    }
}

fn apply(t: Transform, x: f32, y: f32) -> (f32, f32) {
    (t.xx * x + t.xy * y + t.dx, t.yx * x + t.yy * y + t.dy)
}

fn invert(t: Transform) -> Option<Transform> {
    let determinant = t.xx * t.yy - t.xy * t.yx;
    if determinant.abs() < f32::EPSILON * f32::EPSILON {
        return None;
    }
    let (xx, xy) = (t.yy / determinant, -t.xy / determinant);
    let (yx, yy) = (-t.yx / determinant, t.xx / determinant);
    Some(Transform {
        xx,
        yx,
        xy,
        yy,
        dx: -(xx * t.dx + xy * t.dy),
        dy: -(yx * t.dx + yy * t.dy),
    })
}

fn over(below: Pixel, above: Pixel) -> Pixel {
    let rest = 1.0 - above[3];
    std::array::from_fn(|i| above[i] + below[i] * rest)
}

/// The colour at `t` along stops in order of their offsets, past the ends as `extend` says.
fn along(stops: &[(f32, Pixel)], extend: Extend, t: f32) -> Pixel {
    let t = match extend {
        Extend::Repeat => t.rem_euclid(1.0),
        Extend::Reflect => 1.0 - (t.rem_euclid(2.0) - 1.0).abs(),
        _ => t,
    };
    let Some(&(first_offset, first)) = stops.first() else {
        return [0.0; 4];
    };
    if t <= first_offset {
        return first;
    }
    for pair in stops.windows(2) {
        let [(from, a), (to, b)] = pair else { continue };
        if t <= *to {
            let part = if to > from {
                (t - from) / (to - from)
            } else {
                1.0
            };
            return std::array::from_fn(|i| a[i] + (b[i] - a[i]) * part);
        }
    }
    stops.last().map_or(first, |&(_, last)| last)
}

/// How far from the start to the end circle of a radial gradient the circle through
/// `(px, py)` is, relative to the start circle's centre; the furthest such circle, as it
/// paints over nearer ones.
fn radial((px, py): (f32, f32), (cx, cy): (f32, f32), r0: f32, r1: f32) -> Option<f32> {
    let dr = r1 - r0;
    let a = cx * cx + cy * cy - dr * dr;
    let b = px * cx + py * cy + r0 * dr;
    let c = px * px + py * py - r0 * r0;
    let reaches = |t: &f32| r0 + t * dr >= 0.0;
    if a.abs() < 1e-6 {
        return (b.abs() >= 1e-6).then(|| c / (2.0 * b)).filter(reaches);
    }
    let root = (b * b - a * c).sqrt();
    if root.is_nan() {
        return None;
    }
    let (near, far) = ((b - root) / a, (b + root) / a);
    let (near, far) = if near < far { (near, far) } else { (far, near) };
    Some(far).filter(reaches).or(Some(near).filter(reaches))
}

/// `above` composited on `below`, both with their colours multiplied by their alpha.
fn composite(mode: CompositeMode, below: Pixel, above: Pixel) -> Pixel {
    let (ab, aa) = (below[3], above[3]);
    let portions =
        |fa: f32, fb: f32| -> Pixel { std::array::from_fn(|i| above[i] * fa + below[i] * fb) };
    let separable = |blend: fn(f32, f32) -> f32| -> Pixel {
        let mut pixel: Pixel = std::array::from_fn(|i| {
            let unmixed = above[i] * (1.0 - ab) + below[i] * (1.0 - aa);
            let (cb, cs) = (
                if ab > 0.0 { below[i] / ab } else { 0.0 },
                if aa > 0.0 { above[i] / aa } else { 0.0 },
            );
            unmixed + aa * ab * blend(cb, cs)
        });
        pixel[3] = aa + ab - aa * ab;
        pixel
    };
    match mode {
        CompositeMode::Clear => [0.0; 4],
        CompositeMode::Src => above,
        CompositeMode::Dest => below,
        CompositeMode::DestOver => portions(1.0 - ab, 1.0),
        CompositeMode::SrcIn => portions(ab, 0.0),
        CompositeMode::DestIn => portions(0.0, aa),
        CompositeMode::SrcOut => portions(1.0 - ab, 0.0),
        CompositeMode::DestOut => portions(0.0, 1.0 - aa),
        CompositeMode::SrcAtop => portions(ab, 1.0 - aa),
        CompositeMode::DestAtop => portions(1.0 - ab, aa),
        CompositeMode::Xor => portions(1.0 - ab, 1.0 - aa),
        CompositeMode::Plus => portions(1.0, 1.0).map(|c| c.min(1.0)),
        CompositeMode::Multiply => separable(|cb, cs| cb * cs),
        CompositeMode::Screen => separable(|cb, cs| cb + cs - cb * cs),
        CompositeMode::Darken => separable(f32::min),
        CompositeMode::Lighten => separable(f32::max),
        // The remaining blends are not known in any emoji font; plain painting comes
        // closest for what they would change.
        _ => over(below, above),
    }
}

struct Outline {
    transform: Transform,
    path: Vec<Command>,
}

impl Outline {
    fn point(&self, x: f32, y: f32) -> Vector {
        let (x, y) = apply(self.transform, x, y);
        Vector::new(x, y)
    }
}

impl OutlinePen for Outline {
    fn move_to(&mut self, x: f32, y: f32) {
        self.path.push(Command::MoveTo(self.point(x, y)));
    }

    fn line_to(&mut self, x: f32, y: f32) {
        self.path.push(Command::LineTo(self.point(x, y)));
    }

    fn quad_to(&mut self, cx: f32, cy: f32, x: f32, y: f32) {
        let command = Command::QuadTo(self.point(cx, cy), self.point(x, y));
        self.path.push(command);
    }

    fn curve_to(&mut self, ax: f32, ay: f32, bx: f32, by: f32, x: f32, y: f32) {
        let command = Command::CurveTo(self.point(ax, ay), self.point(bx, by), self.point(x, y));
        self.path.push(command);
    }

    fn close(&mut self) {
        self.path.push(Command::Close);
    }
}

impl ColorPainter for Painter<'_> {
    fn push_transform(&mut self, transform: Transform) {
        self.transforms.push(self.transform() * transform);
    }

    fn pop_transform(&mut self) {
        self.transforms.pop();
    }

    fn push_clip_glyph(&mut self, glyph: GlyphId) {
        let mut outline = Outline {
            transform: self.transform(),
            path: Vec::new(),
        };
        if let Some(found) = self.font.outline_glyphs().get(glyph) {
            let settings = DrawSettings::unhinted(Size::unscaled(), LocationRef::default());
            if let Err(error) = found.draw(settings, &mut outline) {
                tracing::debug!("drawing the outline of a colour glyph: {error}");
            }
        }
        self.clip_to(&outline.path);
    }

    fn push_clip_box(&mut self, clip: BoundingBox<f32>) {
        let outline = Outline {
            transform: self.transform(),
            path: Vec::new(),
        };
        let path = [
            Command::MoveTo(outline.point(clip.x_min, clip.y_min)),
            Command::LineTo(outline.point(clip.x_max, clip.y_min)),
            Command::LineTo(outline.point(clip.x_max, clip.y_max)),
            Command::LineTo(outline.point(clip.x_min, clip.y_max)),
            Command::Close,
        ];
        self.clip_to(&path);
    }

    fn pop_clip(&mut self) {
        self.clips.pop();
    }

    fn fill(&mut self, brush: Brush<'_>) {
        match brush {
            Brush::Solid {
                palette_index,
                alpha,
            } => {
                let color = self.color(palette_index, alpha);
                self.shade(|_, _| Some(color));
            }
            Brush::LinearGradient {
                p0,
                p1,
                color_stops,
                extend,
            } => {
                let stops = self.stops(color_stops);
                let (dx, dy) = (p1.x - p0.x, p1.y - p0.y);
                let length = dx * dx + dy * dy;
                if length > 0.0 {
                    self.shade(|x, y| {
                        let t = ((x - p0.x) * dx + (y - p0.y) * dy) / length;
                        Some(along(&stops, extend, t))
                    });
                }
            }
            Brush::RadialGradient {
                c0,
                r0,
                c1,
                r1,
                color_stops,
                extend,
            } => {
                let stops = self.stops(color_stops);
                let centres = (c1.x - c0.x, c1.y - c0.y);
                self.shade(|x, y| {
                    let t = radial((x - c0.x, y - c0.y), centres, r0, r1)?;
                    Some(along(&stops, extend, t))
                });
            }
            Brush::SweepGradient {
                c0,
                start_angle,
                end_angle,
                color_stops,
                extend,
            } => {
                let stops = self.stops(color_stops);
                let span = end_angle - start_angle;
                if span != 0.0 {
                    self.shade(|x, y| {
                        let angle = (y - c0.y).atan2(x - c0.x).to_degrees().rem_euclid(360.0);
                        Some(along(&stops, extend, (angle - start_angle) / span))
                    });
                }
            }
        }
    }

    fn push_layer(&mut self, mode: CompositeMode) {
        self.layers
            .push(vec![[0.0; 4]; (self.width * self.height) as usize]);
        self.modes.push(mode);
    }

    fn pop_layer(&mut self) {
        let (Some(above), Some(mode)) = (self.layers.pop(), self.modes.pop()) else {
            return;
        };
        let (Some(below), Some(clip)) = (self.layers.last_mut(), self.clips.last()) else {
            return;
        };
        for ((pixel, &source), &cover) in below.iter_mut().zip(&above).zip(clip) {
            let cover = f32::from(cover) / 255.0;
            let mixed = composite(mode, *pixel, source);
            *pixel = std::array::from_fn(|i| pixel[i] + (mixed[i] - pixel[i]) * cover);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RED: Pixel = [1.0, 0.0, 0.0, 1.0];
    const BLUE: Pixel = [0.0, 0.0, 1.0, 1.0];

    #[track_caller]
    fn same(a: Pixel, b: Pixel) {
        let close = a.iter().zip(&b).all(|(a, b)| (a - b).abs() < 1e-5);
        assert!(close, "{a:?} and {b:?}");
    }

    #[test]
    fn a_gradient_goes_between_its_stops_and_past_them_as_it_extends() {
        let stops = [(0.0, RED), (1.0, BLUE)];
        same(along(&stops, Extend::Pad, 0.5), [0.5, 0.0, 0.5, 1.0]);
        same(along(&stops, Extend::Pad, 3.0), BLUE);
        same(along(&stops, Extend::Repeat, 1.25), [0.75, 0.0, 0.25, 1.0]);
        same(along(&stops, Extend::Reflect, 1.25), [0.25, 0.0, 0.75, 1.0]);
    }

    #[test]
    fn a_radial_gradient_of_one_centre_goes_by_the_distance_from_it() {
        assert_eq!(radial((5.0, 0.0), (0.0, 0.0), 0.0, 10.0), Some(0.5));
        assert_eq!(radial((10.0, 0.0), (0.0, 0.0), 0.0, 10.0), Some(1.0));
    }

    #[test]
    fn a_layer_composited_in_keeps_only_what_is_under_it() {
        let half = [0.5, 0.0, 0.0, 0.5];
        same(
            composite(CompositeMode::SrcIn, half, BLUE),
            [0.0, 0.0, 0.5, 0.5],
        );
        same(composite(CompositeMode::DestOut, RED, BLUE), [0.0; 4]);
        same(composite(CompositeMode::SrcOver, RED, BLUE), BLUE);
    }

    #[test]
    fn inverting_a_transform_brings_a_point_back() {
        let transform = Transform {
            xx: 2.0,
            yx: 0.5,
            xy: -1.0,
            yy: 3.0,
            dx: 7.0,
            dy: -4.0,
        };
        let inverse = invert(transform).expect("it is not flat");
        let (x, y) = apply(transform, 3.0, 5.0);
        let (x, y) = apply(inverse, x, y);
        assert!((x - 3.0).abs() < 1e-4 && (y - 5.0).abs() < 1e-4);
    }

    #[test]
    fn stops_with_straight_alpha_become_bytes_with_the_colour_apart() {
        assert_eq!(straight([0.5, 0.0, 0.0, 0.5]), [255, 0, 0, 128]);
        assert_eq!(straight([0.0; 4]), [0; 4]);
    }
}
