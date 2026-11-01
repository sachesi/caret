//! The window's sessions while it shows several: one tab for each in the strip, which
//! shows the session's last tab or lists its tabs, and the keys between sessions.

use caret_protocol::{Question, Request, SessionId, TerminalId};
use gettextrs::gettext;
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use super::menus::Choice;
use super::pointer::{BUTTON_LEFT, BUTTON_MIDDLE, BUTTON_RIGHT};
use super::{App, Asked};
use crate::chrome::{self, Item, Part, SessionLabel, TabLabel};
use crate::config::{Config, SessionClick};
use crate::shortcuts::Action;

/// How far, in logical pixels, a held tab goes before it is dragged rather than clicked.
const DRAG: f64 = 8.0;

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

/// The colour of `session`, `picked` for it or else the one by its number.
pub fn session_color(session: SessionId, picked: Option<u8>) -> u32 {
    picked.map_or(COLORS[automatic_color(session)], |index| {
        COLORS[usize::from(index) % COLORS.len()]
    })
}

/// The colour `session` takes when none was picked for it.
pub(super) fn automatic_color(session: SessionId) -> usize {
    session.0.saturating_sub(1) as usize % COLORS.len()
}

impl App {
    /// The window shows several sessions: the strip has a tab for each.
    pub(super) fn grouped(&self) -> bool {
        self.sessions.len() > 1
    }

    /// The colour of `session`, unless sessions go without.
    pub(super) fn session_color(&self, session: SessionId) -> Option<u32> {
        self.config
            .session_colors
            .then(|| COLORS[self.session_color_index(session)])
    }

    /// Which of the colours `session` has: the one picked for it, else one by its number.
    pub(super) fn session_color_index(&self, session: SessionId) -> usize {
        self.session_colors
            .get(&session)
            .map_or(automatic_color(session), |&index| {
                usize::from(index) % COLORS.len()
            })
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
                    busy: tabs.clone().any(super::tabs::Tab::busy),
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

    /// Detaches from `session`, leaving its last tab and the window's profile for the
    /// next window to take up.
    pub(super) fn leave_session(&mut self, session: SessionId) {
        self.remember_tab();
        self.server.send(&Request::RememberSession {
            session,
            tab: self.last_tabs.get(&session).copied(),
            profile: self.profile.clone(),
        });
        self.server.send(&Request::Detach { session });
    }

    /// Goes back to what the window before this one had of `session`: its tab, and its
    /// profile when no other was asked for and this is the window's own session.
    pub(super) fn take_up_session(
        &mut self,
        session: SessionId,
        tab: Option<TerminalId>,
        profile: Option<String>,
    ) {
        if let Some(tab) = tab {
            self.last_tabs.insert(session, tab);
            let showing = self.tabs.get(self.selected).map(|tab| tab.session);
            if let Some(index) = self.tab_of(tab).filter(|_| showing == Some(session)) {
                self.show(index);
            }
        }
        if self.profile.is_none() && profile.is_some() && self.sessions.first() == Some(&session) {
            self.profile = profile;
            self.reconfigure(Config::load(self.profile.as_deref()));
        }
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
            // Clicked once let go of without being dragged.
            (Part::Tab(_), BUTTON_LEFT) => {
                self.pointer.dragged_session = Some((session, self.pointer.position, false));
            }
            (_, BUTTON_LEFT) => self.session_popup(session),
            (_, BUTTON_RIGHT) => self.session_popup_at(session, self.pointer_at()),
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

    /// The session whose tab the pointer is over, in a strip of sessions' tabs.
    pub(super) fn over_session_tab(&self) -> Option<SessionId> {
        let (Part::Tab(index) | Part::TabMenu(index)) = self.chrome.hovered? else {
            return None;
        };
        self.sessions.get(index).copied().filter(|_| self.grouped())
    }

    /// Shows the tab `steps` after `session`'s last, round from its last tab to its first.
    pub(super) fn step_tab_of(&mut self, session: SessionId, steps: i32) {
        let indices: Vec<usize> = (0..self.tabs.len())
            .filter(|&index| self.tabs[index].session == session)
            .collect();
        let Some(last) = self.session_tab(session) else {
            return;
        };
        let Some(at) = indices.iter().position(|&index| index == last) else {
            return;
        };
        let count = indices.len() as i64;
        let next = (at as i64 + i64::from(steps)).rem_euclid(count);
        self.show(indices[next as usize]);
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
        self.session_popup_at(session, at);
    }

    /// Lists `session`'s tabs at `at`, with what can be done with it.
    fn session_popup_at(&mut self, session: SessionId, at: (f32, f32)) {
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
        if self.config.tab_previews {
            self.ask_session_frames(session);
        }
        let tabs: Vec<(Item, Choice)> = self
            .tabs
            .iter()
            .enumerate()
            .filter(|(_, tab)| tab.session == session)
            .filter(|(_, tab)| tab.label().to_lowercase().contains(&needle))
            .map(|(index, tab)| {
                let item = Item {
                    label: tab.label().to_owned(),
                    enabled: true,
                    dot: tab.attention.then_some(chrome::ACCENT),
                    current: Some(tab.key) == shown,
                    closes: true,
                    preview: self.config.tab_previews.then(|| self.tab_preview(index)),
                    ..Item::default()
                };
                (item, Choice::ShowTab(tab.key))
            })
            .collect();
        let mut new_tab = self.item(&gettext("New Tab"), Choice::NewTabIn(session), true);
        new_tab.0.shortcut = self.shortcuts.label(Action::NewTab);
        let sections = vec![
            tabs,
            vec![new_tab],
            vec![
                self.item(
                    &gettext("Rename Session…"),
                    Choice::RenameSession(session),
                    true,
                ),
                self.item(
                    &gettext("Detach Session"),
                    Choice::DetachSession(session),
                    true,
                ),
                self.item(
                    &gettext("Close Session…"),
                    Choice::CloseSession(session),
                    true,
                ),
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
        if !self.sessions.contains(&session) {
            self.close_menu();
            return;
        }
        let Some(old) = self.chrome.menu.take() else {
            return;
        };
        let (at, filter) = (old.at, old.filter.clone().unwrap_or_default());
        // The focus stays with what it was on, wherever that is listed now.
        let focused = old
            .focused
            .and_then(|focused| self.choices.get(focused).cloned());
        let mut menu = self.popup_menu(session, at, &filter);
        menu.focused =
            focused.and_then(|focused| self.choices.iter().position(|choice| *choice == focused));
        menu.keep_place(&old);
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

    /// Starts holding the listed tab at `index`, which may be dragged.
    pub(super) fn hold_listed(&mut self, index: usize) {
        if self.popup.is_some()
            && let Some(Choice::ShowTab(key)) = self.choices.get(index)
        {
            self.pointer.dragged_row = Some((*key, self.pointer.position, false));
        }
    }

    /// Follows the pointer with the session's tab or the listed tab held: the session
    /// moves along the strip, the tab among the listed ones.
    pub(super) fn drag_listed(&mut self) {
        let position = self.pointer.position;
        let far = |start: (f64, f64)| (position.0 - start.0).hypot(position.1 - start.1) > DRAG;
        if let Some((session, start, dragged)) = &mut self.pointer.dragged_session {
            *dragged |= far(*start);
            let session = *session;
            if *dragged {
                self.drag_session(session);
            }
        }
        self.set_cursor();
        if let Some((key, start, dragged)) = &mut self.pointer.dragged_row {
            *dragged |= far(*start);
            let key = *key;
            if *dragged {
                self.drag_row(key);
            }
        }
    }

    /// Moves `session` to where the pointer is along the strip.
    fn drag_session(&mut self, session: SessionId) {
        let (Some((width, _)), Some(from)) = (
            self.size,
            self.sessions.iter().position(|&shown| shown == session),
        ) else {
            return;
        };
        let (x, _) = self.pointer_at();
        let Some(to) = self
            .chrome
            .tab_at(f64::from(x), width)
            .filter(|&to| to != from && to < self.sessions.len())
        else {
            return;
        };
        self.sessions.remove(from);
        self.sessions.insert(to, session);
        self.keep_tabs_with_sessions();
    }

    /// Puts `session` at `place` among the window's sessions.
    pub(super) fn move_session(&mut self, session: SessionId, place: usize) {
        self.sessions.retain(|&shown| shown != session);
        self.sessions
            .insert(place.min(self.sessions.len()), session);
        self.keep_tabs_with_sessions();
    }

    /// Each session's tabs stay together, in the sessions' order.
    fn keep_tabs_with_sessions(&mut self) {
        let shown = self.tabs.get(self.selected).map(|tab| tab.key);
        let order = self.sessions.clone();
        self.tabs
            .sort_by_key(|tab| order.iter().position(|&s| s == tab.session));
        if let Some(index) = shown.and_then(|shown| self.tab_of(shown)) {
            self.selected = index;
        }
        self.retitle();
    }

    /// Moves the listed tab `key` to the place of the listed tab under the pointer.
    fn drag_row(&mut self, key: TerminalId) {
        let Some(menu) = self.chrome.menu.as_ref() else {
            return;
        };
        let margin = f64::from(self.chrome.margin());
        let (x, y) = self.pointer.position;
        let (Part::MenuItem(index) | Part::MenuClose(index)) =
            menu.part_at((x - margin, y - margin))
        else {
            return;
        };
        let Some(&Choice::ShowTab(other)) = self.choices.get(index) else {
            return;
        };
        let keys = self.session_keys_of(key);
        if other == key {
            return;
        }
        if let (Some(to), Some(session)) = (
            keys.iter().position(|&listed| listed == other),
            self.tab_of(key).map(|index| self.tabs[index].session),
        ) {
            self.move_tab_to(key, session, to);
        }
    }

    /// The keys of the tabs of `key`'s session, in order.
    fn session_keys_of(&self, key: TerminalId) -> Vec<TerminalId> {
        let session = self.tab_of(key).map(|index| self.tabs[index].session);
        self.tabs
            .iter()
            .filter(|tab| Some(tab.session) == session)
            .map(|tab| tab.key)
            .collect()
    }

    /// Lets go of the session's tab or the listed tab held: a session's tab not dragged
    /// is clicked, and one dragged out of the window, or a listed tab dragged out of it
    /// or onto another session's tab, goes there. False when neither was held, or a
    /// listed tab was not dragged, so that it is chosen.
    pub(super) fn drop_listed(&mut self) -> bool {
        if let Some((session, _, dragged)) = self.pointer.dragged_session.take() {
            if !dragged {
                if let Some(index) = self.sessions.iter().position(|&shown| shown == session) {
                    self.session_clicked(index);
                }
            } else if self.outside() {
                self.session_to_window(session);
            }
            return true;
        }
        let Some((key, _, true)) = self.pointer.dragged_row.take() else {
            return false;
        };
        let target = self
            .size
            .and_then(|(width, _)| self.chrome.strip_at(self.pointer_at(), width))
            .filter(|_| self.grouped())
            .and_then(|index| self.sessions.get(index).copied());
        let own = self.tab_of(key).map(|index| self.tabs[index].session);
        // The menu may pass the window's edges.
        let over_menu = matches!(
            self.part(),
            Some(Part::MenuItem(_) | Part::MenuClose(_) | Part::Menu)
        );
        if self.outside() && !over_menu {
            self.close_menu();
            self.tab_to_window(key);
        } else if let Some(session) = target.filter(|&target| Some(target) != own) {
            self.close_menu();
            let (_, count) = self.group(session);
            self.move_tab_to(key, session, count);
        }
        true
    }

    /// The pointer is outside the window, as a drag takes it.
    pub(super) fn outside(&self) -> bool {
        let (x, y) = self.pointer.position;
        self.size.is_none_or(|(width, height)| {
            x < 0.0 || y < 0.0 || x >= f64::from(width) || y >= f64::from(height)
        })
    }

    /// Gives `session` a window of its own, leaving this one.
    fn session_to_window(&mut self, session: SessionId) {
        if self.sessions.len() > 1 {
            self.open_window(vec!["--session".into(), session.0.to_string()]);
            self.detach_session(session);
        }
    }

    /// Moves `key`'s tab to a new session, in a window of its own.
    pub(super) fn tab_to_window(&mut self, key: TerminalId) {
        if self.server.version < 2 || self.tabs.len() < 2 {
            return;
        }
        let id = self.server.ask(Question::NewSession);
        self.asked.insert(id, Asked::Window(key));
    }

    /// Moves `key`'s tab into `session`, new, and opens a window on it.
    pub(super) fn tab_into_window(&mut self, key: TerminalId, session: SessionId) {
        self.move_tab_to(key, session, 0);
        self.open_window(vec!["--session".into(), session.0.to_string()]);
    }
}
