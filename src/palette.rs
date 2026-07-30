//! The colours cells are drawn in: a light and a dark set of the 16 ANSI colours, the
//! 256-colour cube and ramp, and what programs changed through OSC 4, 10, 11 and 12.

use alacritty_terminal::term::color::Colors;
use alacritty_terminal::vte::ansi::{Color, NamedColor, Rgb};

const fn rgb(value: u32) -> Rgb {
    Rgb {
        r: (value >> 16) as u8,
        g: (value >> 8) as u8,
        b: value as u8,
    }
}

/// The accent the selection is tinted with, libadwaita's default blue.
const ACCENT: Rgb = rgb(0x3584e4);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Palette {
    pub foreground: Rgb,
    pub background: Rgb,
    pub ansi: [Rgb; 16],
    pub selection: Rgb,
}

impl Palette {
    pub fn dark() -> Self {
        let background = rgb(0x1d1d20);
        Self {
            foreground: rgb(0xffffff),
            background,
            ansi: [
                rgb(0x241f31),
                rgb(0xc01c28),
                rgb(0x2ec27e),
                rgb(0xf5c211),
                rgb(0x1e78e4),
                rgb(0x9841bb),
                rgb(0x0ab9dc),
                rgb(0xc0bfbc),
                rgb(0x5e5c64),
                rgb(0xed333b),
                rgb(0x57e389),
                rgb(0xf8e45c),
                rgb(0x51a1ff),
                rgb(0xc061cb),
                rgb(0x4fd2fd),
                rgb(0xf6f5f4),
            ],
            selection: mix(background, ACCENT, 0.45),
        }
    }

    pub fn light() -> Self {
        let background = rgb(0xffffff);
        Self {
            foreground: rgb(0x1d1d20),
            background,
            ansi: [
                rgb(0x241f31),
                rgb(0xc01c28),
                rgb(0x26a269),
                rgb(0xa67c00),
                rgb(0x1c71d8),
                rgb(0x813d9c),
                rgb(0x1a8a9c),
                rgb(0xdeddda),
                rgb(0x5e5c64),
                rgb(0xed333b),
                rgb(0x2ec27e),
                rgb(0xc88800),
                rgb(0x3584e4),
                rgb(0x9141ac),
                rgb(0x0ab9dc),
                rgb(0xf6f5f4),
            ],
            selection: mix(background, ACCENT, 0.3),
        }
    }

    /// `color` as the terminal shows it now, overrides from escape sequences first.
    pub fn resolve(&self, color: Color, overrides: &Colors) -> Rgb {
        match color {
            Color::Spec(rgb) => rgb,
            Color::Indexed(index) => self.indexed(usize::from(index), overrides),
            Color::Named(name) => self.named(name, overrides),
        }
    }

    fn indexed(&self, index: usize, overrides: &Colors) -> Rgb {
        if let Some(rgb) = overrides[index] {
            return rgb;
        }
        match index {
            0..16 => self.ansi[index],
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

    fn named(&self, name: NamedColor, overrides: &Colors) -> Rgb {
        let index = name as usize;
        if index < 16 {
            return self.indexed(index, overrides);
        }
        if let Some(rgb) = overrides[index] {
            return rgb;
        }
        match name {
            NamedColor::Background => self.background,
            NamedColor::Cursor | NamedColor::Foreground | NamedColor::BrightForeground => {
                overrides[NamedColor::Foreground].unwrap_or(self.foreground)
            }
            NamedColor::DimForeground => self.dim(self.named(NamedColor::Foreground, overrides)),
            _ => {
                let normal = index - NamedColor::DimBlack as usize;
                self.dim(self.indexed(normal, overrides))
            }
        }
    }

    /// Faint text: pulled towards the background, so it fades in the light style as well
    /// as in the dark one.
    pub fn dim(&self, color: Rgb) -> Rgb {
        mix(color, self.background, 0.4)
    }
}

impl Default for Palette {
    fn default() -> Self {
        Self::dark()
    }
}

/// `from` moved `amount` of the way to `to`.
pub fn mix(from: Rgb, to: Rgb, amount: f32) -> Rgb {
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
        let palette = Palette::dark();
        let none = Colors::default();
        assert_eq!(palette.resolve(Color::Indexed(16), &none), rgb(0x000000));
        assert_eq!(palette.resolve(Color::Indexed(196), &none), rgb(0xff0000));
        assert_eq!(palette.resolve(Color::Indexed(231), &none), rgb(0xffffff));
        assert_eq!(palette.resolve(Color::Indexed(232), &none), rgb(0x080808));
        assert_eq!(palette.resolve(Color::Indexed(255), &none), rgb(0xeeeeee));
    }

    #[test]
    fn a_colour_set_by_a_program_wins() {
        let palette = Palette::light();
        let mut colors = Colors::default();
        colors[1] = Some(rgb(0x123456));
        colors[NamedColor::Background] = Some(rgb(0x000000));
        assert_eq!(
            palette.resolve(Color::Named(NamedColor::Red), &colors),
            rgb(0x123456)
        );
        assert_eq!(palette.resolve(Color::Indexed(1), &colors), rgb(0x123456));
        assert_eq!(
            palette.resolve(Color::Named(NamedColor::Background), &colors),
            rgb(0x000000)
        );
    }

    #[test]
    fn dim_colours_fade_towards_the_background() {
        let palette = Palette::dark();
        let none = Colors::default();
        let dim = palette.resolve(Color::Named(NamedColor::DimForeground), &none);
        assert_eq!(dim, mix(palette.foreground, palette.background, 0.4));
        let dim_red = palette.resolve(Color::Named(NamedColor::DimRed), &none);
        assert_eq!(dim_red, mix(palette.ansi[1], palette.background, 0.4));
    }
}
