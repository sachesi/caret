//! Quads painted on the CPU into a buffer, for surfaces that change too seldom to need a
//! swapchain of their own.

use crate::chrome::Quad;
use crate::fonts::Raster;

/// Paints `quads` over `pixels`, premultiplied BGRA `width` wide, in order. Those from
/// `clipped_from` on keep within `clip`, as `[x, y, width, height]`. A quad's `glyph` less
/// one indexes `rasters`.
pub fn paint(
    pixels: &mut [u8],
    width: u32,
    quads: &[Quad],
    rasters: &[Raster],
    (clipped_from, clip): (usize, [f32; 4]),
) {
    let height = pixels.len() as u32 / 4 / width.max(1);
    for (index, quad) in quads.iter().enumerate() {
        let bounds = if index < clipped_from {
            [0.0, 0.0, width as f32, height as f32]
        } else {
            clip
        };
        match quad
            .glyph
            .checked_sub(1)
            .and_then(|at| rasters.get(at as usize))
        {
            Some(raster) => blit(pixels, width, quad, raster, bounds),
            None => fill(pixels, width, quad, bounds),
        }
    }
}

/// What `quad` covers of the pixel centred `(x, y)` from its centre, from 0 to 1.
fn coverage(quad: &Quad, half: (f32, f32), (x, y): (f32, f32)) -> f32 {
    let radius = if quad.corners == 0 {
        0.0
    } else {
        quad.radius.min(half.0).min(half.1)
    };
    let (dx, dy) = (x.abs() - half.0 + radius, y.abs() - half.1 + radius);
    let outside = dx.max(0.0).hypot(dy.max(0.0));
    let distance = outside + dx.max(dy).min(0.0) - radius;
    let fade = quad.blur.max(0.5);
    (0.5 - distance / (2.0 * fade)).clamp(0.0, 1.0)
}

fn fill(pixels: &mut [u8], width: u32, quad: &Quad, bounds: [f32; 4]) {
    let reach = quad.blur;
    let left = (quad.x - reach).max(bounds[0]).floor() as u32;
    let top = (quad.y - reach).max(bounds[1]).floor() as u32;
    let right = (quad.x + quad.width + reach)
        .min(bounds[0] + bounds[2])
        .ceil() as u32;
    let bottom = (quad.y + quad.height + reach)
        .min(bounds[1] + bounds[3])
        .ceil() as u32;
    let half = (quad.width / 2.0, quad.height / 2.0);
    let centre = (quad.x + half.0, quad.y + half.1);
    let [red, green, blue, alpha] = quad.color.to_be_bytes().map(f32::from);
    let rows = u32::try_from(pixels.len() / 4).unwrap_or(0) / width.max(1);
    for y in top..bottom.min(rows) {
        for x in left..right.min(width) {
            let at = (x as f32 + 0.5 - centre.0, y as f32 + 0.5 - centre.1);
            let cover = coverage(quad, half, at) * alpha / 255.0;
            if cover > 0.0 {
                over(
                    pixels,
                    width,
                    (x, y),
                    [red * cover, green * cover, blue * cover, 255.0 * cover],
                );
            }
        }
    }
}

fn blit(pixels: &mut [u8], width: u32, quad: &Quad, raster: &Raster, bounds: [f32; 4]) {
    let [red, green, blue, alpha] = quad.color.to_be_bytes().map(|part| f32::from(part) / 255.0);
    let (left, top) = (quad.x.round() as i64, quad.y.round() as i64);
    let rows = i64::from(u32::try_from(pixels.len() / 4).unwrap_or(0) / width.max(1));
    for row in 0..i64::from(raster.height) {
        for column in 0..i64::from(raster.width) {
            let (x, y) = (left + column, top + row);
            let within = x >= 0
                && y >= 0
                && x < i64::from(width)
                && y < rows
                && (bounds[0]..bounds[0] + bounds[2]).contains(&(x as f32))
                && (bounds[1]..bounds[1] + bounds[3]).contains(&(y as f32));
            if !within {
                continue;
            }
            let at = ((row * i64::from(raster.width) + column) * 4) as usize;
            let source = &raster.pixels[at..at + 4];
            let source = if raster.color {
                [source[0], source[1], source[2], source[3]].map(|part| f32::from(part) * alpha)
            } else {
                let cover = f32::from(source[3]) * alpha;
                [cover * red, cover * green, cover * blue, cover]
            };
            if source[3] > 0.0 {
                over(pixels, width, (x as u32, y as u32), source);
            }
        }
    }
}

/// Lays premultiplied RGBA from 0 to 255 over a pixel.
fn over(pixels: &mut [u8], width: u32, (x, y): (u32, u32), [red, green, blue, alpha]: [f32; 4]) {
    let at = ((y * width + x) * 4) as usize;
    let keep = 1.0 - alpha / 255.0;
    for (byte, source) in pixels[at..at + 4].iter_mut().zip([blue, green, red, alpha]) {
        *byte = (f32::from(*byte) * keep + source).round().clamp(0.0, 255.0) as u8;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WHOLE: [f32; 4] = [0.0, 0.0, 8.0, 8.0];

    fn pixel(pixels: &[u8], x: usize, y: usize) -> &[u8] {
        &pixels[(y * 8 + x) * 4..(y * 8 + x) * 4 + 4]
    }

    #[test]
    fn a_quad_fills_its_pixels_in_blue_green_red_alpha() {
        let mut pixels = vec![0; 8 * 8 * 4];
        let quad = Quad {
            x: 2.0,
            y: 2.0,
            width: 4.0,
            height: 4.0,
            color: 0xff00_00ff,
            ..Quad::default()
        };
        paint(&mut pixels, 8, &[quad], &[], (0, WHOLE));
        assert_eq!(pixel(&pixels, 3, 3), [0, 0, 255, 255]);
        assert_eq!(pixel(&pixels, 1, 1), [0, 0, 0, 0]);
    }

    #[test]
    fn rounded_corners_leave_the_corner_out() {
        let mut pixels = vec![0; 8 * 8 * 4];
        let quad = Quad {
            width: 8.0,
            height: 8.0,
            radius: 4.0,
            corners: 1,
            color: 0xffff_ffff,
            ..Quad::default()
        };
        paint(&mut pixels, 8, &[quad], &[], (0, WHOLE));
        assert_eq!(pixel(&pixels, 0, 0), [0, 0, 0, 0]);
        assert_eq!(pixel(&pixels, 4, 4), [255, 255, 255, 255]);
    }

    #[test]
    fn a_mask_takes_the_quad_s_colour_and_stays_in_the_clip() {
        let mut pixels = vec![0; 8 * 8 * 4];
        let raster = Raster {
            width: 2,
            height: 1,
            left: 0,
            top: 0,
            pixels: vec![255; 8],
            color: false,
        };
        let quad = Quad {
            x: 1.0,
            y: 1.0,
            width: 2.0,
            height: 1.0,
            color: 0x00ff_00ff,
            glyph: 1,
            ..Quad::default()
        };
        paint(
            &mut pixels,
            8,
            &[quad],
            &[raster],
            (0, [0.0, 0.0, 2.0, 8.0]),
        );
        assert_eq!(pixel(&pixels, 1, 1), [0, 255, 0, 255]);
        assert_eq!(pixel(&pixels, 2, 1), [0, 0, 0, 0]);
    }
}
