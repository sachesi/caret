//! The colours cells are drawn in: the 16 ANSI colours, which the server resolves the
//! cells' colours with, and the selection's.

use caret_protocol::Rgb;

const fn rgb(value: u32) -> Rgb {
    Rgb::from_u32(value)
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

    /// The colours the server shows cells in.
    pub fn colors(&self) -> caret_protocol::Palette {
        caret_protocol::Palette {
            foreground: self.foreground,
            background: self.background,
            ansi: self.ansi.to_vec(),
        }
    }
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
