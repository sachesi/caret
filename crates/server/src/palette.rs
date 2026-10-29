//! The colours cells are shown in: a client's 16 ANSI colours, the 256-colour cube and
//! ramp, and what programs changed through OSC 4, 10, 11 and 12.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{self, Color, NamedColor};
use caret_protocol::{Palette, Rgb};

pub fn rgb(color: ansi::Rgb) -> Rgb {
    Rgb {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

pub fn ansi_rgb(color: Rgb) -> ansi::Rgb {
    ansi::Rgb {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

/// The dark palette, for terminals no client has shown yet.
pub fn fallback() -> Palette {
    Palette {
        foreground: Rgb::from_u32(0xffffff),
        background: Rgb::from_u32(0x1d1d20),
        ansi: [
            0x241f31, 0xc01c28, 0x2ec27e, 0xf5c211, 0x1e78e4, 0x9841bb, 0x0ab9dc, 0xc0bfbc,
            0x5e5c64, 0xed333b, 0x57e389, 0xf8e45c, 0x51a1ff, 0xc061cb, 0x4fd2fd, 0xf6f5f4,
        ]
        .map(Rgb::from_u32)
        .to_vec(),
    }
}

/// `color` as the terminal shows it now, overrides from escape sequences first.
pub fn resolve(palette: &Palette, color: Color, overrides: &Colors) -> Rgb {
    match color {
        Color::Spec(color) => rgb(color),
        Color::Indexed(index) => indexed(palette, usize::from(index), overrides),
        Color::Named(name) => named(palette, name, overrides),
    }
}

fn indexed(palette: &Palette, index: usize, overrides: &Colors) -> Rgb {
    if let Some(color) = overrides[index] {
        return rgb(color);
    }
    match index {
        0..16 => palette.ansi.get(index).copied().unwrap_or_default(),
        16..232 => {
            const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
            let cube = index - 16;
            Rgb {
                r: LEVELS[cube / 36],
                g: LEVELS[cube / 6 % 6],
                b: LEVELS[cube % 6],
            }
        }
        _ => {
            let level = 8 + 10 * (index - 232) as u8;
            Rgb {
                r: level,
                g: level,
                b: level,
            }
        }
    }
}

fn named(palette: &Palette, name: NamedColor, overrides: &Colors) -> Rgb {
    let index = name as usize;
    if index < 16 {
        return indexed(palette, index, overrides);
    }
    if let Some(color) = overrides[index] {
        return rgb(color);
    }
    match name {
        NamedColor::Background => palette.background,
        NamedColor::Cursor | NamedColor::Foreground | NamedColor::BrightForeground => {
            overrides[NamedColor::Foreground].map_or(palette.foreground, rgb)
        }
        NamedColor::DimForeground => {
            dim(palette, named(palette, NamedColor::Foreground, overrides))
        }
        _ => {
            let normal = index - NamedColor::DimBlack as usize;
            dim(palette, indexed(palette, normal, overrides))
        }
    }
}

/// Faint text: pulled towards the background, so it fades in a light palette as well as
/// in a dark one.
pub fn dim(palette: &Palette, color: Rgb) -> Rgb {
    mix(color, palette.background, 0.4)
}

/// `from` moved `amount` of the way to `to`.
fn mix(from: Rgb, to: Rgb, amount: f32) -> Rgb {
    let channel =
        |a: u8, b: u8| (f32::from(a) + (f32::from(b) - f32::from(a)) * amount).round() as u8;
    Rgb {
        r: channel(from.r, to.r),
        g: channel(from.g, to.g),
        b: channel(from.b, to.b),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_cube_and_the_ramp_follow_xterm() {
        let palette = fallback();
        let none = Colors::default();
        let resolved = |index| resolve(&palette, Color::Indexed(index), &none);
        assert_eq!(resolved(16), Rgb::from_u32(0x000000));
        assert_eq!(resolved(196), Rgb::from_u32(0xff0000));
        assert_eq!(resolved(231), Rgb::from_u32(0xffffff));
        assert_eq!(resolved(232), Rgb::from_u32(0x080808));
        assert_eq!(resolved(255), Rgb::from_u32(0xeeeeee));
    }

    #[test]
    fn a_colour_set_by_a_program_wins() {
        let palette = fallback();
        let mut colors = Colors::default();
        colors[1] = Some(ansi_rgb(Rgb::from_u32(0x123456)));
        colors[NamedColor::Background] = Some(ansi::Rgb { r: 0, g: 0, b: 0 });
        let resolved = |color| resolve(&palette, color, &colors);
        assert_eq!(
            resolved(Color::Named(NamedColor::Red)),
            Rgb::from_u32(0x123456)
        );
        assert_eq!(resolved(Color::Indexed(1)), Rgb::from_u32(0x123456));
        assert_eq!(
            resolved(Color::Named(NamedColor::Background)),
            Rgb::from_u32(0x000000)
        );
    }

    #[test]
    fn dim_colours_fade_towards_the_background() {
        let palette = fallback();
        let none = Colors::default();
        let dim_text = resolve(&palette, Color::Named(NamedColor::DimForeground), &none);
        assert_eq!(dim_text, mix(palette.foreground, palette.background, 0.4));
        let dim_red = resolve(&palette, Color::Named(NamedColor::DimRed), &none);
        assert_eq!(dim_red, mix(palette.ansi[1], palette.background, 0.4));
    }
}
