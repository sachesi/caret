//! Dialogs: asking before what the user may not mean, closing a tab or the window while
//! programs run in it and pasting lines a program will run one by one, asking for a
//! tab's name, and saying what Caret is.

use caret_protocol::{Answer, Bytes, Question, Request, TerminalId, encode};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use super::{App, Asked, pointer};
use crate::chrome::{Appearance, Dialog};

/// Where Caret is made, which About offers to open.
const WEBSITE: &str = "https://github.com/sachesi/caret";

/// What the dialog open asks to confirm, or asks for.
pub enum Confirm {
    CloseTab(TerminalId),
    CloseWindow,
    Paste { terminal: TerminalId, text: Vec<u8> },
    Rename(TerminalId),
    About,
}

/// The tabs asked whether a program runs in them before the window closes.
pub struct Closing {
    left: usize,
    busy: u32,
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

    /// Closes the tab at `index`, asking first if a program runs in it.
    pub(super) fn close_tab(&mut self, index: usize) {
        if let Some(tab) = self.tabs.get(index) {
            let terminal = tab.terminal;
            let id = self.server.ask(Question::Busy { terminal });
            self.asked.insert(id, Asked::Busy(terminal));
        }
    }

    pub(super) fn busy(&mut self, terminal: TerminalId, answer: Answer) {
        match answer {
            Answer::Busy {
                program: Some(program),
            } if self.tab_of(terminal).is_some() => {
                let body = if program.is_empty() {
                    "A program is still running in this tab and will be stopped.".to_owned()
                } else {
                    format!("“{program}” is still running in this tab and will be stopped.")
                };
                let dialog = Dialog::new("Close Tab?", &body, ["Cancel", "Close"]);
                self.confirm(dialog, Confirm::CloseTab(terminal));
            }
            _ => self.server.send(&Request::CloseTab { terminal }),
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
        self.closing = Some(Closing {
            left: self.tabs.len(),
            busy: 0,
        });
        for terminal in self.tabs.iter().map(|tab| tab.terminal).collect::<Vec<_>>() {
            let id = self.server.ask(Question::Busy { terminal });
            self.asked.insert(id, Asked::Closing);
        }
    }

    /// Counts a tab's answer to whether a program runs in it, and once every tab has
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
            "A program is still running in 1 tab and will be stopped.".to_owned()
        } else {
            format!("Programs are still running in {busy} tabs and will be stopped.")
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
