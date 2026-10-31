//! Finding text in the history: the find bar's field, and the questions it asks the
//! server.

use caret_protocol::{Question, Select};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use super::{App, Asked};
use crate::chrome::{Edit, Search};

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
        if !self
            .chrome
            .search
            .as_ref()
            .is_some_and(|search| search.focused)
        {
            return false;
        }
        match event.keysym {
            Keysym::Escape => {
                self.close_search();
                return true;
            }
            Keysym::Return | Keysym::KP_Enter => {
                self.find(!self.modifiers.shift);
                return true;
            }
            _ => {}
        }
        let typed = self.typed(event);
        let Some(search) = &mut self.chrome.search else {
            return true;
        };
        let edit = search
            .field
            .key(event.keysym, typed.as_deref(), self.modifiers);
        if self.field_clipboard(edit) {
            // A new text is found from the bottom up again.
            self.select(Select::Clear);
            self.find(true);
        }
        self.dirty = true;
        self.draw();
        true
    }

    /// Does what a field's key leaves to the window, saying whether the text changed.
    pub(super) fn field_clipboard(&mut self, edit: Edit) -> bool {
        match edit {
            Edit::Changed => true,
            Edit::Cut(text) => {
                self.copy(text, false);
                true
            }
            Edit::Copy(text) => {
                self.copy(text, false);
                false
            }
            Edit::Paste => {
                self.paste(false);
                false
            }
            Edit::Moved | Edit::Passed => false,
        }
    }

    /// Types `typed` into the dialog's or the find bar's field if one has the keys,
    /// saying whether it took it.
    pub(super) fn type_in_field(&mut self, typed: &str) -> bool {
        if let Some(field) = self
            .chrome
            .dialog
            .as_mut()
            .filter(|dialog| dialog.focused.is_none())
            .and_then(|dialog| dialog.field.as_mut())
        {
            field.type_text(typed);
            self.dirty = true;
            self.draw();
            return true;
        }
        self.search_type(typed)
    }

    /// Types `typed` into the field if it has the keys, saying whether it took it.
    pub(super) fn search_type(&mut self, typed: &str) -> bool {
        let Some(search) = self.chrome.search.as_mut().filter(|search| search.focused) else {
            return false;
        };
        search.field.type_text(typed);
        self.select(Select::Clear);
        self.find(true);
        self.dirty = true;
        self.draw();
        true
    }

    /// The text `event` types into a field: none with Ctrl or Alt held, as such keys
    /// are shortcuts.
    pub(super) fn typed(&self, event: &KeyEvent) -> Option<String> {
        event
            .utf8
            .clone()
            .filter(|typed| !typed.is_empty() && !typed.chars().any(char::is_control))
            .filter(|_| !self.modifiers.ctrl && !self.modifiers.alt)
    }

    /// Finds the field's text upwards or downwards from the selection.
    pub(super) fn find(&mut self, upwards: bool) {
        let terminal = self.terminal();
        let (Some(search), Some(terminal)) = (&mut self.chrome.search, terminal) else {
            return;
        };
        self.searches += 1;
        if search.field.text.is_empty() {
            search.found = true;
            return;
        }
        let id = self.server.ask(Question::Find {
            terminal,
            text: search.field.text.clone(),
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
