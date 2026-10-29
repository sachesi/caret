//! Finding text in the history: the find bar's field, and the questions it asks the
//! server.

use caret_protocol::{Question, Select};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use super::{App, Asked};
use crate::chrome::Search;

impl App {
    /// Opens the find bar, or gives its field the keys if it is open.
    pub(super) fn open_search(&mut self) {
        if let Some(search) = &mut self.chrome.search {
            search.focused = true;
        } else {
            self.chrome.search = Some(Search {
                focused: true,
                found: true,
                ..Search::default()
            });
            self.fit();
        }
        self.dirty = true;
        self.draw();
    }

    pub(super) fn close_search(&mut self) {
        if self.chrome.search.take().is_some() {
            self.select(Select::Clear);
            self.fit();
            self.dirty = true;
            self.draw();
        }
    }

    /// Gives the field `event`'s key if it has the keys, saying whether it took it.
    pub(super) fn search_key(&mut self, event: &KeyEvent) -> bool {
        let Some(search) = self.chrome.search.as_mut().filter(|search| search.focused) else {
            return false;
        };
        let (text, cursor) = (&mut search.text, &mut search.cursor);
        let before = text[..*cursor]
            .chars()
            .next_back()
            .map_or(0, char::len_utf8);
        let after = text[*cursor..].chars().next().map_or(0, char::len_utf8);
        let mut changed = false;
        match event.keysym {
            Keysym::Escape => {
                self.close_search();
                return true;
            }
            Keysym::Return | Keysym::KP_Enter => {
                self.find(!self.modifiers.shift);
                return true;
            }
            Keysym::BackSpace if before > 0 => {
                *cursor -= before;
                text.remove(*cursor);
                changed = true;
            }
            Keysym::Delete | Keysym::KP_Delete if after > 0 => {
                text.remove(*cursor);
                changed = true;
            }
            Keysym::Left | Keysym::KP_Left => *cursor -= before,
            Keysym::Right | Keysym::KP_Right => *cursor += after,
            Keysym::Home | Keysym::KP_Home => *cursor = 0,
            Keysym::End | Keysym::KP_End => *cursor = text.len(),
            _ => {
                let typed = event
                    .utf8
                    .as_deref()
                    .filter(|typed| !typed.is_empty() && !typed.chars().any(char::is_control));
                if let Some(typed) = typed.filter(|_| !self.modifiers.ctrl && !self.modifiers.alt) {
                    text.insert_str(*cursor, typed);
                    *cursor += typed.len();
                    changed = true;
                }
            }
        }
        if changed {
            // A new text is found from the bottom up again.
            self.select(Select::Clear);
            self.find(true);
        }
        self.dirty = true;
        self.draw();
        true
    }

    /// Types `typed` into the field if it has the keys, saying whether it took it.
    pub(super) fn search_type(&mut self, typed: &str) -> bool {
        let Some(search) = self.chrome.search.as_mut().filter(|search| search.focused) else {
            return false;
        };
        search.text.insert_str(search.cursor, typed);
        search.cursor += typed.len();
        self.select(Select::Clear);
        self.find(true);
        self.dirty = true;
        self.draw();
        true
    }

    /// Finds the field's text upwards or downwards from the selection.
    pub(super) fn find(&mut self, upwards: bool) {
        let terminal = self.terminal();
        let (Some(search), Some(terminal)) = (&mut self.chrome.search, terminal) else {
            return;
        };
        self.searches += 1;
        if search.text.is_empty() {
            search.found = true;
            return;
        }
        let id = self.server.ask(Question::Find {
            terminal,
            text: search.text.clone(),
            upwards,
        });
        self.asked.insert(id, Asked::Find(self.searches));
    }

    /// Shows whether the text was found, unless another search has started since.
    pub(super) fn found(&mut self, search: u32, found: bool) {
        if let Some(bar) = &mut self.chrome.search
            && search == self.searches
        {
            bar.found = found;
            self.dirty = true;
            self.draw();
        }
    }
}
