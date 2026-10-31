//! The list of shortcuts: what each action is and the keys that do it, as bound.

use gettextrs::gettext;
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use super::App;
use crate::chrome::ShortcutList;
use crate::shortcuts::Action;

impl App {
    pub(super) fn open_shortcut_list(&mut self) {
        let line = |label: String, action| self.shortcuts.label(action).map(|keys| (label, keys));
        let numbered = |label: String, numbered: fn(u8) -> Action| {
            self.numbered_keys(numbered).map(|keys| (label, keys))
        };
        let section = |title: String, lines: Vec<Option<(String, String)>>| {
            (title, lines.into_iter().flatten().collect::<Vec<_>>())
        };
        let sections = vec![
            section(
                gettext("Windows and Tabs"),
                vec![
                    line(gettext("New Window"), Action::NewWindow),
                    line(gettext("Close Window"), Action::CloseWindow),
                    line(gettext("New Tab"), Action::NewTab),
                    line(gettext("New Session"), Action::NewSession),
                    line(gettext("Close Tab"), Action::CloseTab),
                    line(gettext("Next Tab"), Action::NextTab),
                    line(gettext("Previous Tab"), Action::PreviousTab),
                    numbered(gettext("Switch to a Tab"), Action::ShowTab),
                    line(gettext("Full Screen"), Action::Fullscreen),
                    line(gettext("Show or Hide the Title Bar"), Action::TitleBar),
                ],
            ),
            section(
                gettext("Panes"),
                vec![
                    line(gettext("Split Right"), Action::SplitRight),
                    line(gettext("Split Down"), Action::SplitDown),
                    line(gettext("Close Pane"), Action::ClosePane),
                    line(gettext("Focus the Pane to the Left"), Action::FocusLeft),
                    line(gettext("Focus the Pane to the Right"), Action::FocusRight),
                    line(gettext("Focus the Pane Above"), Action::FocusUp),
                    line(gettext("Focus the Pane Below"), Action::FocusDown),
                    line(gettext("Move and Resize Panes"), Action::ControlMode),
                ],
            ),
            section(
                gettext("Sessions"),
                vec![
                    line(gettext("Sessions…"), Action::Sessions),
                    line(gettext("Tabs of the Session"), Action::SessionTabs),
                    line(gettext("Next Session"), Action::NextSession),
                    line(gettext("Previous Session"), Action::PreviousSession),
                    numbered(gettext("Switch to a Session"), Action::ShowSession),
                ],
            ),
            section(
                gettext("Terminal"),
                vec![
                    line(gettext("Copy"), Action::Copy),
                    line(gettext("Paste"), Action::Paste),
                    line(gettext("Select All"), Action::SelectAll),
                    line(gettext("Deselect"), Action::Deselect),
                    line(gettext("Find"), Action::Find),
                    line(gettext("Zoom In"), Action::ZoomIn),
                    line(gettext("Zoom Out"), Action::ZoomOut),
                    line(gettext("Original Size"), Action::ZoomReset),
                    line(gettext("Scroll Up a Page"), Action::PageUp),
                    line(gettext("Scroll Down a Page"), Action::PageDown),
                    line(
                        gettext("Scroll to the Previous Prompt"),
                        Action::PreviousPrompt,
                    ),
                    line(gettext("Scroll to the Next Prompt"), Action::NextPrompt),
                ],
            ),
            section(
                gettext("General"),
                vec![
                    line(gettext("Preferences"), Action::Preferences),
                    line(gettext("Keyboard Shortcuts"), Action::ShortcutList),
                ],
            ),
        ];
        self.close_menu();
        self.chrome.shortcut_list = Some(ShortcutList {
            heading: gettext("Keyboard Shortcuts"),
            sections: sections
                .into_iter()
                .filter(|(_, lines)| !lines.is_empty())
                .collect(),
            first: 0,
            line: self.fonts.label_height() as f32 / self.factor() as f32,
        });
        self.hover();
        self.dirty = true;
        self.draw();
    }

    /// The keys of the actions numbered one to nine, as one range where they differ
    /// only in the number, such as `"Alt+1…9"`.
    fn numbered_keys(&self, numbered: fn(u8) -> Action) -> Option<String> {
        let first = self.shortcuts.label(numbered(1))?;
        let last = self.shortcuts.label(numbered(9));
        match (
            first.strip_suffix('1'),
            last.as_deref().and_then(|last| last.strip_suffix('9')),
        ) {
            (Some(start), Some(end)) if start == end => Some(format!("{first}…9")),
            _ => Some(first),
        }
    }

    /// Scrolls the list a line a notch of the wheel, turned `value120` in 120ths or
    /// `discrete` notches.
    pub(super) fn scroll_shortcut_list(&mut self, value120: i32, discrete: i32) {
        let turned = if value120 != 0 {
            value120
        } else {
            discrete * 120
        };
        self.pointer.turned_list += turned;
        let notches = self.pointer.turned_list / 120;
        self.pointer.turned_list %= 120;
        if let (Some(size), Some(list)) = (self.size, &mut self.chrome.shortcut_list)
            && notches != 0
        {
            list.scroll(notches as isize, size);
            self.dirty = true;
            self.draw();
        }
    }

    pub(super) fn close_shortcut_list(&mut self) {
        if self.chrome.shortcut_list.take().is_some() {
            self.hover();
            self.dirty = true;
            self.draw();
        }
    }

    /// Takes a key while the list is open, which takes them all: the arrows and the
    /// page keys scroll it, and Escape, Enter or its own shortcut close it.
    pub(super) fn shortcut_list_key(&mut self, event: &KeyEvent) -> bool {
        let Some(size) = self.size else {
            return false;
        };
        let closes =
            self.shortcuts.action(event.keysym, self.modifiers) == Some(Action::ShortcutList);
        let Some(list) = &mut self.chrome.shortcut_list else {
            return false;
        };
        let page = list.page(size).cast_signed();
        let lines = match event.keysym {
            _ if closes => None,
            Keysym::Escape | Keysym::Return | Keysym::KP_Enter => None,
            Keysym::Down | Keysym::KP_Down => Some(1),
            Keysym::Up | Keysym::KP_Up => Some(-1),
            Keysym::Page_Down | Keysym::KP_Page_Down | Keysym::space => Some(page),
            Keysym::Page_Up | Keysym::KP_Page_Up => Some(-page),
            Keysym::End | Keysym::KP_End => Some(isize::MAX),
            Keysym::Home | Keysym::KP_Home => Some(isize::MIN),
            _ => Some(0),
        };
        match lines {
            None => self.close_shortcut_list(),
            Some(0) => {}
            Some(lines) => {
                list.scroll(lines, size);
                self.dirty = true;
                self.draw();
            }
        }
        true
    }
}
