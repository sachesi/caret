//! The window's sessions while it shows several: one tab for each in the strip, which
//! shows the session's last tab or lists its tabs, and the keys between sessions.

use caret_protocol::{SessionId, TerminalId};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use super::App;
use super::menus::Choice;
use super::pointer::{BUTTON_LEFT, BUTTON_MIDDLE, BUTTON_RIGHT};
use crate::chrome::{self, Item, Part, SessionLabel, TabLabel};
use crate::config::SessionClick;
use crate::shortcuts::Action;

/// The colours sessions take by their number, unless the user gave one another.
pub const COLORS: [u32; 9] = [
    0x3584_e4ff,
    0x2190_a4ff,
    0x3a94_4aff,
    0xc888_00ff,
    0xed5b_00ff,
    0xe62d_42ff,
    0xd561_99ff,
    0x9141_acff,
    0x6f83_96ff,
];

impl App {
    /// The window shows several sessions: the strip has a tab for each.
    pub(super) fn grouped(&self) -> bool {
        self.sessions.len() > 1
    }

    /// The colour of `session`, unless sessions go without.
    pub(super) fn session_color(&self, session: SessionId) -> Option<u32> {
        if !self.config.session_colors {
            return None;
        }
        let index = self
            .session_colors
            .get(&session)
            .map_or(session.0.saturating_sub(1) as usize, |&index| {
                usize::from(index)
            });
        Some(COLORS[index % COLORS.len()])
    }

    /// The strip's tab for each session, in the window's order.
    pub(super) fn session_labels(&self) -> Vec<TabLabel> {
        self.sessions
            .iter()
            .map(|&session| {
                let tabs = self.tabs.iter().filter(|tab| tab.session == session);
                let shown = self
                    .session_tab(session)
                    .map_or("", |index| self.tabs[index].label());
                TabLabel {
                    title: format!("{} · {shown}", self.session_label(session)),
                    attention: tabs.clone().any(|tab| tab.attention),
                    session: Some(SessionLabel {
                        tabs: tabs.count(),
                        color: self.session_color(session),
                    }),
                }
            })
            .collect()
    }

    /// The tab of `session` it shows: the one shown last, or its first.
    pub(super) fn session_tab(&self, session: SessionId) -> Option<usize> {
        self.last_tabs
            .get(&session)
            .and_then(|&key| self.tab_of(key))
            .filter(|&index| self.tabs[index].session == session)
            .or_else(|| self.tabs.iter().position(|tab| tab.session == session))
    }

    /// Remembers the tab shown as its session's last.
    pub(super) fn remember_tab(&mut self) {
        if let Some(tab) = self.tabs.get(self.selected) {
            self.last_tabs.insert(tab.session, tab.key);
        }
    }

    /// Shows `session`'s last tab.
    pub(super) fn show_session_tab(&mut self, session: SessionId) {
        if let Some(index) = self.session_tab(session) {
            self.show(index);
        }
    }

    /// Takes a press of `button` on `part` of the strip of sessions' tabs: false for
    /// parts it does not have.
    pub(super) fn pressed_session_tab(&mut self, part: Part, button: u32) -> bool {
        let (Part::Tab(index) | Part::TabMenu(index)) = part else {
            return false;
        };
        let Some(&session) = self.sessions.get(index) else {
            return true;
        };
        match (part, button) {
            (Part::Tab(_), BUTTON_LEFT) => self.session_clicked(index),
            (_, BUTTON_LEFT | BUTTON_RIGHT) => self.session_popup(session),
            (_, BUTTON_MIDDLE) if self.config.middle_click_detach => {
                self.detach_session(session);
            }
            _ => {}
        }
        true
    }

    /// A click on the strip's tab of the session at `index`: shows its last tab, or
    /// lists its tabs as the settings say.
    pub(super) fn session_clicked(&mut self, index: usize) {
        let Some(&session) = self.sessions.get(index) else {
            return;
        };
        if self.config.session_click == SessionClick::Popup || self.shown_session() == Some(session)
        {
            self.session_popup(session);
        } else {
            self.show_session_tab(session);
        }
    }

    /// Shows the last tab of the session `steps` after the shown one, round from the
    /// last to the first.
    pub(super) fn step_session(&mut self, steps: isize) {
        let Some(shown) = self.shown_session() else {
            return;
        };
        let (Some(at), Ok(count)) = (
            self.sessions.iter().position(|&session| session == shown),
            isize::try_from(self.sessions.len()),
        ) else {
            return;
        };
        let Ok(at) = isize::try_from(at) else {
            return;
        };
        let next = self.sessions[(at + steps).rem_euclid(count).unsigned_abs()];
        self.show_session_tab(next);
    }

    /// Shows the last tab of the window's session numbered `number` from 1.
    pub(super) fn nth_session(&mut self, number: usize) {
        if let Some(&session) = number.checked_sub(1).and_then(|at| self.sessions.get(at)) {
            self.show_session_tab(session);
        }
    }

    /// The keys of the shown session's tabs, in order.
    pub(super) fn session_keys(&self) -> Vec<TerminalId> {
        let session = self.shown_session();
        self.tabs
            .iter()
            .filter(|tab| Some(tab.session) == session)
            .map(|tab| tab.key)
            .collect()
    }

    /// Lists `session`'s tabs under its tab in the strip, with what can be done with it.
    pub(super) fn session_popup(&mut self, session: SessionId) {
        let at = self
            .sessions
            .iter()
            .position(|&shown| shown == session)
            .filter(|_| self.grouped())
            .zip(self.size)
            .and_then(|(index, (width, _))| self.chrome.under_tab(index, width))
            .unwrap_or_else(|| self.keyed_menu_at());
        let mut menu = self.popup_menu(session, at, "");
        let current = menu.items().position(|item| item.current);
        menu.focused = current;
        self.chrome.menu = Some(menu);
        self.popup = Some(session);
        self.hover();
        self.dirty = true;
        self.draw();
    }

    /// The menu listing `session`'s tabs whose titles have `filter` in them, at `at`.
    fn popup_menu(&mut self, session: SessionId, at: (f32, f32), filter: &str) -> chrome::Menu {
        let shown = self.tabs.get(self.selected).map(|tab| tab.key);
        let needle = filter.to_lowercase();
        let tabs: Vec<(Item, Choice)> = self
            .tabs
            .iter()
            .filter(|tab| tab.session == session)
            .filter(|tab| tab.label().to_lowercase().contains(&needle))
            .map(|tab| {
                let item = Item {
                    label: tab.label().to_owned(),
                    enabled: true,
                    dot: tab.attention.then_some(chrome::ACCENT),
                    current: Some(tab.key) == shown,
                    closes: true,
                    preview: None,
                    shortcut: None,
                };
                (item, Choice::ShowTab(tab.key))
            })
            .collect();
        let mut new_tab = self.item("New Tab", Choice::NewTabIn(session), true);
        new_tab.0.shortcut = self.shortcuts.label(Action::NewTab);
        let sections = vec![
            tabs,
            vec![new_tab],
            vec![
                self.item("Rename Session…", Choice::RenameSession(session), true),
                self.item("Detach Session", Choice::DetachSession(session), true),
                self.item("Close Session…", Choice::CloseSession(session), true),
            ],
        ];
        let mut menu = self.menu(sections, at, false);
        menu.heading = Some((self.session_label(session), self.session_color(session)));
        menu.filter = Some(filter.to_owned());
        menu
    }

    /// Lists the tabs again as they are now, if a session's list is open.
    pub(super) fn refresh_popup(&mut self) {
        let Some(session) = self.popup else {
            return;
        };
        let Some(old) = self.chrome.menu.as_ref() else {
            return;
        };
        if !self.sessions.contains(&session) {
            self.close_menu();
            return;
        }
        let (at, filter, focused) = (old.at, old.filter.clone().unwrap_or_default(), old.focused);
        let mut menu = self.popup_menu(session, at, &filter);
        let count = menu.items().count();
        menu.focused = focused.filter(|&focused| focused < count);
        self.chrome.menu = Some(menu);
    }

    /// Takes a key while a session's tabs are listed: typing narrows them down, Delete
    /// closes the tab focused. False for keys any menu takes.
    pub(super) fn popup_key(&mut self, event: &KeyEvent) -> bool {
        let Some(menu) = self.chrome.menu.as_mut() else {
            return false;
        };
        let filter = menu.filter.get_or_insert_with(String::new);
        match event.keysym {
            Keysym::Delete | Keysym::KP_Delete => {
                if let Some(focused) = menu.focused {
                    self.close_listed(focused);
                }
                return true;
            }
            Keysym::BackSpace => {
                filter.pop();
            }
            _ => match event
                .utf8
                .as_deref()
                .filter(|text| text.chars().all(|c| !c.is_control()) && !text.trim().is_empty())
            {
                Some(text) => filter.push_str(text),
                None => return false,
            },
        }
        self.refresh_popup();
        if let Some(menu) = self.chrome.menu.as_mut() {
            menu.focused = None;
            menu.step(1);
        }
        self.dirty = true;
        self.draw();
        true
    }

    /// Closes the tab of the listed item at `index`, keeping the list open.
    pub(super) fn close_listed(&mut self, index: usize) {
        if let Some(Choice::ShowTab(key)) = self.choices.get(index).cloned()
            && let Some(index) = self.tab_of(key)
        {
            self.close_tab(index);
        }
    }
}
