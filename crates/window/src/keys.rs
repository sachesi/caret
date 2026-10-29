//! Keys as xkbcommon names them, turned into the keys the terminal encodes for the
//! program.

use caret_protocol::encode::Key;
use smithay_client_toolkit::seat::keyboard::Keysym;

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
            return keysym.key_char().filter(|c| !c.is_control()).map(Key::Char);
        }
    };
    Some(named)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys_function_keys_and_characters() {
        assert_eq!(key(Keysym::KP_Enter), Some(Key::Enter));
        assert_eq!(key(Keysym::F5), Some(Key::Function(5)));
        assert_eq!(key(Keysym::A), Some(Key::Char('A')));
        assert_eq!(key(Keysym::Cyrillic_a), Some(Key::Char('а')));
        assert_eq!(key(Keysym::Shift_L), None);
    }
}
