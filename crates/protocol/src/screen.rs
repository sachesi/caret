//! What a client is sent of a terminal's screen: rows of cells already resolved to
//! colours and glyphs, the cursor, the selection and the modes that change what input
//! means.

use crate::wire::{DecodeError, Wire, byte, record, variants};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgb {
    pub r: u8,
    pub g: u8,
    pub b: u8,
}
record!(Rgb { r, g, b });

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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum GlyphText {
    Char(char),
    /// A character with combining marks after it.
    Cluster(Box<str>),
}

impl Wire for GlyphText {
    fn put(&self, out: &mut Vec<u8>) {
        match self {
            Self::Char(c) => {
                out.push(0);
                c.put(out);
            }
            Self::Cluster(text) => {
                out.push(1);
                text.put(out);
            }
        }
    }

    fn take(input: &mut &[u8]) -> Result<Self, DecodeError> {
        match byte(input)? {
            0 => char::take(input).map(Self::Char),
            1 => Box::<str>::take(input).map(Self::Cluster),
            tag => Err(DecodeError::UnknownTag {
                kind: "GlyphText",
                tag,
            }),
        }
    }
}

pub const BOLD: u8 = 1;
pub const ITALIC: u8 = 2;

/// What a cell shows, and how: the key glyphs are rasterised and cached by.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Glyph {
    pub text: GlyphText,
    /// [`BOLD`] and [`ITALIC`].
    pub style: u8,
    /// Two cells wide.
    pub wide: bool,
}
record!(Glyph { text, style, wide });

/// A set of flags in an integer, written as the integer.
macro_rules! flags {
    ($(#[$doc:meta])* $name:ident($bits:ty) { $($(#[$flag_doc:meta])* $flag:ident = $value:expr),* $(,)? }) => {
        $(#[$doc])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
        pub struct $name(pub $bits);

        impl $name {
            $($(#[$flag_doc])* pub const $flag: Self = Self($value);)*

            pub const fn empty() -> Self {
                Self(0)
            }

            pub const fn contains(self, other: Self) -> bool {
                self.0 & other.0 == other.0
            }

            pub const fn intersects(self, other: Self) -> bool {
                self.0 & other.0 != 0
            }

            pub fn insert(&mut self, other: Self) {
                self.0 |= other.0;
            }
        }

        impl std::ops::BitOr for $name {
            type Output = Self;

            fn bitor(self, other: Self) -> Self {
                Self(self.0 | other.0)
            }
        }

        impl Wire for $name {
            fn put(&self, out: &mut Vec<u8>) {
                self.0.put(out);
            }

            fn take(input: &mut &[u8]) -> Result<Self, DecodeError> {
                <$bits>::take(input).map(Self)
            }
        }
    };
}

flags! {
    /// Lines drawn with a cell's text.
    CellFlags(u16) {
        UNDERLINE = 1,
        DOUBLE_UNDERLINE = 1 << 1,
        UNDERCURL = 1 << 2,
        DOTTED_UNDERLINE = 1 << 3,
        DASHED_UNDERLINE = 1 << 4,
        STRIKEOUT = 1 << 5,
        ALL_UNDERLINES = 0b1_1111,
    }
}

flags! {
    /// What the program asked of the terminal that changes what keys, the mouse and
    /// pastes send it.
    Mode(u32) {
        APP_CURSOR = 1,
        APP_KEYPAD = 1 << 1,
        LINE_FEED_NEW_LINE = 1 << 2,
        MOUSE_REPORT_CLICK = 1 << 3,
        MOUSE_DRAG = 1 << 4,
        MOUSE_MOTION = 1 << 5,
        SGR_MOUSE = 1 << 6,
        UTF8_MOUSE = 1 << 7,
        BRACKETED_PASTE = 1 << 8,
        FOCUS_IN_OUT = 1 << 9,
        ALT_SCREEN = 1 << 10,
        ALTERNATE_SCROLL = 1 << 11,
        MOUSE_MODE = 0b11_1000,
    }
}

/// A cell with something to draw: a glyph, a background of its own or a line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub column: u16,
    pub glyph: Option<Glyph>,
    pub fg: Rgb,
    pub bg: Rgb,
    pub underline: Rgb,
    pub flags: CellFlags,
}
record!(Cell {
    column,
    glyph,
    fg,
    bg,
    underline,
    flags
});

/// A line of the screen: its cells with something to draw, left to right.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Row {
    pub line: u16,
    pub cells: Vec<Cell>,
}
record!(Row { line, cells });

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CursorShape {
    #[default]
    Block,
    Underline,
    Beam,
    HollowBlock,
}
variants!(CursorShape {
    0 => Block,
    1 => Underline,
    2 => Beam,
    3 => HollowBlock,
});

/// A cell of the screen, from the top left.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Hash)]
pub struct Point {
    pub line: u16,
    pub column: u16,
}
record!(Point { line, column });

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cursor {
    pub point: Point,
    pub shape: CursorShape,
    /// Over a wide character.
    pub wide: bool,
}
record!(Cursor { point, shape, wide });

/// The selection in cells of the screen, first and last included.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Selection {
    pub start: Point,
    pub end: Point,
    pub block: bool,
}
record!(Selection { start, end, block });

/// The address under the point a client hovers, as a click would open it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hovered {
    pub uri: String,
    /// A program marked the text as a link, so its text may say something else.
    pub marked: bool,
}
record!(Hovered { uri, marked });

/// The screen as it changed since the client's last frame.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Frame {
    pub columns: u16,
    pub lines: u16,
    /// Lines that changed; every line when the size did or for a client's first frame.
    pub rows: Vec<Row>,
    pub cursor: Option<Cursor>,
    pub cursor_blinks: bool,
    pub selection: Option<Selection>,
    pub background: Rgb,
    pub cursor_color: Rgb,
    pub mode: Mode,
    /// Lines of history above the screen, and how far up the view is scrolled into it.
    pub history: u32,
    pub offset: u32,
    pub hovered: Option<Hovered>,
}
record!(Frame {
    columns,
    lines,
    rows,
    cursor,
    cursor_blinks,
    selection,
    background,
    cursor_color,
    mode,
    history,
    offset,
    hovered,
});
