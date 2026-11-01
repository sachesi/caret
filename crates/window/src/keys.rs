//! Keys as xkbcommon names them, turned into the keys the terminal encodes for the
//! program.

use caret_protocol::encode::Key;
use smithay_client_toolkit::seat::keyboard::Keysym;
use xkbcommon::xkb;

/// The terminal's key for `keysym`: a named key, or the character it types.
pub fn key(keysym: Keysym) -> Option<Key> {
    named(keysym).or_else(|| keysym.key_char().filter(|c| !c.is_control()).map(Key::Char))
}

/// The terminal's key for a key that types no character.
pub fn named(keysym: Keysym) -> Option<Key> {
    let named = match keysym {
        Keysym::Return | Keysym::KP_Enter => Key::Enter,
        Keysym::Tab | Keysym::ISO_Left_Tab | Keysym::KP_Tab => Key::Tab,
        Keysym::BackSpace => Key::Backspace,
        Keysym::Escape => Key::Escape,
        Keysym::Up | Keysym::KP_Up => Key::Up,
        Keysym::Down | Keysym::KP_Down => Key::Down,
        Keysym::Right | Keysym::KP_Right => Key::Right,
        Keysym::Left | Keysym::KP_Left => Key::Left,
        Keysym::Home | Keysym::KP_Home => Key::Home,
        Keysym::End | Keysym::KP_End => Key::End,
        Keysym::Insert | Keysym::KP_Insert => Key::Insert,
        Keysym::Delete | Keysym::KP_Delete => Key::Delete,
        Keysym::Page_Up | Keysym::KP_Page_Up => Key::PageUp,
        Keysym::Page_Down | Keysym::KP_Page_Down => Key::PageDown,
        _ => {
            let raw = keysym.raw();
            if (Keysym::F1.raw()..=Keysym::F35.raw()).contains(&raw) {
                return u8::try_from(raw - Keysym::F1.raw() + 1)
                    .ok()
                    .map(Key::Function);
            }
            return None;
        }
    };
    Some(named)
}

/// The keymap's layouts, to find what a key types in each.
pub struct Layouts(xkb::Keymap);

impl Layouts {
    /// The keymap the compositor sent, in xkb's text format.
    pub fn new(text: String) -> Option<Self> {
        let context = xkb::Context::new(xkb::CONTEXT_NO_FLAGS);
        xkb::Keymap::new_from_string(
            &context,
            text,
            xkb::KEYMAP_FORMAT_TEXT_V1,
            xkb::KEYMAP_COMPILE_NO_FLAGS,
        )
        .map(Self)
    }

    /// What the key of Wayland key code `code` types unshifted in the first layout in
    /// which that is a Latin character.
    pub fn latin(&self, code: u32) -> Option<Keysym> {
        let key = xkb::Keycode::new(code + 8);
        (0..self.0.num_layouts_for_key(key)).find_map(|layout| {
            self.0
                .key_get_syms_by_level(key, layout, 0)
                .iter()
                .copied()
                .find(|keysym| keysym.key_char().is_some_and(|c| c.is_ascii_graphic()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_that_types_a_character_has_no_name() {
        for keysym in [
            Keysym::a,
            Keysym::A,
            Keysym::exclam,
            Keysym::less,
            Keysym::space,
        ] {
            assert_eq!(named(keysym), None, "{keysym:?}");
            assert!(matches!(key(keysym), Some(Key::Char(_))), "{keysym:?}");
        }
        assert_eq!(named(Keysym::Return), Some(Key::Enter));
        assert_eq!(named(Keysym::F5), Some(Key::Function(5)));
    }

    #[test]
    fn named_keys_function_keys_and_characters() {
        assert_eq!(key(Keysym::KP_Enter), Some(Key::Enter));
        assert_eq!(key(Keysym::F5), Some(Key::Function(5)));
        assert_eq!(key(Keysym::A), Some(Key::Char('A')));
        assert_eq!(key(Keysym::Cyrillic_a), Some(Key::Char('а')));
        assert_eq!(key(Keysym::Shift_L), None);
    }

    #[test]
    fn a_key_of_a_cyrillic_layout_types_the_latin_letter_of_the_next() {
        let keymap = r#"xkb_keymap {
            xkb_keycodes { minimum = 8; maximum = 255; <AB03> = 62; <AB04> = 63; };
            xkb_types { type "ONE_LEVEL" { modifiers = none; level_name[1] = "Any"; }; };
            xkb_compat { };
            xkb_symbols {
                key <AB03> { symbols[Group1] = [ Cyrillic_es ], symbols[Group2] = [ c ] };
                key <AB04> { symbols[Group1] = [ Cyrillic_em ] };
            };
        };"#;
        let layouts = Layouts::new(keymap.to_owned()).expect("the keymap compiles");
        assert_eq!(layouts.latin(54), Some(Keysym::c));
        assert_eq!(layouts.latin(55), None);
    }
}
