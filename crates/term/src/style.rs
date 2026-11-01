//! What a cell holds: its character, colours, attributes and link.

/// A colour as a program gave it, resolved to red, green and blue by whoever draws it. It is
/// held as one number so that styles compare in one step: every cell's is compared as
/// history is kept, which takes a quarter of the time output goes through otherwise.
#[derive(Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Color(u32);

/// What a [`Color`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorKind {
    /// The default foreground or background, whichever the colour is used as.
    Default,
    /// One of the 256 colours of the palette.
    Indexed(u8),
    Rgb(u8, u8, u8),
}

impl Color {
    pub const DEFAULT: Self = Self(0);

    #[must_use]
    pub const fn indexed(index: u8) -> Self {
        Self(1 << 24 | index as u32)
    }

    #[must_use]
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self(2 << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32)
    }

    #[must_use]
    pub const fn kind(self) -> ColorKind {
        match self.0 >> 24 {
            0 => ColorKind::Default,
            1 => ColorKind::Indexed(self.0 as u8),
            _ => ColorKind::Rgb((self.0 >> 16) as u8, (self.0 >> 8) as u8, self.0 as u8),
        }
    }
}

impl std::fmt::Debug for Color {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.kind().fmt(f)
    }
}

/// A cell's attributes, a bit each but for the kind of underline.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Flags(u16);

impl Flags {
    pub const BOLD: Self = Self(1);
    pub const DIM: Self = Self(1 << 1);
    pub const ITALIC: Self = Self(1 << 2);
    pub const BLINK: Self = Self(1 << 3);
    pub const INVERSE: Self = Self(1 << 4);
    pub const HIDDEN: Self = Self(1 << 5);
    pub const STRIKEOUT: Self = Self(1 << 6);
    /// The first half of a character two cells wide.
    pub const WIDE: Self = Self(1 << 7);
    /// The second half of a character two cells wide, which draws nothing of its own.
    pub const SPACER: Self = Self(1 << 8);
    /// Marks follow the cell's character: its whole text is in its row's clusters.
    pub const CLUSTER: Self = Self(1 << 9);
    /// Three bits for the underline, from `Underline`.
    const UNDERLINE_SHIFT: u16 = 10;
    const UNDERLINE_MASK: u16 = 0b111 << Self::UNDERLINE_SHIFT;

    #[must_use]
    pub const fn empty() -> Self {
        Self(0)
    }

    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    #[must_use]
    pub const fn intersects(self, other: Self) -> bool {
        self.0 & other.0 != 0
    }

    pub fn insert(&mut self, other: Self) {
        self.0 |= other.0;
    }

    pub fn remove(&mut self, other: Self) {
        self.0 &= !other.0;
    }

    pub fn set(&mut self, other: Self, on: bool) {
        if on {
            self.insert(other);
        } else {
            self.remove(other);
        }
    }

    #[must_use]
    pub const fn underline(self) -> Underline {
        match (self.0 & Self::UNDERLINE_MASK) >> Self::UNDERLINE_SHIFT {
            1 => Underline::Single,
            2 => Underline::Double,
            3 => Underline::Curly,
            4 => Underline::Dotted,
            5 => Underline::Dashed,
            _ => Underline::None,
        }
    }

    pub fn set_underline(&mut self, underline: Underline) {
        self.0 = (self.0 & !Self::UNDERLINE_MASK) | ((underline as u16) << Self::UNDERLINE_SHIFT);
    }
}

impl std::ops::BitOr for Flags {
    type Output = Self;

    fn bitor(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum Underline {
    #[default]
    None = 0,
    Single = 1,
    Double = 2,
    Curly = 3,
    Dotted = 4,
    Dashed = 5,
}

/// The number of a link in its terminal's table; zero for none.
pub type LinkId = u16;

/// How text is drawn: what SGR sets, and the link OSC 8 gives it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Style {
    pub fg: Color,
    pub bg: Color,
    /// The underline's colour; the default is the foreground's.
    pub underline: Color,
    pub flags: Flags,
    pub link: LinkId,
}

impl Style {
    /// The attributes that carry over to blank cells an erase leaves: only the background,
    /// as terminals that erase in the current background do.
    #[must_use]
    pub fn erased(self) -> Self {
        Self {
            bg: self.bg,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Cell {
    pub c: char,
    pub style: Style,
}

impl Default for Cell {
    fn default() -> Self {
        Self::blank(Style::default())
    }
}

impl Cell {
    #[must_use]
    pub fn blank(style: Style) -> Self {
        Self { c: ' ', style }
    }

    /// Whether the cell shows nothing: a space without a background, underline or link.
    #[must_use]
    pub fn is_blank(&self) -> bool {
        self.c == ' ' && self.style == Style::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cells_stay_small() {
        assert_eq!(size_of::<Cell>(), 20);
    }

    #[test]
    fn a_color_gives_back_what_it_was_made_of() {
        assert_eq!(Color::default(), Color::DEFAULT);
        assert_eq!(Color::DEFAULT.kind(), ColorKind::Default);
        assert_eq!(Color::indexed(0).kind(), ColorKind::Indexed(0));
        assert_eq!(Color::indexed(255).kind(), ColorKind::Indexed(255));
        assert_eq!(Color::rgb(0, 0, 0).kind(), ColorKind::Rgb(0, 0, 0));
        assert_eq!(Color::rgb(1, 2, 3).kind(), ColorKind::Rgb(1, 2, 3));
        assert_ne!(Color::indexed(0), Color::DEFAULT);
        assert_ne!(Color::rgb(0, 0, 0), Color::indexed(0));
    }

    #[test]
    fn underline_kinds_share_their_bits() {
        let mut flags = Flags::BOLD;
        flags.set_underline(Underline::Curly);
        assert_eq!(flags.underline(), Underline::Curly);
        flags.set_underline(Underline::Single);
        assert_eq!(flags.underline(), Underline::Single);
        assert!(flags.contains(Flags::BOLD));
        flags.set_underline(Underline::None);
        assert_eq!(flags, Flags::BOLD);
    }
}
