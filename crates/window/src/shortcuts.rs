//! Shortcuts: keys that act on the window instead of going to the terminal, Caret's own
//! unless the `[keys]` table of `config.toml` binds others.

use std::collections::HashMap;

use caret_protocol::encode::Modifiers;
use smithay_client_toolkit::seat::keyboard::Keysym;
use xkbcommon::xkb;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    NewWindow,
    CloseWindow,
    Copy,
    Paste,
    SelectAll,
    Deselect,
    ZoomIn,
    ZoomOut,
    ZoomReset,
    Fullscreen,
    TitleBar,
}

/// Each action, its name in `config.toml`, and the keys it has unless bound to others.
const ACTIONS: &[(Action, &str, &[&str])] = &[
    (Action::NewWindow, "new-window", &["Ctrl+Shift+N"]),
    (Action::CloseWindow, "close-window", &["Ctrl+Shift+Q"]),
    (Action::Copy, "copy", &["Ctrl+Shift+C"]),
    (Action::Paste, "paste", &["Ctrl+Shift+V"]),
    (Action::SelectAll, "select-all", &["Ctrl+Shift+A"]),
    (Action::Deselect, "deselect", &["Ctrl+Shift+D"]),
    (
        Action::ZoomIn,
        "zoom-in",
        &["Ctrl+plus", "Ctrl+equal", "Ctrl+KP_Add"],
    ),
    (
        Action::ZoomOut,
        "zoom-out",
        &["Ctrl+minus", "Ctrl+KP_Subtract"],
    ),
    (Action::ZoomReset, "zoom-reset", &["Ctrl+0", "Ctrl+KP_0"]),
    (Action::Fullscreen, "fullscreen", &["F11"]),
    (Action::TitleBar, "title-bar", &["Ctrl+Shift+H"]),
];

impl Action {
    /// The action named `name` in `config.toml`.
    pub fn named(name: &str) -> Option<Self> {
        ACTIONS
            .iter()
            .find(|(_, known, _)| *known == name)
            .map(|(action, ..)| *action)
    }
}

/// A key and the modifiers held with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Shortcut {
    key: Keysym,
    ctrl: bool,
    shift: bool,
    alt: bool,
}

impl Shortcut {
    /// The shortcut written as modifiers and a key name joined by `+`, such as
    /// `"Ctrl+Shift+T"` or `"F11"`.
    pub fn parse(text: &str) -> Option<Self> {
        let (modifiers, name) = text.trim().rsplit_once('+').unwrap_or(("", text.trim()));
        let key = xkb::keysym_from_name(name, xkb::KEYSYM_CASE_INSENSITIVE);
        if key == Keysym::NoSymbol {
            return None;
        }
        let mut held = Modifiers::default();
        for modifier in modifiers.split('+').filter(|modifier| !modifier.is_empty()) {
            match modifier.to_ascii_lowercase().as_str() {
                "ctrl" | "control" => held.ctrl = true,
                "shift" => held.shift = true,
                "alt" => held.alt = true,
                _ => return None,
            }
        }
        Some(Self::pressed(key, held))
    }

    /// The shortcut a key pressed with `held` makes. Letters count the same in either
    /// case, and Shift does not count for the symbol it typed, so that `"Ctrl+plus"` is
    /// pressed with Shift where the plus sign is shifted.
    pub fn pressed(key: Keysym, held: Modifiers) -> Self {
        let typed = char::from_u32(xkb::keysym_to_utf32(key)).filter(|c| !c.is_control());
        let (key, shift) = match typed {
            Some(c) if c.is_alphabetic() => (
                Keysym::from_char(c.to_lowercase().next().unwrap_or(c)),
                held.shift,
            ),
            Some(_) => (key, false),
            None => (key, held.shift),
        };
        Self {
            key,
            ctrl: held.ctrl,
            shift,
            alt: held.alt,
        }
    }
}

pub struct Shortcuts(HashMap<Shortcut, Action>);

impl Shortcuts {
    /// Caret's shortcuts, with those of the actions in `bound` replaced.
    pub fn new(bound: &HashMap<Action, Vec<Shortcut>>) -> Self {
        let mut shortcuts = HashMap::new();
        for (action, _, keys) in ACTIONS {
            match bound.get(action) {
                Some(keys) => {
                    shortcuts.extend(keys.iter().map(|&shortcut| (shortcut, *action)));
                }
                None => shortcuts.extend(
                    keys.iter()
                        .filter_map(|key| Shortcut::parse(key))
                        .map(|shortcut| (shortcut, *action)),
                ),
            }
        }
        Self(shortcuts)
    }

    /// The action a key pressed with `held` does, if any.
    pub fn action(&self, key: Keysym, held: Modifiers) -> Option<Action> {
        self.0.get(&Shortcut::pressed(key, held)).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CTRL_SHIFT: Modifiers = Modifiers {
        shift: true,
        ctrl: true,
        alt: false,
    };

    #[test]
    fn every_default_shortcut_parses() {
        for (_, name, keys) in ACTIONS {
            for key in *keys {
                assert!(Shortcut::parse(key).is_some(), "{name}: {key}");
            }
        }
    }

    #[test]
    fn letters_match_in_either_case_and_symbols_without_their_shift() {
        let shortcuts = Shortcuts::new(&HashMap::new());
        assert_eq!(shortcuts.action(Keysym::C, CTRL_SHIFT), Some(Action::Copy));
        assert_eq!(shortcuts.action(Keysym::c, Modifiers::default()), None);
        assert_eq!(
            shortcuts.action(Keysym::plus, CTRL_SHIFT),
            Some(Action::ZoomIn)
        );
        assert_eq!(
            shortcuts.action(Keysym::F11, Modifiers::default()),
            Some(Action::Fullscreen)
        );
    }

    #[test]
    fn binding_an_action_replaces_its_keys() {
        let bound = HashMap::from([(
            Action::Copy,
            vec![Shortcut::parse("Ctrl+Insert").expect("a key")],
        )]);
        let shortcuts = Shortcuts::new(&bound);
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(shortcuts.action(Keysym::Insert, ctrl), Some(Action::Copy));
        assert_eq!(shortcuts.action(Keysym::C, CTRL_SHIFT), None);
        assert_eq!(Shortcut::parse("Hyper+X"), None);
        assert_eq!(Shortcut::parse("Ctrl+NoSuchKey"), None);
    }
}
