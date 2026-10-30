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
    NewTab,
    CloseTab,
    SplitRight,
    SplitDown,
    ClosePane,
    FocusLeft,
    FocusRight,
    FocusUp,
    FocusDown,
    /// Moves, resizes and closes panes from the keys until left.
    ControlMode,
    Find,
    NextTab,
    PreviousTab,
    /// Shows the tab of this number, counted from one.
    ShowTab(u8),
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
    (Action::NewTab, "new-tab", &["Ctrl+Shift+T"]),
    (Action::CloseTab, "close-tab", &[]),
    (Action::SplitRight, "split-right", &["Ctrl+Shift+E"]),
    (Action::SplitDown, "split-down", &["Ctrl+Shift+O"]),
    (Action::ClosePane, "close-pane", &["Ctrl+Shift+W"]),
    (Action::FocusLeft, "focus-left", &["Alt+Left"]),
    (Action::FocusRight, "focus-right", &["Alt+Right"]),
    (Action::FocusUp, "focus-up", &["Alt+Up"]),
    (Action::FocusDown, "focus-down", &["Alt+Down"]),
    (Action::ControlMode, "control-mode", &["Ctrl+Shift+space"]),
    (Action::Find, "find", &["Ctrl+Shift+F"]),
    (Action::NextTab, "next-tab", &["Ctrl+Page_Down", "Ctrl+Tab"]),
    (
        Action::PreviousTab,
        "previous-tab",
        &["Ctrl+Page_Up", "Ctrl+Shift+ISO_Left_Tab"],
    ),
    (Action::ShowTab(1), "tab-1", &["Alt+1"]),
    (Action::ShowTab(2), "tab-2", &["Alt+2"]),
    (Action::ShowTab(3), "tab-3", &["Alt+3"]),
    (Action::ShowTab(4), "tab-4", &["Alt+4"]),
    (Action::ShowTab(5), "tab-5", &["Alt+5"]),
    (Action::ShowTab(6), "tab-6", &["Alt+6"]),
    (Action::ShowTab(7), "tab-7", &["Alt+7"]),
    (Action::ShowTab(8), "tab-8", &["Alt+8"]),
    (Action::ShowTab(9), "tab-9", &["Alt+9"]),
];

/// Each action's name in `config.toml` and the keys it has unless bound to others.
pub fn defaults() -> impl Iterator<Item = (&'static str, &'static [&'static str])> {
    ACTIONS.iter().map(|(_, name, keys)| (*name, *keys))
}

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
    /// pressed with Shift where the plus sign is shifted; the space bar types the same
    /// with Shift, which counts.
    pub fn pressed(key: Keysym, held: Modifiers) -> Self {
        let typed = char::from_u32(xkb::keysym_to_utf32(key))
            .filter(|c| !c.is_control() && !c.is_whitespace());
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

    /// The shortcut as it is shown: its modifiers, then its key as typed or named.
    fn label(self) -> String {
        let typed = char::from_u32(xkb::keysym_to_utf32(self.key))
            .filter(|c| !c.is_control() && !c.is_whitespace());
        let key = match (typed, self.key) {
            (Some(c), _) => c.to_uppercase().collect(),
            (None, Keysym::Page_Down) => "Page Down".to_owned(),
            (None, Keysym::Page_Up) => "Page Up".to_owned(),
            (None, Keysym::ISO_Left_Tab) => "Tab".to_owned(),
            (None, key) => xkb::keysym_get_name(key).replace('_', " "),
        };
        let mut label = String::new();
        for (held, name) in [
            (self.ctrl, "Ctrl+"),
            (self.shift, "Shift+"),
            (self.alt, "Alt+"),
        ] {
            if held {
                label.push_str(name);
            }
        }
        label + &key
    }
}

pub struct Shortcuts {
    actions: HashMap<Shortcut, Action>,
    /// Each action's first shortcut, to show it by.
    first: HashMap<Action, Shortcut>,
}

impl Shortcuts {
    /// Caret's shortcuts, with those of the actions in `bound` replaced.
    pub fn new(bound: &HashMap<Action, Vec<Shortcut>>) -> Self {
        let mut actions = HashMap::new();
        let mut first = HashMap::new();
        for (action, _, keys) in ACTIONS {
            let keys: Vec<Shortcut> = match bound.get(action) {
                Some(keys) => keys.clone(),
                None => keys.iter().filter_map(|key| Shortcut::parse(key)).collect(),
            };
            if let Some(&key) = keys.first() {
                first.insert(*action, key);
            }
            actions.extend(keys.into_iter().map(|shortcut| (shortcut, *action)));
        }
        Self { actions, first }
    }

    /// The action a key pressed with `held` does, if any.
    pub fn action(&self, key: Keysym, held: Modifiers) -> Option<Action> {
        self.actions.get(&Shortcut::pressed(key, held)).copied()
    }

    /// `action`'s first shortcut as it is shown, such as `"Ctrl+Shift+C"`.
    pub fn label(&self, action: Action) -> Option<String> {
        self.first.get(&action).copied().map(Shortcut::label)
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
        assert_eq!(
            shortcuts.action(Keysym::space, CTRL_SHIFT),
            Some(Action::ControlMode)
        );
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(shortcuts.action(Keysym::space, ctrl), None);
    }

    #[test]
    fn an_action_is_shown_by_its_first_shortcut() {
        let shortcuts = Shortcuts::new(&HashMap::new());
        assert_eq!(
            shortcuts.label(Action::Copy).as_deref(),
            Some("Ctrl+Shift+C")
        );
        assert_eq!(
            shortcuts.label(Action::NextTab).as_deref(),
            Some("Ctrl+Page Down")
        );
        assert_eq!(shortcuts.label(Action::Fullscreen).as_deref(), Some("F11"));
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
