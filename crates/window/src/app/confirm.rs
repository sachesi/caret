//! Dialogs: asking before what the user may not mean, closing a tab or the window while
//! programs run in it and pasting lines a program will run one by one, asking for a
//! tab's name, and saying what Caret is.

use caret_protocol::{Answer, Bytes, Question, Request, SessionId, TerminalId, encode};
use gettextrs::gettext;
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use super::{App, Asked, pointer, sessions};
use crate::chrome::{Appearance, Dialog};
use crate::i18n::{gettext_f, ngettext_f};

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
    CloseSessions(Vec<SessionId>),
    CloseSession(SessionId),
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
        // A menu would stand over the dialog and take its clicks.
        self.close_menu();
        self.chrome.dialog = Some(dialog);
        self.confirming = Some(confirm);
        self.hover();
        self.dirty = true;
        self.draw();
    }

    /// Does what the dialog's button at `index` says.
    pub(super) fn press_response(&mut self, index: usize) {
        let accepts = self
            .chrome
            .dialog
            .as_ref()
            .is_some_and(|dialog| dialog.accepts(index));
        if index > 0 && !accepts && matches!(self.confirming, Some(Confirm::CloseWindow)) {
            self.respond(false);
            self.end(true);
            return;
        }
        self.respond(accepts);
    }

    /// Closes the dialog, doing what it asked if `accepted`.
    pub(super) fn respond(&mut self, accepted: bool) {
        let (field, picked) = self
            .chrome
            .dialog
            .take()
            .map_or((None, None), |dialog| (dialog.field, dialog.picked));
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
            Some(Confirm::CloseSession(session)) if accepted => {
                self.server.send(&Request::CloseSession { session });
            }
            Some(Confirm::CloseSessions(sessions)) if accepted => {
                for session in sessions {
                    self.server.send(&Request::CloseSession { session });
                }
            }
            Some(Confirm::RenameSession(session)) if accepted => {
                let name = field
                    .map(|field| field.text.trim().to_owned())
                    .filter(|name| !name.is_empty());
                self.server.send(&Request::NameSession { session, name });
                if let Some(picked) = picked
                    && picked != self.session_color_index(session)
                {
                    let color = (picked != sessions::automatic_color(session))
                        .then(|| u8::try_from(picked).unwrap_or_default());
                    self.server.send(&Request::ColorSession { session, color });
                }
            }
            _ => {}
        }
    }

    /// Asks for a name for `terminal`'s tab, starting from the one it shows.
    pub(super) fn rename_tab(&mut self, terminal: TerminalId) {
        let dialog = Dialog::asking(
            &gettext("Rename Tab"),
            &gettext("Without a name, the tab shows the program’s title."),
            [&gettext("Cancel"), &gettext("Rename")],
            &self.tab_title(terminal),
        );
        self.confirm(dialog, Confirm::Rename(terminal));
    }

    pub(super) fn rename_session(&mut self, session: SessionId) {
        if self.server.version < 2 {
            self.toast(gettext("Naming sessions needs a newer terminal server"));
            return;
        }
        let name = self
            .session_names
            .get(&session)
            .cloned()
            .unwrap_or_default();
        let mut dialog = Dialog::asking(
            &gettext("Rename Session"),
            &gettext("Without a name, the session is known by its number."),
            [&gettext("Cancel"), &gettext("Rename")],
            &name,
        );
        if self.config.session_colors {
            dialog.swatches = sessions::COLORS.to_vec();
            dialog.picked = Some(self.session_color_index(session));
        }
        self.confirm(dialog, Confirm::RenameSession(session));
    }

    /// Picks the dialog's colour at `index`.
    pub(super) fn pick_swatch(&mut self, index: usize) {
        if let Some(dialog) = &mut self.chrome.dialog {
            dialog.picked = Some(index);
            self.dirty = true;
            self.draw();
        }
    }

    /// Closes `session`'s tabs and ends their programs, asking first.
    pub(super) fn close_session(&mut self, session: SessionId) {
        let body = gettext_f(
            "The programs in the tabs of “{session}” will be stopped.",
            &[("session", &self.session_label(session))],
        );
        let dialog = Dialog::new(
            &gettext("Close Session?"),
            &body,
            [&gettext("Cancel"), &gettext("Close")],
        );
        self.confirm(dialog, Confirm::CloseSession(session));
    }

    /// Asks before ending the detached `sessions`, with how many programs run in them if the
    /// server can tell.
    pub(super) fn close_detached(&mut self, sessions: Vec<SessionId>) {
        if self.server.version < 3 {
            self.confirm_close_detached(sessions, None);
            return;
        }
        let id = self.server.ask(Question::Programs {
            sessions: sessions.clone(),
        });
        self.asked.insert(id, Asked::CloseDetached(sessions));
    }

    pub(super) fn confirm_close_detached(
        &mut self,
        sessions: Vec<SessionId>,
        programs: Option<u32>,
    ) {
        let count = sessions.len() as u32;
        let ending = ngettext_f(
            "{count} detached session will end.",
            "{count} detached sessions will end.",
            count,
            &[("count", &count.to_string())],
        );
        let body = match programs.filter(|&programs| programs > 0) {
            Some(programs) => {
                let running = ngettext_f(
                    "{count} program in them will be stopped.",
                    "{count} programs in them will be stopped.",
                    programs,
                    &[("count", &programs.to_string())],
                );
                format!("{ending} {running}")
            }
            None => ending,
        };
        let dialog = Dialog::new(
            &gettext("Close All Detached?"),
            &body,
            [&gettext("Cancel"), &gettext("Close All")],
        );
        self.confirm(dialog, Confirm::CloseSessions(sessions));
    }

    /// Says what Caret is, which version, and under which licence, offering its website.
    pub(super) fn about(&mut self) {
        let body = gettext_f(
            "A terminal drawn by the GPU. Version {version}, by sachesi, free software under the GNU General Public License, version 3 or later.",
            &[("version", env!("CARGO_PKG_VERSION"))],
        );
        let dialog = Dialog {
            appearance: Appearance::Suggested,
            ..Dialog::new("Caret", &body, [&gettext("Close"), &gettext("Website")])
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
        let modifiers = self.modifiers;
        let Some(dialog) = &mut self.chrome.dialog else {
            return false;
        };
        let has_field = dialog.field.is_some();
        match (event.keysym, dialog.focused, &mut dialog.field) {
            (Keysym::Escape, ..) => self.respond(false),
            (Keysym::Return | Keysym::KP_Enter, None, _) => self.respond(true),
            (Keysym::Return | Keysym::KP_Enter | Keysym::space, Some(focused), _) => {
                self.press_response(focused);
            }
            (Keysym::Tab | Keysym::ISO_Left_Tab, ..)
            | (Keysym::Left | Keysym::Right, Some(_), _) => {
                let order: Vec<Option<usize>> = has_field
                    .then_some(None)
                    .into_iter()
                    .chain((0..dialog.responses.len()).map(Some))
                    .collect();
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
                let edit = field.key(key, typed.as_deref(), modifiers);
                self.field_clipboard(edit);
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
                let dialog = Dialog::new(
                    &gettext("Close Tab?"),
                    &stopped(&program, false),
                    [&gettext("Cancel"), &gettext("Close")],
                );
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
                let title = if alone {
                    gettext("Close Tab?")
                } else {
                    gettext("Close Pane?")
                };
                let dialog = Dialog::new(
                    &title,
                    &stopped(&program, !alone),
                    [&gettext("Cancel"), &gettext("Close")],
                );
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
        let several = self.sessions.len() > 1;
        let named = self
            .sessions
            .iter()
            .find_map(|session| self.session_names.get(session).cloned());
        let body = if busy > 0 {
            ngettext_f(
                "A program is still running in this window and will be stopped.",
                "{count} programs are still running in this window and will be stopped.",
                busy,
                &[("count", &busy.to_string())],
            )
        } else if several {
            let count = self.sessions.len() as u32;
            ngettext_f(
                "{count} session will end with this window.",
                "{count} sessions will end with this window.",
                count,
                &[("count", &count.to_string())],
            )
        } else if let Some(name) = named {
            gettext_f(
                "The session “{session}” will end with this window.",
                &[("session", &name)],
            )
        } else {
            self.end(false);
            return;
        };
        let close = if several {
            gettext("Close All")
        } else {
            gettext("Close")
        };
        let cancel = gettext("Cancel");
        let detach = gettext("Detach");
        let mut dialog = Dialog::new(&gettext("Close Window?"), &body, [&cancel, &close]);
        dialog.responses.insert(1, detach);
        self.confirm(dialog, Confirm::CloseWindow);
    }

    /// Pastes `text` into `terminal`, asking first if the program there would run each
    /// line as it arrives: one that does not mark pasted text.
    pub(super) fn paste_into(&mut self, terminal: TerminalId, text: &[u8], bracketed: bool) {
        if !bracketed && text.iter().any(|&byte| byte == b'\n' || byte == b'\r') {
            let dialog = Dialog::new(
                &gettext("Paste Several Lines?"),
                &gettext(
                    "The program here does not mark pasted text, so each line runs as if typed and entered.",
                ),
                [&gettext("Cancel"), &gettext("Paste")],
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

/// Says `program` runs in the tab, or the `pane`, and will be stopped.
fn stopped(program: &str, pane: bool) -> String {
    let named = [("program", program)];
    match (program.is_empty(), pane) {
        (true, false) => gettext("A program is still running in this tab and will be stopped."),
        (true, true) => gettext("A program is still running in this pane and will be stopped."),
        (false, false) => gettext_f(
            "“{program}” is still running in this tab and will be stopped.",
            &named,
        ),
        (false, true) => gettext_f(
            "“{program}” is still running in this pane and will be stopped.",
            &named,
        ),
    }
}
