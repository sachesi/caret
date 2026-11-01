//! The colours a terminal's cells resolve to: the 16 its client chose, the 256-colour cube
//! and grey ramp, and what programs changed through escape sequences.

use crate::style::Color;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}

impl Rgb {
    #[must_use]
    pub const fn from_u32(value: u32) -> Self {
        Self {
            r: (value >> 16) as u8,
            g: (value >> 8) as u8,
            b: value as u8,
        }
    }
}

/// The colours a program can ask for or change by number: the 256, then the default
/// foreground, background and cursor.
pub const FOREGROUND: usize = 256;
pub const BACKGROUND: usize = 257;
pub const CURSOR: usize = 258;
pub const SLOTS: usize = 259;

/// What a terminal's colours are before programs change them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Base {
    pub foreground: Rgb,
    pub background: Rgb,
    /// None: the cursor takes the colour of the text under it.
    pub cursor: Option<Rgb>,
    pub ansi: [Rgb; 16],
}

impl Base {
    /// Whether the background is dark, by how bright it looks.
    #[must_use]
    pub fn is_dark(&self) -> bool {
        let Rgb { r, g, b } = self.background;
        u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114 < 128_000
    }
}

impl Default for Base {
    fn default() -> Self {
        Self {
            foreground: Rgb::from_u32(0xffffff),
            background: Rgb::from_u32(0x1d1d20),
            cursor: None,
            ansi: [
                0x241f31, 0xc01c28, 0x2ec27e, 0xf5c211, 0x1e78e4, 0x9841bb, 0x0ab9dc, 0xc0bfbc,
                0x5e5c64, 0xed333b, 0x57e389, 0xf8e45c, 0x51a1ff, 0xc061cb, 0x4fd2fd, 0xf6f5f4,
            ]
            .map(Rgb::from_u32),
        }
    }
}

/// The base colours and what programs changed of them.
#[derive(Debug, Clone)]
pub struct Palette {
    base: Base,
    overrides: Box<[Option<Rgb>; SLOTS]>,
}

impl Default for Palette {
    fn default() -> Self {
        Self {
            base: Base::default(),
            overrides: Box::new([None; SLOTS]),
        }
    }
}

impl Palette {
    pub fn set_base(&mut self, base: Base) {
        self.base = base;
    }

    #[must_use]
    pub fn base(&self) -> &Base {
        &self.base
    }

    /// The colour in `slot` now, a program's change first. The cursor's is None when it
    /// takes the colour of the text under it.
    #[must_use]
    pub fn slot(&self, slot: usize) -> Option<Rgb> {
        if let Some(color) = self.overrides.get(slot).copied().flatten() {
            return Some(color);
        }
        Some(match slot {
            0..16 => self.base.ansi[slot],
            16..232 => {
                const LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];
                let cube = slot - 16;
                Rgb {
                    r: LEVELS[cube / 36],
                    g: LEVELS[cube / 6 % 6],
                    b: LEVELS[cube % 6],
                }
            }
            232..256 => {
                let level = 8 + 10 * (slot - 232) as u8;
                Rgb {
                    r: level,
                    g: level,
                    b: level,
                }
            }
            FOREGROUND => self.base.foreground,
            BACKGROUND => self.base.background,
            _ => return self.base.cursor,
        })
    }

    /// `color` used as a foreground (`background` false) or a background.
    #[must_use]
    pub fn resolve(&self, color: Color, background: bool) -> Rgb {
        match color {
            Color::Rgb(r, g, b) => Rgb { r, g, b },
            Color::Indexed(index) => self.slot(usize::from(index)).unwrap_or_default(),
            Color::Default if background => self.slot(BACKGROUND).unwrap_or_default(),
            Color::Default => self.slot(FOREGROUND).unwrap_or_default(),
        }
    }

    pub fn set(&mut self, slot: usize, color: Rgb) {
        if let Some(entry) = self.overrides.get_mut(slot) {
            *entry = Some(color);
        }
    }

    pub fn reset(&mut self, slot: usize) {
        if let Some(entry) = self.overrides.get_mut(slot) {
            *entry = None;
        }
    }

    pub fn reset_all(&mut self) {
        self.overrides.fill(None);
    }
}

/// A colour as X11 writes it, `rgb:RR/GG/BB` with one to four hex digits each, or
/// `#RGB` with one to four digits each.
#[must_use]
pub fn parse(spec: &[u8]) -> Option<Rgb> {
    let spec = std::str::from_utf8(spec).ok()?;
    let scaled = |digits: &str| {
        if digits.is_empty() || digits.len() > 4 {
            return None;
        }
        let value = u32::from_str_radix(digits, 16).ok()?;
        let max = (1 << (4 * digits.len())) - 1;
        Some(((value * 255 + max / 2) / max) as u8)
    };
    if let Some(rest) = spec.strip_prefix("rgb:") {
        let mut parts = rest.split('/');
        let (r, g, b) = (parts.next()?, parts.next()?, parts.next()?);
        if parts.next().is_some() {
            return None;
        }
        return Some(Rgb {
            r: scaled(r)?,
            g: scaled(g)?,
            b: scaled(b)?,
        });
    }
    let digits = spec.strip_prefix('#')?;
    if digits.is_empty() || digits.len() % 3 != 0 || digits.len() > 12 || !digits.is_ascii() {
        return None;
    }
    let each = digits.len() / 3;
    // The high digits of each, as X11 reads `#` colours.
    let high = |part: &str| {
        let value = u32::from_str_radix(part, 16).ok()?;
        Some(match each {
            1 => value << 4,
            2 => value,
            3 => value >> 4,
            _ => value >> 8,
        } as u8)
    };
    Some(Rgb {
        r: high(&digits[..each])?,
        g: high(&digits[each..2 * each])?,
        b: high(&digits[2 * each..])?,
    })
}

/// `color` as replies to colour queries write it.
#[must_use]
pub fn format(color: Rgb) -> String {
    format!(
        "rgb:{0:02x}{0:02x}/{1:02x}{1:02x}/{2:02x}{2:02x}",
        color.r, color.g, color.b
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_in_both_forms_are_read() {
        let orange = Rgb {
            r: 0xff,
            g: 0x80,
            b: 0x00,
        };
        assert_eq!(parse(b"rgb:ff/80/00"), Some(orange));
        assert_eq!(parse(b"rgb:ffff/8080/0000"), Some(orange));
        assert_eq!(parse(b"#ff8000"), Some(orange));
        assert_eq!(
            parse(b"#f80"),
            Some(Rgb {
                r: 0xf0,
                g: 0x80,
                b: 0
            })
        );
        assert_eq!(parse(b"rgb:f/8/0").map(|color| color.r), Some(0xff));
        assert_eq!(parse(b"red"), None);
        assert_eq!(parse(b"rgb:ff/80"), None);
    }

    #[test]
    fn the_cube_and_ramp_follow_the_sixteen() {
        let palette = Palette::default();
        assert_eq!(palette.slot(16), Some(Rgb::default()));
        assert_eq!(palette.slot(231), Some(Rgb::from_u32(0xffffff)));
        assert_eq!(palette.slot(232), Some(Rgb::from_u32(0x080808)));
        assert_eq!(palette.slot(CURSOR), None);
    }

    #[test]
    fn changes_go_over_the_base_until_reset() {
        let mut palette = Palette::default();
        palette.set(1, Rgb::from_u32(0x123456));
        assert_eq!(
            palette.resolve(Color::Indexed(1), false),
            Rgb::from_u32(0x123456)
        );
        palette.reset(1);
        assert_eq!(palette.slot(1), Some(Base::default().ansi[1]));
        assert_eq!(format(Rgb::from_u32(0x0a0b0c)), "rgb:0a0a/0b0b/0c0c");
    }
}
