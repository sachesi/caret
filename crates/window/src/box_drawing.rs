//! Box-drawing characters and block elements, drawn to fill the cell exactly instead of
//! taken from the font, whose versions leave gaps between rows when the line is taller
//! than the glyphs. Braille patterns too, as few fonts have them and spinners and graphs
//! in terminals use them.

use swash::zeno::{Cap, Mask, PathBuilder, Stroke};

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

/// The column and row of each dot of a braille pattern, by its bit.
const BRAILLE_DOTS: [(i32, i32); 8] = [
    (0, 0),
    (0, 1),
    (0, 2),
    (1, 0),
    (1, 1),
    (1, 2),
    (0, 3),
    (1, 3),
];

enum Shape {
    Braille(u8),
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
        0x2800..=0x28ff => Shape::Braille((code - 0x2800) as u8),
        _ => return None,
    })
}

/// The coverage of `c` in a `width` by `height` cell, with lines `light` pixels thick,
/// a byte for each pixel; none for characters left to the font.
#[expect(
    clippy::many_single_char_names,
    reason = "cell geometry: width, height, centre, line widths"
)]
pub fn coverage(c: char, width: u32, height: u32, light: u32) -> Option<Vec<u8>> {
    let shape = shape(c)?;
    let (width, height, light) = (width as i32, height as i32, light as i32);
    let mut cover = vec![0u8; (width * height) as usize];
    let mut fill = |x0: i32, y0: i32, x1: i32, y1: i32, value: u8| {
        for y in y0.max(0)..y1.min(height) {
            for x in x0.max(0)..x1.min(width) {
                cover[(y * width + x) as usize] = value;
            }
        }
    };
    let heavy = (light * 5 + 1) / 2;
    let thickness = |weight: u8| match weight {
        0 => 0,
        1 => light,
        _ => heavy,
    };
    match shape {
        Shape::Lines([up, down, left, right]) => {
            let vertical = thickness(up).max(thickness(down));
            let horizontal = thickness(left).max(thickness(right));
            // Each arm runs from its edge to the far side of the stroke across it, so the
            // arms meet without a notch.
            let across = |own: i32, other: i32| if other > 0 { other } else { own };
            for (weight, is_left) in [(left, true), (right, false)] {
                let t = thickness(weight);
                if t == 0 {
                    continue;
                }
                let join = across(t, vertical);
                let y = (height - t) / 2;
                if is_left {
                    fill(0, y, (width - join) / 2 + join, y + t, 255);
                } else {
                    fill((width - join) / 2, y, width, y + t, 255);
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
                    fill(x, 0, x + t, (height - join) / 2 + join, 255);
                } else {
                    fill(x, (height - join) / 2, x + t, height, 255);
                }
            }
        }
        Shape::Arc { down, right } => {
            let (w, h, t) = (width as f32, height as f32, light as f32);
            let cx = ((width - light) / 2) as f32 + t / 2.0;
            let cy = ((height - light) / 2) as f32 + t / 2.0;
            let radius = (w / 2.0).min(h / 2.0);
            let (ey, sy) = if down { (h, 1.0) } else { (0.0, -1.0) };
            let (ex, sx) = if right { (w, 1.0) } else { (0.0, -1.0) };
            // A quarter circle as a cubic: the control points sit this far along the
            // tangents, as a fraction of the radius.
            let k = 0.552_284_8 * radius;
            let mut path = Vec::new();
            path.move_to([cx, ey])
                .line_to([cx, cy + sy * radius])
                .curve_to(
                    [cx, cy + sy * (radius - k)],
                    [cx + sx * (radius - k), cy],
                    [cx + sx * radius, cy],
                )
                .line_to([ex, cy]);
            let mut stroke = Stroke::new(t);
            stroke.cap(Cap::Butt);
            Mask::new(&path)
                .style(stroke)
                .size(width as u32, height as u32)
                .render_into(&mut cover, None);
        }
        Shape::Blocks(blocks) => {
            let at = |eighths: u8, size: i32| (size * i32::from(eighths) * 2 + 8) / 16;
            for &[x0, y0, x1, y1] in blocks {
                fill(
                    at(x0, width),
                    at(y0, height),
                    at(x1, width),
                    at(y1, height),
                    255,
                );
            }
        }
        Shape::Shade(alpha) => {
            fill(0, 0, width, height, (alpha * 255.0).round() as u8);
        }
        Shape::Braille(pattern) => {
            let size = (width / 4).max(1);
            for (bit, (column, row)) in BRAILLE_DOTS.into_iter().enumerate() {
                if pattern & (1 << bit) != 0 {
                    let x = width * (2 * column + 1) / 4 - size / 2;
                    let y = height * (2 * row + 1) / 8 - size / 2;
                    fill(x, y, x + size, y + size, 255);
                }
            }
        }
    }
    Some(cover)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Coverage of each pixel of `c` drawn in a 9 by 18 cell.
    fn rows(c: char) -> Vec<Vec<u8>> {
        let cover = coverage(c, 9, 18, 1).expect("drawn");
        cover.chunks(9).map(<[u8]>::to_vec).collect()
    }

    #[test]
    fn a_braille_pattern_fills_only_its_dots() {
        let dots = |c: char| {
            let mut dots = Vec::new();
            let drawn = rows(c);
            for (column, x) in [2, 6].into_iter().enumerate() {
                for (row, y) in [2, 6, 11, 15].into_iter().enumerate() {
                    if drawn[y][x] == 255 {
                        dots.push((column, row));
                    }
                }
            }
            dots
        };
        assert_eq!(dots('\u{2801}'), [(0, 0)]);
        assert_eq!(dots('\u{2840}'), [(0, 3)]);
        assert_eq!(dots('\u{2880}'), [(1, 3)]);
        assert_eq!(dots('\u{28ff}').len(), 8);
        assert!(rows('\u{2800}').iter().flatten().all(|&cover| cover == 0));
    }

    #[test]
    fn lines_reach_the_edges_they_join() {
        let cross = rows('┼');
        assert_eq!(cross[8][0], 255, "left edge");
        assert_eq!(cross[8][8], 255, "right edge");
        assert_eq!(cross[0][4], 255, "top edge");
        assert_eq!(cross[17][4], 255, "bottom edge");
        assert_eq!(cross[0][0], 0);

        let corner = rows('┌');
        assert_eq!(corner[8][8], 255);
        assert_eq!(corner[17][4], 255);
        assert_eq!(corner[8][0], 0, "nothing to the left");
        assert_eq!(corner[0][4], 0, "nothing above");
    }

    #[test]
    fn half_blocks_split_the_cell() {
        let upper = rows('▀');
        assert_eq!(upper[0][0], 255);
        assert_eq!(upper[8][8], 255);
        assert_eq!(upper[9][0], 0);
        assert!((127..=128).contains(&rows('▒')[5][5]));
    }

    #[test]
    fn rounded_corners_meet_the_lines_they_join() {
        let corner = rows('╭');
        assert!(corner[17][4] > 200, "down to the bottom edge");
        assert!(corner[8][8] > 200, "out to the right edge");
        assert_eq!(corner[0][4], 0, "nothing above");
        assert_eq!(corner[8][0], 0, "nothing to the left");
    }

    #[test]
    fn letters_are_left_to_the_font() {
        assert!(shape('a').is_none());
        assert!(shape('═').is_none());
        assert!(shape('╭').is_some());
    }
}
