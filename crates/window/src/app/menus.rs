//! The window's menus: the main one from the header bar, and the terminal's from a
//! right click, with what it can do where it was clicked.

use caret_protocol::{Question, SessionInfo, TerminalId};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use super::{App, Asked, pointer};
use crate::chrome::{Item, Menu};
use crate::config;
use crate::shortcuts::Action;

/// What a menu's item does.
#[derive(Debug, Clone)]
pub enum Choice {
    Act(Action),
    RenameTab(TerminalId),
    CloseTab(TerminalId),
    /// A new window with the settings of a profile.
    Profile(String),
    Detach,
    Reattach,
    Preferences,
    About,
    OpenLink(String),
    CopyLink(String),
}

impl App {
    fn item(&self, label: &str, choice: Choice, enabled: bool) -> (Item, Choice) {
        let shortcut = match choice {
            Choice::Act(action) => self.shortcuts.label(action),
            _ => None,
        };
        let item = Item {
            label: label.to_owned(),
            shortcut,
            enabled,
        };
        (item, choice)
    }

    fn act_item(&self, label: &str, action: Action) -> (Item, Choice) {
        self.item(label, Choice::Act(action), true)
    }

    fn show_menu(&mut self, sections: Vec<Vec<(Item, Choice)>>, at: (f32, f32), main: bool) {
        let (sections, choices): (Vec<Vec<Item>>, Vec<Vec<Choice>>) = sections
            .into_iter()
            .map(|section| section.into_iter().unzip())
            .unzip();
        self.choices = choices.into_iter().flatten().collect();
        self.chrome.menu = Some(Menu::new(sections, at, main));
        self.hover();
        self.dirty = true;
        self.draw();
    }

    pub(super) fn open_main_menu(&mut self) {
        let Some((width, _)) = self.size else {
            return;
        };
        let mut sections = vec![vec![
            self.act_item("New Window", Action::NewWindow),
            self.act_item("New Tab", Action::NewTab),
        ]];
        if !self.config.profiles.is_empty() {
            let profiles = self.config.profiles.iter().map(|profile| {
                let label = format!("New “{profile}” Window");
                self.item(&label, Choice::Profile(profile.clone()), true)
            });
            sections.push(profiles.collect());
        }
        sections.extend([
            vec![
                self.item("Detach Window", Choice::Detach, true),
                self.item("Reattach Windows", Choice::Reattach, true),
            ],
            vec![self.act_item("Full Screen", Action::Fullscreen)],
            vec![
                self.item("Preferences", Choice::Preferences, true),
                self.item("About Caret", Choice::About, true),
            ],
        ]);
        self.show_menu(sections, Menu::under_main_button(width), true);
    }

    /// Asks for the link under the pointer, to open the terminal's menu with it.
    pub(super) fn open_terminal_menu(&mut self) {
        let at = self.pointer_at();
        match (self.terminal(), self.pointer_cell()) {
            (Some(terminal), Some(point)) => {
                let id = self.server.ask(Question::LinkAt { terminal, point });
                self.asked.insert(id, Asked::Menu(at));
            }
            _ => self.terminal_menu(at, None),
        }
    }

    /// The terminal's menu at `at`, offering `link` if one was clicked.
    pub(super) fn terminal_menu(&mut self, at: (f32, f32), link: Option<String>) {
        let selected = self.grid.has_selection();
        let mut sections = vec![vec![
            self.item("Copy", Choice::Act(Action::Copy), selected),
            self.act_item("Paste", Action::Paste),
            self.act_item("Select All", Action::SelectAll),
            self.item("Deselect", Choice::Act(Action::Deselect), selected),
        ]];
        if let Some(link) = link {
            sections.push(vec![
                self.item("Open Link", Choice::OpenLink(link.clone()), true),
                self.item("Copy Link Address", Choice::CopyLink(link), true),
            ]);
        }
        // What the header bar holds, for when it is hidden.
        let bar = self.chrome.header() > 0;
        let mut window = Vec::new();
        if !bar {
            window.push(self.act_item("New Window", Action::NewWindow));
        }
        window.push(self.act_item("New Tab", Action::NewTab));
        window.push(self.act_item("Find…", Action::Find));
        sections.push(window);
        if !bar {
            sections.push(vec![
                self.act_item("Title Bar", Action::TitleBar),
                self.act_item("Full Screen", Action::Fullscreen),
            ]);
            sections.push(vec![self.act_item("Close Window", Action::CloseWindow)]);
        }
        self.show_menu(sections, at, false);
    }

    /// The menu of the tab at `index`, where the pointer is.
    pub(super) fn tab_menu(&mut self, index: usize) {
        let Some(terminal) = self.tabs.get(index).map(|tab| tab.terminal) else {
            return;
        };
        let sections = vec![
            vec![self.item("Rename Tab…", Choice::RenameTab(terminal), true)],
            vec![self.item("Close Tab", Choice::CloseTab(terminal), true)],
        ];
        self.show_menu(sections, self.pointer_at(), false);
    }

    pub(super) fn close_menu(&mut self) {
        if self.chrome.menu.take().is_some() {
            self.choices.clear();
            self.hover();
            self.dirty = true;
            self.draw();
        }
    }

    /// Closes the menu and does what its item at `index` does.
    pub(super) fn choose(&mut self, index: usize) {
        let enabled = self
            .chrome
            .menu
            .as_ref()
            .and_then(|menu| menu.items().nth(index))
            .is_some_and(|item| item.enabled);
        let choice = self.choices.get(index).cloned();
        self.close_menu();
        match choice.filter(|_| enabled) {
            Some(Choice::Act(action)) => self.act(action),
            Some(Choice::OpenLink(link)) => pointer::open(&link, &self.toaster),
            Some(Choice::CopyLink(link)) => self.copy(link, false),
            Some(Choice::RenameTab(terminal)) => self.rename_tab(terminal),
            Some(Choice::CloseTab(terminal)) => {
                if let Some(index) = self.tab_of(terminal) {
                    self.close_tab(index);
                }
            }
            Some(Choice::Profile(profile)) => self.new_window(Some(&profile)),
            Some(Choice::Detach) => self.end(true),
            Some(Choice::Reattach) => {
                let id = self.server.ask(Question::Sessions);
                self.asked.insert(id, Asked::Reattach);
            }
            Some(Choice::Preferences) => match config::editable() {
                Some(path) => pointer::open(&path.to_string_lossy(), &self.toaster),
                None => self.toast("Could not make the settings file".to_owned()),
            },
            Some(Choice::About) => self.about(),
            None => {}
        }
    }

    /// Opens another window, with the settings of `profile` if one is named.
    pub(super) fn new_window(&self, profile: Option<&str>) {
        let arguments = profile.map(|profile| vec!["--profile".into(), profile.to_owned()]);
        self.open_window(arguments.unwrap_or_default());
    }

    /// Opens a window for each session no window shows, or says there is none.
    pub(super) fn reattach(&mut self, sessions: &[SessionInfo]) {
        let mut detached = sessions
            .iter()
            .filter(|session| session.clients == 0 && !session.tabs.is_empty())
            .peekable();
        if detached.peek().is_none() {
            self.toast("No windows were detached".to_owned());
        }
        for session in detached {
            self.open_window(vec!["--session".into(), session.session.0.to_string()]);
        }
    }

    /// Takes a key while a menu is open, which takes them all: Escape closes it, the
    /// arrows move between its items, and Enter chooses one.
    pub(super) fn menu_key(&mut self, event: &KeyEvent) -> bool {
        let Some(menu) = &mut self.chrome.menu else {
            return false;
        };
        match event.keysym {
            Keysym::Escape => self.close_menu(),
            Keysym::Down | Keysym::KP_Down | Keysym::Tab => {
                menu.step(1);
                self.dirty = true;
                self.draw();
            }
            Keysym::Up | Keysym::KP_Up | Keysym::ISO_Left_Tab => {
                menu.step(-1);
                self.dirty = true;
                self.draw();
            }
            Keysym::Return | Keysym::KP_Enter | Keysym::space => {
                if let Some(focused) = menu.focused {
                    self.choose(focused);
                }
            }
            _ => {}
        }
        true
    }
}
