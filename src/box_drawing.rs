//! Box-drawing characters and block elements, drawn to fill the cell exactly instead of
//! taken from the font, whose versions leave gaps between rows when the line is taller
//! than the glyphs.

use std::f64::consts::PI;

use crate::gtk::cairo;

/// Weights of the arms of U+2500 to U+254F: up, down, left, right; 1 light, 2 heavy.
/// Dashed lines are drawn solid.
#[rustfmt::skip]
const LINES: [[u8; 4]; 80] = [
    [0,0,1,1], [0,0,2,2], [1,1,0,0], [2,2,0,0], [0,0,1,1], [0,0,2,2], [1,1,0,0], [2,2,0,0],
    [0,0,1,1], [0,0,2,2], [1,1,0,0], [2,2,0,0], [0,1,0,1], [0,1,0,2], [0,2,0,1], [0,2,0,2],
    [0,1,1,0], [0,1,2,0], [0,2,1,0], [0,2,2,0], [1,0,0,1], [1,0,0,2], [2,0,0,1], [2,0,0,2],
    [1,0,1,0], [1,0,2,0], [2,0,1,0], [2,0,2,0], [1,1,0,1], [1,1,0,2], [2,1,0,1], [1,2,0,1],
    [2,2,0,1], [2,1,0,2], [1,2,0,2], [2,2,0,2], [1,1,1,0], [1,1,2,0], [2,1,1,0], [1,2,1,0],
    [2,2,1,0], [2,1,2,0], [1,2,2,0], [2,2,2,0], [0,1,1,1], [0,1,2,1], [0,1,1,2], [0,1,2,2],
    [0,2,1,1], [0,2,2,1], [0,2,1,2], [0,2,2,2], [1,0,1,1], [1,0,2,1], [1,0,1,2], [1,0,2,2],
    [2,0,1,1], [2,0,2,1], [2,0,1,2], [2,0,2,2], [1,1,1,1], [1,1,2,1], [1,1,1,2], [1,1,2,2],
    [2,1,1,1], [1,2,1,1], [2,2,1,1], [2,1,2,1], [2,1,1,2], [1,2,2,1], [1,2,1,2], [2,1,2,2],
    [1,2,2,2], [2,2,2,1], [2,2,1,2], [2,2,2,2], [0,0,1,1], [0,0,2,2], [1,1,0,0], [2,2,0,0],
];

/// U+2574 to U+257F: half lines and lines that change weight halfway.
#[rustfmt::skip]
const HALF_LINES: [[u8; 4]; 12] = [
    [0,0,1,0], [1,0,0,0], [0,0,0,1], [0,1,0,0], [0,0,2,0], [2,0,0,0], [0,0,0,2], [0,2,0,0],
    [0,0,1,2], [1,2,0,0], [0,0,2,1], [2,1,0,0],
];

enum Shape {
    Lines([u8; 4]),
    /// A rounded corner joining the two edges named, U+256D to U+2570.
    Arc {
        down: bool,
        right: bool,
    },
    /// Rectangles in eighths of the cell: x0, y0, x1, y1.
    Blocks(&'static [[u8; 4]]),
    Shade(f64),
}

fn shape(c: char) -> Option<Shape> {
    const UPPER_LEFT: [u8; 4] = [0, 0, 4, 4];
    const UPPER_RIGHT: [u8; 4] = [4, 0, 8, 4];
    const LOWER_LEFT: [u8; 4] = [0, 4, 4, 8];
    const LOWER_RIGHT: [u8; 4] = [4, 4, 8, 8];
    let code = u32::from(c);
    Some(match code {
        0x2500..=0x254f => Shape::Lines(LINES[(code - 0x2500) as usize]),
        0x256d => Shape::Arc {
            down: true,
            right: true,
        },
        0x256e => Shape::Arc {
            down: true,
            right: false,
        },
        0x256f => Shape::Arc {
            down: false,
            right: false,
        },
        0x2570 => Shape::Arc {
            down: false,
            right: true,
        },
        0x2574..=0x257f => Shape::Lines(HALF_LINES[(code - 0x2574) as usize]),
        0x2580 => Shape::Blocks(&[[0, 0, 8, 4]]),
        0x2581..=0x2587 => {
            const LOWER: [[u8; 4]; 7] = [
                [0, 7, 8, 8],
                [0, 6, 8, 8],
                [0, 5, 8, 8],
                [0, 4, 8, 8],
                [0, 3, 8, 8],
                [0, 2, 8, 8],
                [0, 1, 8, 8],
            ];
            Shape::Blocks(std::slice::from_ref(&LOWER[(code - 0x2581) as usize]))
        }
        0x2588 => Shape::Blocks(&[[0, 0, 8, 8]]),
        0x2589..=0x258f => {
            const LEFT: [[u8; 4]; 7] = [
                [0, 0, 7, 8],
                [0, 0, 6, 8],
                [0, 0, 5, 8],
                [0, 0, 4, 8],
                [0, 0, 3, 8],
                [0, 0, 2, 8],
                [0, 0, 1, 8],
            ];
            Shape::Blocks(std::slice::from_ref(&LEFT[(code - 0x2589) as usize]))
        }
        0x2590 => Shape::Blocks(&[[4, 0, 8, 8]]),
        0x2591 => Shape::Shade(0.25),
        0x2592 => Shape::Shade(0.5),
        0x2593 => Shape::Shade(0.75),
        0x2594 => Shape::Blocks(&[[0, 0, 8, 1]]),
        0x2595 => Shape::Blocks(&[[7, 0, 8, 8]]),
        0x2596 => Shape::Blocks(&[LOWER_LEFT]),
        0x2597 => Shape::Blocks(&[LOWER_RIGHT]),
        0x2598 => Shape::Blocks(&[UPPER_LEFT]),
        0x2599 => Shape::Blocks(&[UPPER_LEFT, LOWER_LEFT, LOWER_RIGHT]),
        0x259a => Shape::Blocks(&[UPPER_LEFT, LOWER_RIGHT]),
        0x259b => Shape::Blocks(&[UPPER_LEFT, UPPER_RIGHT, LOWER_LEFT]),
        0x259c => Shape::Blocks(&[UPPER_LEFT, UPPER_RIGHT, LOWER_RIGHT]),
        0x259d => Shape::Blocks(&[UPPER_RIGHT]),
        0x259e => Shape::Blocks(&[UPPER_RIGHT, LOWER_LEFT]),
        0x259f => Shape::Blocks(&[UPPER_RIGHT, LOWER_LEFT, LOWER_RIGHT]),
        _ => return None,
    })
}

pub fn is_drawn(c: char) -> bool {
    shape(c).is_some()
}

/// Draws `c` in white on a `width` by `height` cell, with lines `light` pixels thick.
/// False for characters left to the font.
pub fn draw(c: char, cr: &cairo::Context, width: i32, height: i32, light: i32) -> bool {
    let Some(shape) = shape(c) else {
        return false;
    };
    let (w, h) = (f64::from(width), f64::from(height));
    let heavy = (light * 5 + 1) / 2;
    let thickness = |weight: u8| match weight {
        0 => 0,
        1 => light,
        _ => heavy,
    };
    cr.set_source_rgba(1.0, 1.0, 1.0, 1.0);
    match shape {
        Shape::Lines([up, down, left, right]) => {
            let vertical = thickness(up).max(thickness(down));
            let horizontal = thickness(left).max(thickness(right));
            // Each arm runs from its edge to the far side of the stroke across it, so the
            // arms meet without a notch.
            let across = |own: i32, other: i32| if other > 0 { other } else { own };
            let fill = |x: i32, y: i32, x1: i32, y1: i32| {
                cr.rectangle(
                    f64::from(x),
                    f64::from(y),
                    f64::from(x1 - x),
                    f64::from(y1 - y),
                );
            };
            for (weight, is_left) in [(left, true), (right, false)] {
                let t = thickness(weight);
                if t == 0 {
                    continue;
                }
                let join = across(t, vertical);
                let y = (height - t) / 2;
                if is_left {
                    fill(0, y, (width - join) / 2 + join, y + t);
                } else {
                    fill((width - join) / 2, y, width, y + t);
                }
            }
            for (weight, is_up) in [(up, true), (down, false)] {
                let t = thickness(weight);
                if t == 0 {
                    continue;
                }
                let join = across(t, horizontal);
                let x = (width - t) / 2;
                if is_up {
                    fill(x, 0, x + t, (height - join) / 2 + join);
                } else {
                    fill(x, (height - join) / 2, x + t, height);
                }
            }
            let _ = cr.fill();
        }
        Shape::Arc { down, right } => {
            let t = f64::from(light);
            let cx = f64::from((width - light) / 2) + t / 2.0;
            let cy = f64::from((height - light) / 2) + t / 2.0;
            let radius = (w / 2.0).min(h / 2.0);
            let (ey, sy) = if down { (h, 1.0) } else { (0.0, -1.0) };
            let (ex, sx) = if right { (w, 1.0) } else { (0.0, -1.0) };
            // From the vertical edge along the line, round the corner, out to the side.
            cr.move_to(cx, ey);
            cr.line_to(cx, cy + sy * radius);
            let (centre_x, centre_y) = (cx + sx * radius, cy + sy * radius);
            let start = if right { PI } else { 0.0 };
            let end = if down { 1.5 * PI } else { 0.5 * PI };
            if right == down {
                cr.arc(centre_x, centre_y, radius, start, end);
            } else {
                cr.arc_negative(centre_x, centre_y, radius, start, end);
            }
            cr.line_to(ex, cy);
            cr.set_line_width(t);
            let _ = cr.stroke();
        }
        Shape::Blocks(blocks) => {
            let at = |eighths: u8, size: i32| (f64::from(size) * f64::from(eighths) / 8.0).round();
            for &[x0, y0, x1, y1] in blocks {
                let (left, top) = (at(x0, width), at(y0, height));
                cr.rectangle(left, top, at(x1, width) - left, at(y1, height) - top);
            }
            let _ = cr.fill();
        }
        Shape::Shade(alpha) => {
            cr.set_source_rgba(1.0, 1.0, 1.0, alpha);
            cr.rectangle(0.0, 0.0, w, h);
            let _ = cr.fill();
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coverage of each pixel of `c` drawn in a 9 by 18 cell.
    fn coverage(c: char) -> Vec<Vec<u8>> {
        let (width, height) = (9, 18);
        let mut surface =
            cairo::ImageSurface::create(cairo::Format::ARgb32, width, height).unwrap();
        {
            let cr = cairo::Context::new(&surface).unwrap();
            assert!(draw(c, &cr, width, height, 1));
        }
        let stride = surface.stride() as usize;
        let data = surface.data().unwrap();
        (0..height as usize)
            .map(|y| {
                (0..width as usize)
                    .map(|x| data[y * stride + x * 4 + 3])
                    .collect()
            })
            .collect()
    }

    #[test]
    fn lines_reach_the_edges_they_join() {
        let cross = coverage('┼');
        assert_eq!(cross[8][0], 255, "left edge");
        assert_eq!(cross[8][8], 255, "right edge");
        assert_eq!(cross[0][4], 255, "top edge");
        assert_eq!(cross[17][4], 255, "bottom edge");
        assert_eq!(cross[0][0], 0);

        let corner = coverage('┌');
        assert_eq!(corner[8][8], 255);
        assert_eq!(corner[17][4], 255);
        assert_eq!(corner[8][0], 0, "nothing to the left");
        assert_eq!(corner[0][4], 0, "nothing above");
    }

    #[test]
    fn half_blocks_split_the_cell() {
        let upper = coverage('▀');
        assert_eq!(upper[0][0], 255);
        assert_eq!(upper[8][8], 255);
        assert_eq!(upper[9][0], 0);
        assert!((127..=128).contains(&coverage('▒')[5][5]));
    }

    #[test]
    fn letters_are_left_to_the_font() {
        assert!(!is_drawn('a'));
        assert!(!is_drawn('═'));
        assert!(is_drawn('╭'));
    }
}
