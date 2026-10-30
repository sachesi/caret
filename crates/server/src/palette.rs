//! The colours a client shows terminals in, as the emulator takes them, and its colours
//! as the protocol sends them.

use caret_protocol::{Palette, Rgb};
use caret_term::palette::{self as term_palette, Base};

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

fn term_rgb(color: Rgb) -> term_palette::Rgb {
    term_palette::Rgb {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

pub fn rgb(color: term_palette::Rgb) -> Rgb {
    Rgb {
        r: color.r,
        g: color.g,
        b: color.b,
    }
}

/// A client's palette as the emulator's base colours; the cursor takes the colour of the
/// text, and colours a client left out the emulator's own.
pub fn base(palette: &Palette) -> Base {
    let mut base = Base {
        foreground: term_rgb(palette.foreground),
        background: term_rgb(palette.background),
        cursor: None,
        ..Base::default()
    };
    for (slot, &color) in base.ansi.iter_mut().zip(&palette.ansi) {
        *slot = term_rgb(color);
    }
    base
}

/// Faint text: pulled towards the background, so it fades in a light palette as well as
/// in a dark one.
pub fn dim(color: Rgb, background: Rgb) -> Rgb {
    mix(color, background, 0.4)
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
    fn a_clients_colours_become_the_base() {
        let mut palette = fallback();
        palette.ansi.truncate(8);
        let base = base(&palette);
        assert_eq!(rgb(base.background), palette.background);
        assert_eq!(rgb(base.ansi[1]), palette.ansi[1]);
        assert_eq!(base.ansi[9], Base::default().ansi[9]);
    }

    #[test]
    fn dim_colours_fade_towards_the_background() {
        let palette = fallback();
        let faded = dim(palette.foreground, palette.background);
        assert_eq!(faded, mix(palette.foreground, palette.background, 0.4));
        assert_ne!(faded, palette.foreground);
    }
}
