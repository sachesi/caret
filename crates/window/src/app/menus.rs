//! The window's menus: the main one from the header bar, and the terminal's from a
//! right click, with what it can do where it was clicked.

use caret_protocol::{Question, SessionEntry, SessionId, SessionInfo, TerminalId};
use gettextrs::gettext;
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use super::{App, Asked, pointer};
use crate::chrome::{Item, Menu};
use crate::config;
use crate::i18n::{gettext_f, ngettext_f};
use crate::shortcuts::Action;

/// What a menu's item does.
#[derive(Debug, Clone, PartialEq)]
pub enum Choice {
    Act(Action),
    RenameTab(TerminalId),
    CloseTab(TerminalId),
    /// A new window with the settings of a profile.
    Profile(String),
    Detach,
    Reattach,
    RenameSession(SessionId),
    DetachSession(SessionId),
    CloseSession(SessionId),
    /// What can be done with a session, in a menu of its own.
    SessionActions(SessionId),
    ShowSession(SessionId),
    SessionWindow(SessionId),
    /// A tab of a session's list of tabs, by its key.
    ShowTab(TerminalId),
    /// A session of its own in this window.
    NewSession,
    NewTabIn(SessionId),
    Preferences,
    About,
    /// The main menu, for a window without its header bar.
    MainMenu,
    OpenLink(String),
    CopyLink(String),
}

impl App {
    pub(super) fn item(&self, label: &str, choice: Choice, enabled: bool) -> (Item, Choice) {
        let shortcut = match choice {
            Choice::Act(action) => self.shortcuts.label(action),
            _ => None,
        };
        let item = Item {
            label: label.to_owned(),
            shortcut,
            enabled,
            ..Item::default()
        };
        (item, choice)
    }

    fn act_item(&self, label: &str, action: Action) -> (Item, Choice) {
        self.item(label, Choice::Act(action), true)
    }

    pub(super) fn show_menu(
        &mut self,
        sections: Vec<Vec<(Item, Choice)>>,
        at: (f32, f32),
        main: bool,
    ) {
        self.chrome.menu = Some(self.menu(sections, at, main));
        self.popup = None;
        self.hover();
        self.dirty = true;
        self.draw();
    }

    /// A menu of `sections`, keeping what each item does.
    pub(super) fn menu(
        &mut self,
        sections: Vec<Vec<(Item, Choice)>>,
        at: (f32, f32),
        main: bool,
    ) -> Menu {
        let (sections, choices): (Vec<Vec<Item>>, Vec<Vec<Choice>>) = sections
            .into_iter()
            .map(|section| section.into_iter().unzip())
            .unzip();
        self.choices = choices.into_iter().flatten().collect();
        Menu::new(sections, at, main)
    }

    /// The main menu: under its button in the header bar, or at `at` without one.
    pub(super) fn open_main_menu(&mut self, at: Option<(f32, f32)>) {
        let Some((width, _)) = self.size else {
            return;
        };
        let mut sections = vec![vec![
            self.act_item(&gettext("New Window"), Action::NewWindow),
            self.act_item(&gettext("New Tab"), Action::NewTab),
        ]];
        if !self.config.profiles.is_empty() {
            let profiles = self.config.profiles.iter().map(|profile| {
                let label = gettext_f("New “{profile}” Window", &[("profile", profile)]);
                self.item(&label, Choice::Profile(profile.clone()), true)
            });
            sections.push(profiles.collect());
        }
        let session = self.shown_session();
        sections.extend([
            vec![
                self.act_item(&gettext("Sessions…"), Action::Sessions),
                self.item(
                    &gettext("Rename Session…"),
                    Choice::RenameSession(session.unwrap_or(SessionId(0))),
                    session.is_some(),
                ),
                self.item(&gettext("Detach Window"), Choice::Detach, true),
                self.item(&gettext("Reattach Windows"), Choice::Reattach, true),
            ],
            vec![self.act_item(&gettext("Full Screen"), Action::Fullscreen)],
            vec![
                self.item(&gettext("Preferences"), Choice::Preferences, true),
                self.item(&gettext("About Caret"), Choice::About, true),
            ],
        ]);
        match at {
            Some(at) => self.show_menu(sections, at, false),
            None => self.show_menu(sections, Menu::under_main_button(width), true),
        }
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
            self.item(&gettext("Copy"), Choice::Act(Action::Copy), selected),
            self.act_item(&gettext("Paste"), Action::Paste),
            self.act_item(&gettext("Select All"), Action::SelectAll),
            self.item(
                &gettext("Deselect"),
                Choice::Act(Action::Deselect),
                selected,
            ),
        ]];
        if let Some(link) = link {
            sections.push(vec![
                self.item(&gettext("Open Link"), Choice::OpenLink(link.clone()), true),
                self.item(&gettext("Copy Link Address"), Choice::CopyLink(link), true),
            ]);
        }
        // What the header bar holds, for when it is hidden.
        let bar = self.chrome.header() > 0;
        let mut window = Vec::new();
        if !bar {
            window.push(self.act_item(&gettext("New Window"), Action::NewWindow));
        }
        window.push(self.act_item(&gettext("New Tab"), Action::NewTab));
        window.push(self.act_item(&gettext("Find…"), Action::Find));
        sections.push(window);
        let mut panes = vec![
            self.act_item(&gettext("Split Right"), Action::SplitRight),
            self.act_item(&gettext("Split Down"), Action::SplitDown),
        ];
        if self
            .tabs
            .get(self.selected)
            .is_some_and(|tab| tab.panes.len() > 1)
        {
            panes.push(self.act_item(&gettext("Close Pane"), Action::ClosePane));
        }
        sections.push(panes);
        if !bar {
            sections.push(vec![
                self.act_item(&gettext("Title Bar"), Action::TitleBar),
                self.act_item(&gettext("Full Screen"), Action::Fullscreen),
            ]);
            sections.push(vec![
                self.act_item(&gettext("Close Window"), Action::CloseWindow),
                self.item(&gettext("Main Menu"), Choice::MainMenu, true),
            ]);
        }
        self.show_menu(sections, at, false);
    }

    /// The menu of the tab at `index`, where the pointer is.
    pub(super) fn tab_menu(&mut self, index: usize) {
        let Some(terminal) = self.tabs.get(index).map(|tab| tab.key) else {
            return;
        };
        let sections = vec![
            vec![self.item(&gettext("Rename Tab…"), Choice::RenameTab(terminal), true)],
            vec![self.item(&gettext("Close Tab"), Choice::CloseTab(terminal), true)],
        ];
        self.show_menu(sections, self.pointer_at(), false);
    }

    pub(super) fn close_menu(&mut self) {
        self.popup = None;
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
        let at = self.chrome.menu.as_ref().map(|menu| menu.at);
        self.close_menu();
        match choice.filter(|_| enabled) {
            Some(Choice::Act(action)) => {
                self.act(action);
            }
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
            Some(Choice::RenameSession(session)) => self.rename_session(session),
            Some(Choice::DetachSession(session)) => self.detach_session(session),
            Some(Choice::CloseSession(session)) => self.close_session(session),
            Some(Choice::SessionActions(session)) => self.session_menu(session),
            Some(Choice::ShowSession(session)) => self.show_session(session),
            Some(Choice::SessionWindow(session)) => {
                self.open_window(vec!["--session".into(), session.0.to_string()]);
            }
            Some(Choice::ShowTab(key)) => {
                if let Some(index) = self.tab_of(key) {
                    self.show(index);
                }
            }
            Some(Choice::NewSession) => {
                let id = self.server.ask(Question::NewSession);
                self.asked.insert(id, Asked::SessionHere);
            }
            Some(Choice::NewTabIn(session)) => {
                self.show_session_tab(session);
                self.new_tab();
            }
            Some(Choice::Reattach) => {
                let id = self.server.ask(Question::Sessions);
                self.asked.insert(id, Asked::Reattach);
            }
            Some(Choice::Preferences) => match config::editable() {
                Some(path) => pointer::open(&path.to_string_lossy(), &self.toaster),
                None => self.toast(gettext("Could not make the settings file")),
            },
            Some(Choice::About) => self.about(),
            Some(Choice::MainMenu) => self.open_main_menu(at),
            None => {}
        }
    }

    /// Opens another window, with the settings of `profile` if one is named.
    pub(super) fn new_window(&self, profile: Option<&str>) {
        let arguments = profile.map(|profile| vec!["--profile".into(), profile.to_owned()]);
        self.open_window(arguments.unwrap_or_default());
    }

    /// Asks for the sessions, to list them in a menu.
    pub(super) fn ask_sessions(&mut self) {
        if self.server.version < 2 {
            self.toast(gettext("Listing sessions needs a newer terminal server"));
            return;
        }
        let id = self.server.ask(Question::SessionList);
        self.asked.insert(id, Asked::SessionList);
    }

    /// Where a menu the keys opened goes: near the top, across the middle.
    pub(super) fn keyed_menu_at(&self) -> (f32, f32) {
        let width = self.size.map_or(0, |(width, _)| width);
        let x = (width as f32 / 2.0 - 160.0).max(8.0);
        (x, self.chrome.top() as f32 + 8.0)
    }

    /// A menu of the sessions, each opening a menu of what can be done with it.
    pub(super) fn sessions_menu(&mut self, sessions: &[SessionEntry]) {
        self.learn_sessions(sessions);
        let items = sessions
            .iter()
            .map(|entry| {
                let title = entry.tabs.first().map_or("", |tab| {
                    tab.name
                        .as_deref()
                        .filter(|name| !name.is_empty())
                        .unwrap_or(&tab.title)
                });
                let place = if self.sessions.contains(&entry.session) {
                    gettext("here")
                } else if entry.clients > 0 {
                    gettext("shown")
                } else {
                    gettext("detached")
                };
                let tabs = entry.tabs.len() as u32;
                let tabs = ngettext_f(
                    "{count} tab",
                    "{count} tabs",
                    tabs,
                    &[("count", &tabs.to_string())],
                );
                let panes = ngettext_f(
                    "{count} pane",
                    "{count} panes",
                    entry.panes,
                    &[("count", &entry.panes.to_string())],
                );
                let label = gettext_f(
                    "{session} · {title} — {tabs}, {panes}, {place}",
                    &[
                        ("session", &self.session_label(entry.session)),
                        ("title", title),
                        ("tabs", &tabs),
                        ("panes", &panes),
                        ("place", &place),
                    ],
                );
                let mut item = self.item(&label, Choice::SessionActions(entry.session), true);
                item.0.dot = self.session_color(entry.session);
                item
            })
            .collect();
        let at = self.keyed_menu_at();
        self.show_menu(vec![items], at, false);
    }

    /// Keeps the names and colours the server lists, for sessions not shown here too.
    fn learn_sessions(&mut self, sessions: &[SessionEntry]) {
        for entry in sessions {
            match &entry.name {
                Some(name) => self.session_names.insert(entry.session, name.clone()),
                None => self.session_names.remove(&entry.session),
            };
            match entry.color {
                Some(color) => self.session_colors.insert(entry.session, color),
                None => self.session_colors.remove(&entry.session),
            };
        }
    }

    /// Asks for the sessions, to offer from the + button with a new one.
    pub(super) fn open_new_menu(&mut self) {
        if self.server.version < 2 {
            return;
        }
        let id = self.server.ask(Question::SessionList);
        self.asked.insert(id, Asked::NewMenu);
    }

    /// The + button's menu: a new session in this window, and the sessions it does not
    /// show, to show them here.
    pub(super) fn new_menu(&mut self, sessions: &[SessionEntry]) {
        self.learn_sessions(sessions);
        let others: Vec<(Item, Choice)> = sessions
            .iter()
            .filter(|entry| !self.sessions.contains(&entry.session))
            .map(|entry| {
                let title = entry.tabs.first().map_or("", |tab| {
                    tab.name
                        .as_deref()
                        .filter(|name| !name.is_empty())
                        .unwrap_or(&tab.title)
                });
                let label = gettext_f(
                    "Show {session} · {title}",
                    &[
                        ("session", &self.session_label(entry.session)),
                        ("title", title),
                    ],
                );
                let mut item = self.item(&label, Choice::ShowSession(entry.session), true);
                item.0.dot = self.session_color(entry.session);
                item
            })
            .collect();
        let sections = vec![
            vec![self.item(&gettext("New Session"), Choice::NewSession, true)],
            others,
        ];
        let at = Menu::under_new_tab_button();
        self.show_menu(sections, at, false);
    }

    /// What can be done with `session`.
    fn session_menu(&mut self, session: SessionId) {
        let here = self.sessions.contains(&session);
        let sections = vec![
            vec![
                self.item(&gettext("Show Here"), Choice::ShowSession(session), !here),
                self.item(
                    &gettext("Open in New Window"),
                    Choice::SessionWindow(session),
                    true,
                ),
            ],
            vec![self.item(&gettext("Rename…"), Choice::RenameSession(session), true)],
            vec![
                self.item(&gettext("Detach"), Choice::DetachSession(session), here),
                self.item(&gettext("Close…"), Choice::CloseSession(session), true),
            ],
        ];
        let at = self.keyed_menu_at();
        self.show_menu(sections, at, false);
    }

    /// Opens a window for each session no window shows, or says there is none.
    pub(super) fn reattach(&mut self, sessions: &[SessionInfo]) {
        let mut detached = sessions
            .iter()
            .filter(|session| session.clients == 0 && !session.tabs.is_empty())
            .peekable();
        if detached.peek().is_none() {
            self.toast(gettext("No windows were detached"));
        }
        for session in detached {
            self.open_window(vec!["--session".into(), session.session.0.to_string()]);
        }
    }

    /// Takes a key while a menu is open, which takes them all: Escape closes it, the
    /// arrows move between its items, and Enter chooses one.
    pub(super) fn menu_key(&mut self, event: &KeyEvent) -> bool {
        if self.popup.is_some() && self.popup_key(event) {
            return true;
        }
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
