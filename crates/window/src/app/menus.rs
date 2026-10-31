//! The window's menus: the main one from the header bar, and the terminal's from a
//! right click, with what it can do where it was clicked.

use caret_protocol::{Question, SessionEntry, SessionId, SessionInfo, TerminalId};
use gettextrs::gettext;
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use super::{App, Asked, pointer};
use crate::chrome::{Item, Menu, Part};
use crate::config::{self, SessionListClick};
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
    /// A row of the Sessions list, which goes to the session.
    ListedSession(SessionId),
    /// Shows the session in place of the shown one.
    ReplaceWith(SessionId),
    /// Ends these detached sessions, asking first.
    CloseDetached(Vec<SessionId>),
    /// A line naming the items after it.
    Title,
    ShowSession(SessionId),
    SessionWindow(SessionId),
    /// A tab of a session's list of tabs, by its key.
    ShowTab(TerminalId),
    NewTabIn(SessionId),
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
            self.act_item(&gettext("New Session"), Action::NewSession),
        ]];
        if !self.config.profiles.is_empty() {
            let profiles = self.config.profiles.iter().map(|profile| {
                let label = gettext_f("New “{profile}” Window", &[("profile", profile)]);
                self.item(&label, Choice::Profile(profile.clone()), true)
            });
            sections.push(profiles.collect());
        }
        let session = self.shown_session();
        let mut detach_items = Vec::new();
        if self.sessions.len() > 1
            && let Some(session) = session
        {
            detach_items.push(self.item(
                &gettext("Detach Session"),
                Choice::DetachSession(session),
                true,
            ));
        }
        detach_items.push(self.item(&gettext("Detach Window"), Choice::Detach, true));
        sections.extend([
            vec![
                self.act_item(&gettext("Sessions…"), Action::Sessions),
                self.item(
                    &gettext("Rename Session…"),
                    Choice::RenameSession(session.unwrap_or(SessionId(0))),
                    session.is_some(),
                ),
            ]
            .into_iter()
            .chain(detach_items)
            .collect(),
            vec![self.act_item(&gettext("Full Screen"), Action::Fullscreen)],
            vec![
                self.act_item(&gettext("Preferences"), Action::Preferences),
                self.act_item(&gettext("Keyboard Shortcuts"), Action::ShortcutList),
                self.item(&gettext("About Caret"), Choice::About, true),
            ],
        ]);
        match at {
            Some(at) => self.show_menu(sections, at, false),
            None => self.show_menu(
                sections,
                Menu::under_main_button(width, &self.chrome.buttons),
                true,
            ),
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
            Some(Choice::ListedSession(session)) => self.go_to_listed(session),
            Some(Choice::ReplaceWith(session)) => self.replace_session(session),
            Some(Choice::CloseDetached(sessions)) => self.close_detached(sessions),
            Some(Choice::ShowSession(session)) => self.show_session(session),
            Some(Choice::SessionWindow(session)) => {
                self.open_window(vec!["--session".into(), session.0.to_string()]);
            }
            Some(Choice::ShowTab(key)) => {
                if let Some(index) = self.tab_of(key) {
                    self.show(index);
                }
            }
            Some(Choice::NewTabIn(session)) => {
                self.show_session_tab(session);
                self.new_tab();
            }
            Some(Choice::Reattach) => {
                let id = self.server.ask(Question::Sessions);
                self.asked.insert(id, Asked::Reattach);
            }
            Some(Choice::About) => self.about(),
            Some(Choice::MainMenu) => self.open_main_menu(at),
            Some(Choice::Title) | None => {}
        }
    }

    /// Opens the settings file with the program the desktop has for it.
    pub(super) fn open_preferences(&mut self) {
        match config::editable() {
            Some(path) => pointer::open(&path.to_string_lossy(), &self.toaster),
            None => self.toast(gettext("Could not make the settings file")),
        }
    }

    /// Where the terminal's menu opens from the keys: under the cursor, or where other
    /// menus the keys open go while it is out of sight.
    pub(super) fn cursor_menu_at(&self) -> (f32, f32) {
        let Some(cursor) = self.grid.cursor else {
            return self.keyed_menu_at();
        };
        let (scale, cell, margin) = (self.factor(), self.fonts.cell(), self.chrome.margin());
        let (x, y) = self.origin();
        let logical = |device: i32| (f64::from(device) / scale) as f32 - margin as f32;
        (
            logical(x + i32::from(cursor.point.column) * cell.width),
            logical(y + (i32::from(cursor.point.line) + 1) * cell.height),
        )
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

    /// The Sessions list: where each session is, then New Session. A click goes to one, as
    /// the setting has it; its context menu has the rest.
    pub(super) fn sessions_menu(&mut self, sessions: &[SessionEntry]) {
        self.learn_sessions(sessions);
        if self.chrome.toast_button.take().is_some() {
            self.chrome.toast = None;
        }
        let shown = self.shown_session();
        let (mut here, mut elsewhere, mut detached) = (Vec::new(), Vec::new(), Vec::new());
        let mut detached_sessions = Vec::new();
        for entry in sessions {
            let mut row = self.session_row(entry);
            row.0.checked = Some(entry.session) == shown;
            row.0.current = row.0.checked;
            if self.sessions.contains(&entry.session) {
                here.push(row);
            } else if entry.clients > 0 {
                elsewhere.push(row);
            } else {
                detached.push(row);
                detached_sessions.push(entry.session);
            }
        }
        let mut sections = Vec::new();
        for (title, rows) in [
            (gettext("In This Window"), here),
            (gettext("In Other Windows"), elsewhere),
            (gettext("Detached"), detached),
        ] {
            if !rows.is_empty() {
                let mut section = vec![self.title_item(&title)];
                section.extend(rows);
                sections.push(section);
            }
        }
        if detached_sessions.len() > 1 {
            sections.push(vec![
                self.item(&gettext("Open All Detached"), Choice::Reattach, true),
                self.item(
                    &gettext("Close All Detached…"),
                    Choice::CloseDetached(detached_sessions),
                    true,
                ),
            ]);
        }
        sections.push(vec![
            self.act_item(&gettext("New Session"), Action::NewSession),
        ]);
        let at = self.keyed_menu_at();
        self.show_menu(sections, at, false);
        let focused = self
            .chrome
            .menu
            .as_ref()
            .and_then(|menu| menu.items().position(|item| item.checked));
        if let Some(menu) = &mut self.chrome.menu {
            menu.focused = focused;
            if focused.is_none() {
                menu.step(1);
            }
        }
        self.dirty = true;
        self.draw();
    }

    /// A line naming the rows after it.
    fn title_item(&self, title: &str) -> (Item, Choice) {
        let mut item = self.item(title, Choice::Title, false);
        item.0.title = true;
        item
    }

    /// `entry`'s row in the Sessions list: its name and title, then its tabs, and its panes
    /// where those differ.
    fn session_row(&self, entry: &SessionEntry) -> (Item, Choice) {
        let title = entry.tabs.first().map_or("", |tab| {
            tab.name
                .as_deref()
                .filter(|name| !name.is_empty())
                .unwrap_or(&tab.title)
        });
        let label = gettext_f(
            "{session} · {title}",
            &[
                ("session", &self.session_label(entry.session)),
                ("title", title),
            ],
        );
        let tabs = entry.tabs.len() as u32;
        let count = ngettext_f(
            "{count} tab",
            "{count} tabs",
            tabs,
            &[("count", &tabs.to_string())],
        );
        let count = if entry.panes == tabs {
            count
        } else {
            let panes = ngettext_f(
                "{count} pane",
                "{count} panes",
                entry.panes,
                &[("count", &entry.panes.to_string())],
            );
            gettext_f("{tabs}, {panes}", &[("tabs", &count), ("panes", &panes)])
        };
        let mut row = self.item(&label, Choice::ListedSession(entry.session), true);
        row.0.dot = self.session_color(entry.session);
        row.0.shortcut = Some(count);
        row
    }

    /// Goes to a session listed in the Sessions list, as the setting has it.
    fn go_to_listed(&mut self, session: SessionId) {
        match self.config.session_list_click {
            SessionListClick::Add => self.show_session(session),
            SessionListClick::Replace => self.replace_session(session),
        }
    }

    /// The session of the Sessions list's row at `index`, if it is one.
    pub(super) fn listed_session(&self, index: usize) -> Option<SessionId> {
        match self.choices.get(index) {
            Some(&Choice::ListedSession(session)) => Some(session),
            _ => None,
        }
    }

    /// The Sessions list's row a press is holding, for a long press to open its menu.
    pub(super) fn pressed_listed_row(&self) -> Option<usize> {
        match self.chrome.pressed {
            Some(Part::MenuItem(index)) => {
                Some(index).filter(|&index| self.listed_session(index).is_some())
            }
            _ => None,
        }
    }

    /// The context menu of the Sessions list's row at `index`, at `at`, if it has one.
    pub(super) fn listed_menu(&mut self, index: usize, at: (f32, f32)) {
        if let Some(session) = self.listed_session(index) {
            self.session_menu(session, at);
        }
    }

    /// Detaches the session of the Sessions list's row at `index`, if this window shows it.
    pub(super) fn detach_listed(&mut self, index: usize) {
        if let Some(session) = self
            .listed_session(index)
            .filter(|session| self.sessions.contains(session))
        {
            self.close_menu();
            self.detach_session(session);
        }
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

    /// Asks for a session of its own in this window, with a tab.
    pub(super) fn new_session(&mut self) {
        if self.server.version < 2 {
            self.toast(gettext("Listing sessions needs a newer terminal server"));
            return;
        }
        let id = self.server.ask(Question::NewSession);
        self.asked.insert(id, Asked::SessionHere);
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
            vec![self.act_item(&gettext("New Session"), Action::NewSession)],
            others,
        ];
        let at = Menu::under_new_tab_button(&self.chrome.buttons);
        self.show_menu(sections, at, false);
    }

    /// What can be done with `session`, at `at`.
    fn session_menu(&mut self, session: SessionId, at: (f32, f32)) {
        let here = self.sessions.contains(&session);
        let other_action = match self.config.session_list_click {
            SessionListClick::Add => self.item(
                &gettext("Replace This Window"),
                Choice::ReplaceWith(session),
                !here,
            ),
            SessionListClick::Replace => self.item(
                &gettext("Add to This Window"),
                Choice::ShowSession(session),
                !here,
            ),
        };
        let sections = vec![
            vec![
                self.item(
                    &gettext("Open in New Window"),
                    Choice::SessionWindow(session),
                    true,
                ),
                other_action,
            ],
            vec![self.item(&gettext("Rename…"), Choice::RenameSession(session), true)],
            vec![
                self.item(&gettext("Detach"), Choice::DetachSession(session), here),
                self.item(&gettext("Close…"), Choice::CloseSession(session), true),
            ],
        ];
        self.show_menu(sections, at, false);
    }

    /// Says how many sessions are detached, if any, with a button that lists them.
    pub(super) fn hint_detached(&mut self, sessions: &[SessionInfo]) {
        let count = sessions
            .iter()
            .filter(|session| session.clients == 0 && !session.tabs.is_empty())
            .count() as u32;
        if count == 0 || !self.config.detached_hint {
            return;
        }
        let text = ngettext_f(
            "{count} detached session",
            "{count} detached sessions",
            count,
            &[("count", &count.to_string())],
        );
        self.toast_sessions(text, gettext("Show"));
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
            Keysym::Right | Keysym::KP_Right | Keysym::Menu => {
                if let Some(focused) = menu.focused {
                    let at = self.keyed_menu_at();
                    self.listed_menu(focused, at);
                }
            }
            _ => {}
        }
        true
    }
}
