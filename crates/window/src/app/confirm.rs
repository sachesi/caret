//! Dialogs: asking before what the user may not mean, closing a tab or the window while
//! programs run in it and pasting lines a program will run one by one, asking for a
//! tab's name, and saying what Caret is.

use caret_protocol::{Answer, Bytes, Question, Request, SessionId, TerminalId, encode};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use super::{App, Asked, pointer};
use crate::chrome::{Appearance, Dialog};

/// Where Caret is made, which About offers to open.
const WEBSITE: &str = "https://github.com/sachesi/caret";

/// What the dialog open asks to confirm, or asks for.
pub enum Confirm {
    CloseTab(TerminalId),
    ClosePane(TerminalId),
    CloseWindow,
    Paste { terminal: TerminalId, text: Vec<u8> },
    Rename(TerminalId),
    RenameSession(SessionId),
    About,
}

/// The panes asked whether a program runs in them before the window closes.
pub struct Closing {
    left: usize,
    busy: u32,
}

/// The panes of a tab asked whether a program runs in them before it closes.
pub struct TabClosing {
    key: TerminalId,
    left: usize,
    /// The first program found running.
    program: Option<String>,
}

impl App {
    fn confirm(&mut self, dialog: Dialog, confirm: Confirm) {
        self.chrome.dialog = Some(dialog);
        self.confirming = Some(confirm);
        self.hover();
        self.dirty = true;
        self.draw();
    }

    /// Closes the dialog, doing what it asked if `accepted`.
    pub(super) fn respond(&mut self, accepted: bool) {
        let field = self.chrome.dialog.take().and_then(|dialog| dialog.field);
        let confirm = self.confirming.take();
        self.hover();
        self.dirty = true;
        self.draw();
        match confirm {
            Some(Confirm::CloseTab(terminal)) if accepted => {
                self.server.send(&Request::CloseTab { terminal });
            }
            Some(Confirm::ClosePane(terminal)) if accepted => self.end_pane(terminal),
            Some(Confirm::CloseWindow) if accepted => self.end(false),
            Some(Confirm::Paste { terminal, text }) if accepted => {
                self.server.send(&Request::Input {
                    terminal,
                    bytes: Bytes(encode::paste(&text, false)),
                    bottom: true,
                });
            }
            Some(Confirm::About) if accepted => pointer::open(WEBSITE, &self.toaster),
            Some(Confirm::Rename(terminal)) if accepted => {
                let name = field
                    .map(|field| field.text.trim().to_owned())
                    .filter(|name| !name.is_empty());
                self.server.send(&Request::NameTab { terminal, name });
            }
            Some(Confirm::RenameSession(session)) if accepted => {
                let name = field
                    .map(|field| field.text.trim().to_owned())
                    .filter(|name| !name.is_empty());
                self.server.send(&Request::NameSession { session, name });
            }
            _ => {}
        }
    }

    /// Asks for a name for `terminal`'s tab, starting from the one it shows.
    pub(super) fn rename_tab(&mut self, terminal: TerminalId) {
        let dialog = Dialog::asking(
            "Rename Tab",
            "Without a name, the tab shows the program’s title.",
            ["Cancel", "Rename"],
            &self.tab_title(terminal),
        );
        self.confirm(dialog, Confirm::Rename(terminal));
    }

    pub(super) fn rename_session(&mut self, session: SessionId) {
        if self.server.version < 2 {
            self.toast("Naming sessions needs a newer terminal server".to_owned());
            return;
        }
        let name = self
            .session_names
            .get(&session)
            .cloned()
            .unwrap_or_default();
        let dialog = Dialog::asking(
            "Rename Session",
            "Without a name, the session is known by its number.",
            ["Cancel", "Rename"],
            &name,
        );
        self.confirm(dialog, Confirm::RenameSession(session));
    }

    /// Says what Caret is, which version, and under which licence, offering its website.
    pub(super) fn about(&mut self) {
        let body = format!(
            "A terminal drawn by the GPU. Version {}, by sachesi, free software under the \
             GNU General Public License, version 3 or later.",
            env!("CARGO_PKG_VERSION")
        );
        let dialog = Dialog {
            appearance: Appearance::Suggested,
            ..Dialog::new("Caret", &body, ["Close", "Website"])
        };
        self.confirm(dialog, Confirm::About);
    }

    /// Closes a dialog about `terminal`, which has gone.
    pub(super) fn forget_confirming(&mut self, terminal: TerminalId) {
        if let Some(
            Confirm::CloseTab(asked)
            | Confirm::ClosePane(asked)
            | Confirm::Rename(asked)
            | Confirm::Paste {
                terminal: asked, ..
            },
        ) = self.confirming
            && asked == terminal
        {
            self.respond(false);
        }
    }

    /// Takes a key while a dialog is open, which takes them all: Escape declines, Enter
    /// presses the focused button or accepts the field's text, Tab moves the focus
    /// round, and the rest edit the field while it has the keys.
    pub(super) fn dialog_key(&mut self, event: &KeyEvent) -> bool {
        let typed = self.typed(event);
        let Some(dialog) = &mut self.chrome.dialog else {
            return false;
        };
        let has_field = dialog.field.is_some();
        match (event.keysym, dialog.focused, &mut dialog.field) {
            (Keysym::Escape, ..) => self.respond(false),
            (Keysym::Return | Keysym::KP_Enter, focused, _) => {
                self.respond(focused.is_none_or(|focused| focused == 1));
            }
            (Keysym::space, Some(focused), _) => self.respond(focused == 1),
            (Keysym::Tab | Keysym::ISO_Left_Tab, ..)
            | (Keysym::Left | Keysym::Right, Some(_), _) => {
                let order: &[Option<usize>] = if has_field {
                    &[None, Some(0), Some(1)]
                } else {
                    &[Some(0), Some(1)]
                };
                let at = order
                    .iter()
                    .position(|&focused| focused == dialog.focused)
                    .unwrap_or(0);
                let backwards = matches!(event.keysym, Keysym::ISO_Left_Tab | Keysym::Left);
                let step = if backwards { order.len() - 1 } else { 1 };
                dialog.focused = order[(at + step) % order.len()];
                self.dirty = true;
                self.draw();
            }
            (key, None, Some(field)) => {
                field.key(key, typed.as_deref());
                self.dirty = true;
                self.draw();
            }
            _ => {}
        }
        true
    }

    /// Closes the tab at `index`, asking first if a program runs in one of its panes.
    pub(super) fn close_tab(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let (key, panes) = (tab.key, tab.root.panes());
        self.closing_tab = Some(TabClosing {
            key,
            left: panes.len(),
            program: None,
        });
        for terminal in panes {
            let id = self.server.ask(Question::Busy { terminal });
            self.asked.insert(id, Asked::TabBusy(key));
        }
    }

    /// Counts a pane's answer to whether a program runs in it, and once every pane of
    /// the tab has answered, closes it or asks.
    pub(super) fn tab_busy(&mut self, key: TerminalId, answer: Answer) {
        let Some(closing) = self
            .closing_tab
            .as_mut()
            .filter(|closing| closing.key == key)
        else {
            return;
        };
        closing.left -= 1;
        if let Answer::Busy {
            program: Some(program),
        } = answer
        {
            closing.program.get_or_insert(program);
        }
        if closing.left > 0 {
            return;
        }
        let program = closing.program.take();
        self.closing_tab = None;
        match program {
            Some(program) if self.tab_of(key).is_some() => {
                let dialog =
                    Dialog::new("Close Tab?", &stopped(&program, "tab"), ["Cancel", "Close"]);
                self.confirm(dialog, Confirm::CloseTab(key));
            }
            _ => self.server.send(&Request::CloseTab { terminal: key }),
        }
    }

    /// Closes `terminal`'s pane, asking first if a program runs in it.
    pub(super) fn close_pane(&mut self, terminal: TerminalId) {
        let id = self.server.ask(Question::Busy { terminal });
        self.asked.insert(id, Asked::Busy(terminal));
    }

    pub(super) fn busy(&mut self, terminal: TerminalId, answer: Answer) {
        match answer {
            Answer::Busy {
                program: Some(program),
            } if self.tab_of(terminal).is_some() => {
                let alone = self
                    .tab_of(terminal)
                    .is_some_and(|index| self.tabs[index].panes.len() == 1);
                let (title, place) = if alone {
                    ("Close Tab?", "tab")
                } else {
                    ("Close Pane?", "pane")
                };
                let dialog = Dialog::new(title, &stopped(&program, place), ["Cancel", "Close"]);
                self.confirm(dialog, Confirm::ClosePane(terminal));
            }
            _ => self.end_pane(terminal),
        }
    }

    /// Ends `terminal`'s program and its pane, and the tab with its last pane.
    fn end_pane(&mut self, terminal: TerminalId) {
        // A server from before panes knows only tabs, of one terminal each.
        if self.server.version < 2 {
            self.server.send(&Request::CloseTab { terminal });
        } else {
            self.server.send(&Request::ClosePane { terminal });
        }
    }

    /// Closes the window, asking first if programs run in its tabs and closing would
    /// stop them.
    pub(super) fn close(&mut self) {
        if self.closing.is_some() {
            return;
        }
        if self.config.close_detaches || self.tabs.is_empty() {
            self.end(self.config.close_detaches);
            return;
        }
        let panes: Vec<TerminalId> = self.tabs.iter().flat_map(|tab| tab.root.panes()).collect();
        self.closing = Some(Closing {
            left: panes.len(),
            busy: 0,
        });
        for terminal in panes {
            let id = self.server.ask(Question::Busy { terminal });
            self.asked.insert(id, Asked::Closing);
        }
    }

    /// Counts a pane's answer to whether a program runs in it, and once every pane has
    /// answered, closes the window or asks.
    pub(super) fn closing_busy(&mut self, answer: &Answer) {
        let Some(closing) = &mut self.closing else {
            return;
        };
        closing.left -= 1;
        if let Answer::Busy { program: Some(_) } = answer {
            closing.busy += 1;
        }
        if closing.left > 0 {
            return;
        }
        let busy = closing.busy;
        self.closing = None;
        if busy == 0 {
            self.end(false);
            return;
        }
        let body = if busy == 1 {
            "A program is still running in this window and will be stopped.".to_owned()
        } else {
            format!("{busy} programs are still running in this window and will be stopped.")
        };
        let dialog = Dialog::new("Close Window?", &body, ["Cancel", "Close"]);
        self.confirm(dialog, Confirm::CloseWindow);
    }

    /// Pastes `text` into `terminal`, asking first if the program there would run each
    /// line as it arrives: one that does not mark pasted text.
    pub(super) fn paste_into(&mut self, terminal: TerminalId, text: &[u8], bracketed: bool) {
        if !bracketed && text.iter().any(|&byte| byte == b'\n' || byte == b'\r') {
            let dialog = Dialog::new(
                "Paste Several Lines?",
                "The program here does not mark pasted text, so each line runs as if typed and entered.",
                ["Cancel", "Paste"],
            );
            let text = text.to_vec();
            self.confirm(dialog, Confirm::Paste { terminal, text });
            return;
        }
        self.server.send(&Request::Input {
            terminal,
            bytes: Bytes(encode::paste(text, bracketed)),
            bottom: true,
        });
    }
}

/// Says `program` runs in the `place` and will be stopped.
fn stopped(program: &str, place: &str) -> String {
    if program.is_empty() {
        format!("A program is still running in this {place} and will be stopped.")
    } else {
        format!("“{program}” is still running in this {place} and will be stopped.")
    }
}
