//! The window's tabs: one terminal of the session each, opened, shown, closed and named
//! as the server tells.

use std::os::unix::ffi::OsStrExt;

use caret_protocol::{Bytes, Command, Frame, Question, Request, TerminalId};

use super::{App, Asked};
use crate::gpu::FRAMES_IN_FLIGHT;
use crate::grid::Grid;

pub struct Tab {
    pub terminal: TerminalId,
    /// The program's title, and the name the user gave the tab.
    title: String,
    name: Option<String>,
    /// The tab's cells while another is shown; the shown tab's are the window's.
    grid: Grid,
    /// A frame has been asked of the server and not yet come.
    pub frame_asked: bool,
}

impl Tab {
    fn label(&self) -> &str {
        self.name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or(Some(self.title.as_str()).filter(|title| !title.is_empty()))
            .unwrap_or("Caret")
    }
}

impl App {
    /// The terminal of the tab shown.
    pub(super) fn terminal(&self) -> Option<TerminalId> {
        self.tabs.get(self.selected).map(|tab| tab.terminal)
    }

    pub(super) fn tab_of(&self, terminal: TerminalId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.terminal == terminal)
    }

    /// A tab the server opened in the window's session, at `index`.
    pub(super) fn tab_opened(&mut self, index: u32, tab: caret_protocol::Tab) {
        let index =
            usize::try_from(index).map_or(self.tabs.len(), |index| index.min(self.tabs.len()));
        if index <= self.selected && !self.tabs.is_empty() {
            self.selected += 1;
        }
        self.tabs.insert(
            index,
            Tab {
                terminal: tab.terminal,
                title: tab.title,
                name: tab.name,
                grid: Grid::new(FRAMES_IN_FLIGHT),
                frame_asked: false,
            },
        );
        if let Some(size) = self.grid_size {
            self.server.send(&Request::Resize {
                terminal: tab.terminal,
                size,
            });
        }
        if self.tabs.len() == 1 || self.opened == Some(tab.terminal) {
            self.opened = None;
            self.show(index);
        }
        self.retitle();
    }

    pub(super) fn tab_closed(&mut self, terminal: TerminalId) {
        let Some(index) = self.tab_of(terminal) else {
            return;
        };
        self.forget_confirming(terminal);
        let closed = self.tabs.remove(index);
        if self.tabs.is_empty() {
            return;
        }
        if index < self.selected {
            self.selected -= 1;
        } else if index == self.selected {
            // The closed tab's cells were the window's: the next tab's take their place.
            self.selected = index.min(self.tabs.len() - 1);
            self.grid = std::mem::replace(&mut self.tabs[self.selected].grid, closed.grid);
            self.reveal();
        }
        self.retitle();
    }

    pub(super) fn tab_named(&mut self, terminal: TerminalId, name: Option<String>) {
        if let Some(index) = self.tab_of(terminal) {
            self.tabs[index].name = name;
            self.retitle();
        }
    }

    pub(super) fn tab_titled(&mut self, terminal: TerminalId, title: String) {
        if let Some(index) = self.tab_of(terminal) {
            self.tabs[index].title = title;
            self.retitle();
        }
    }

    /// Takes in a frame of `terminal`'s, into the window's cells if its tab is shown.
    pub(super) fn tab_frame(&mut self, terminal: TerminalId, frame: Frame) {
        let Some(index) = self.tab_of(terminal) else {
            return;
        };
        self.tabs[index].frame_asked = false;
        if index != self.selected {
            // Its glyphs are looked up when it is shown.
            self.tabs[index].grid.apply(frame, |_| 0);
            return;
        }
        let (atlas, fonts) = (&mut self.atlas, &mut self.fonts);
        self.grid.apply(frame, |glyph| atlas.index(glyph, fonts));
        self.renew_full_atlas();
        self.show_link();
        self.dirty = true;
        self.draw();
    }

    /// Shows the tab at `index`.
    pub(super) fn show(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if index != self.selected {
            if let Some(terminal) = self.terminal() {
                self.server.send(&Request::Focus {
                    terminal,
                    focused: false,
                });
                self.unhover(terminal);
            }
            std::mem::swap(&mut self.grid, &mut self.tabs[self.selected].grid);
            self.selected = index;
            std::mem::swap(&mut self.grid, &mut self.tabs[index].grid);
        }
        self.reveal();
        self.retitle();
    }

    /// Draws the shown tab's cells, which the GPU's copies do not hold, and asks for the
    /// rows it missed while hidden.
    fn reveal(&mut self) {
        let (atlas, fonts) = (&mut self.atlas, &mut self.fonts);
        self.grid.refill(|glyph| atlas.index(glyph, fonts));
        self.renew_full_atlas();
        self.show_link();
        if let Some(terminal) = self.terminal() {
            self.server.send(&Request::Focus {
                terminal,
                focused: self.focused,
            });
        }
        self.ask_frame();
        self.dirty = true;
        self.draw();
    }

    /// Shows the tab `steps` after the shown one, round from the last to the first.
    pub(super) fn step_tab(&mut self, steps: isize) {
        if let Ok(count) = isize::try_from(self.tabs.len())
            && count > 0
            && let Ok(selected) = isize::try_from(self.selected)
        {
            self.show((selected + steps).rem_euclid(count).unsigned_abs());
        }
    }

    /// Opens a tab after the shown one, running a shell where the shown one's is.
    pub(super) fn new_tab(&mut self) {
        match self.terminal() {
            Some(terminal) => {
                let id = self.server.ask(Question::NextCommand { terminal });
                self.asked.insert(id, Asked::NextCommand);
            }
            None => self.open_tab(Command::default()),
        }
    }

    /// Asks the server to start `command` in a tab after the shown one: the custom
    /// command or the shell, in this program's environment unless it has its own.
    pub(super) fn open_tab(&mut self, mut command: Command) {
        let (Some(session), Some(size)) = (self.session, self.grid_size) else {
            return;
        };
        if command.argv.is_empty() {
            command.argv = self.config.argv();
        }
        if command.directory.is_none() && self.tabs.is_empty() {
            command.directory = std::env::current_dir()
                .ok()
                .map(|path| Bytes(path.as_os_str().as_bytes().to_vec()));
        }
        // The server's own environment is the user manager's, without the session's.
        if command.env.is_empty() {
            command.env = std::env::vars().collect();
        }
        let index = if self.tabs.is_empty() {
            0
        } else {
            self.selected + 1
        };
        let id = self.server.ask(Question::OpenTab {
            session,
            index: u32::try_from(index).unwrap_or(u32::MAX),
            command,
            size,
        });
        self.asked.insert(id, Asked::Tab(size));
    }

    /// Shows the shown tab's title on the window and every tab's in the strip.
    fn retitle(&mut self) {
        let labels: Vec<String> = self.tabs.iter().map(|tab| tab.label().to_owned()).collect();
        let title = labels
            .get(self.selected)
            .cloned()
            .unwrap_or_else(|| "Caret".to_owned());
        if title != self.chrome.title {
            self.window.set_title(title.clone());
            self.chrome.title = title;
        }
        let strip = self.chrome.tab_bar();
        self.chrome.tabs = labels;
        self.chrome.selected = self.selected;
        if self.chrome.tab_bar() != strip {
            self.fit();
        }
        self.dirty = true;
        self.draw();
    }
}
