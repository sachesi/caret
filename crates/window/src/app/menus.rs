//! The window's menus: the main one from the header bar, and the terminal's from a
//! right click, with what it can do where it was clicked.

use caret_protocol::Question;
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use super::{App, Asked, pointer};
use crate::chrome::{Item, Menu};
use crate::shortcuts::Action;

/// What a menu's item does.
#[derive(Debug, Clone)]
pub enum Choice {
    Act(Action),
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
        let sections = vec![
            vec![
                self.act_item("New Window", Action::NewWindow),
                self.act_item("New Tab", Action::NewTab),
            ],
            vec![self.act_item("Full Screen", Action::Fullscreen)],
        ];
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
            Some(Choice::OpenLink(link)) => pointer::open(&link),
            Some(Choice::CopyLink(link)) => self.copy(link, false),
            None => {}
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
