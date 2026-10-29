//! The colours cells are drawn in: the 16 ANSI colours, which the server resolves the
//! cells' colours with, and the selection's. They come from a colour scheme, Caret's own
//! or one in a file of the user's.

use caret_protocol::Rgb;
use toml_edit::DocumentMut;

use crate::config;

/// The name of Caret's own colour scheme.
pub const OWN: &str = "caret";

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

    /// The colour scheme `name` in its dark or light form: Caret's own for [`OWN`], else
    /// the one in its file, with Caret's own standing in for one missing or wrong.
    pub fn named(name: &str, dark: bool) -> Self {
        let own = if dark { Self::dark() } else { Self::light() };
        if name == OWN {
            return own;
        }
        let Some(path) = config::scheme_path(name) else {
            return own;
        };
        let read = std::fs::read_to_string(&path).map_err(|error| error.to_string());
        match read.and_then(|text| Self::parse(&text, dark)) {
            Ok(palette) => palette,
            Err(error) => {
                tracing::warn!("{}: {error}", path.display());
                own
            }
        }
    }

    /// The scheme a file holds, its selection tinted as the `dark` style's or the
    /// light's when it sets none.
    fn parse(text: &str, dark: bool) -> Result<Self, String> {
        let document = text
            .parse::<DocumentMut>()
            .map_err(|error| error.to_string())?;
        let color = |key: &str| {
            document
                .get(key)
                .and_then(|item| hex(item.as_str()?))
                .ok_or_else(|| format!("`{key}` is not a colour such as \"#1d1d20\""))
        };
        let background = color("background")?;
        let ansi: Option<Vec<Rgb>> = document
            .get("palette")
            .and_then(|item| item.as_array())
            .and_then(|colors| colors.iter().map(|color| hex(color.as_str()?)).collect());
        let ansi = ansi
            .and_then(|ansi| <[Rgb; 16]>::try_from(ansi).ok())
            .ok_or("`palette` is not a list of 16 colours")?;
        let selection = if document.contains_key("selection") {
            color("selection")?
        } else {
            mix(background, ACCENT, if dark { 0.45 } else { 0.3 })
        };
        Ok(Self {
            foreground: color("foreground")?,
            background,
            ansi,
            selection,
        })
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

/// The colour `"#rrggbb"` names.
fn hex(text: &str) -> Option<Rgb> {
    let digits = text.strip_prefix('#').filter(|digits| digits.len() == 6)?;
    u32::from_str_radix(digits, 16).ok().map(Rgb::from_u32)
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

    const SCHEME: &str = "foreground = \"#eeeeee\"\nbackground = \"#101010\"\n\
        palette = [\"#000000\", \"#100000\", \"#200000\", \"#300000\", \"#400000\", \
        \"#500000\", \"#600000\", \"#700000\", \"#800000\", \"#900000\", \"#a00000\", \
        \"#b00000\", \"#c00000\", \"#d00000\", \"#e00000\", \"#ff0000\"]\n";

    #[test]
    fn a_scheme_file_gives_every_colour_and_tints_a_selection_it_lacks() {
        let palette = Palette::parse(SCHEME, true).expect("the scheme is whole");
        assert_eq!(palette.background, rgb(0x101010));
        assert_eq!(palette.ansi[15], rgb(0xff0000));
        assert_eq!(palette.selection, mix(rgb(0x101010), ACCENT, 0.45));
        let chosen = Palette::parse(&format!("{SCHEME}selection = \"#333333\"\n"), false);
        assert_eq!(chosen.map(|palette| palette.selection), Ok(rgb(0x333333)));
    }

    #[test]
    fn a_scheme_missing_a_colour_or_with_a_wrong_one_is_refused() {
        assert!(Palette::parse(&SCHEME.replace("#eeeeee", "white"), true).is_err());
        assert!(Palette::parse(&SCHEME.replace("\"#ff0000\"", ""), true).is_err());
        assert!(Palette::parse(&SCHEME.replace("foreground", "fg"), true).is_err());
        assert!(Palette::parse(&format!("{SCHEME}selection = 3\n"), true).is_err());
    }
}
