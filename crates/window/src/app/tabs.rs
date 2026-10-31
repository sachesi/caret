//! The window's tabs: each a tree of split panes of the session's terminals, opened,
//! shown, closed and named as the server tells.

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;

use caret_protocol::layout::Node;
use caret_protocol::{Bytes, Command, Frame, Question, Request, SessionId, Size, TerminalId};

use super::{App, Asked};
use crate::chrome::TabLabel;
use crate::grid::{Grid, Unresolved};
use crate::panes::{self, Direction, Placed, Rect};

/// The first protocol version with split panes.
const PANES: u16 = 2;

pub struct Pane {
    /// The program's title.
    title: String,
    /// The pane's cells, unless it is the focused one of the tab shown: those are the
    /// window's.
    pub grid: Grid,
    /// A frame has been asked of the server and not yet come.
    frame_asked: bool,
    /// Its size as the server was last told.
    size: Option<Size>,
}

impl Pane {
    fn new(title: String) -> Self {
        Self {
            title,
            grid: Grid::default(),
            frame_asked: false,
            size: None,
        }
    }
}

pub struct Tab {
    /// What the server knows the tab by: the terminal it was opened with.
    pub key: TerminalId,
    pub session: SessionId,
    pub root: Node,
    /// The pane that has the keys.
    pub focused: TerminalId,
    pub panes: HashMap<TerminalId, Pane>,
    /// The name the user gave the tab.
    name: Option<String>,
    /// Something happened in it while it was out of sight.
    attention: bool,
    /// The focused pane takes the whole tab, the others hidden.
    pub zoomed: bool,
}

impl Tab {
    fn label(&self) -> &str {
        let title = self
            .panes
            .get(&self.focused)
            .map_or("", |pane| pane.title.as_str());
        self.name
            .as_deref()
            .filter(|name| !name.is_empty())
            .or(Some(title).filter(|title| !title.is_empty()))
            .unwrap_or("Caret")
    }

    fn has(&self, terminal: TerminalId) -> bool {
        self.key == terminal || self.root.contains(terminal)
    }
}

impl App {
    /// The terminal of the pane with the keys, in the tab shown.
    pub(super) fn terminal(&self) -> Option<TerminalId> {
        self.tabs.get(self.selected).map(|tab| tab.focused)
    }

    /// The tab `terminal` is a pane of, or the key of.
    pub(super) fn tab_of(&self, terminal: TerminalId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.has(terminal))
    }

    /// Swaps the window's cells with those of the focused pane of the tab shown, which
    /// are the window's while it is focused and shown.
    fn swap_focused(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.selected)
            && let Some(pane) = tab.panes.get_mut(&tab.focused)
        {
            std::mem::swap(&mut self.grid, &mut pane.grid);
        }
    }

    /// The room the panes of a tab share, in device pixels from the surface's corner.
    pub(super) fn area(&self) -> Option<Rect> {
        let (width, height) = self.size?;
        let margin = self.chrome.margin();
        let padding = self.device(super::PADDING) as i32;
        let height = height.saturating_sub(self.chrome.top());
        Some(Rect {
            x: self.device(margin + super::PADDING) as i32,
            y: self.device(margin + self.chrome.top() + super::PADDING) as i32,
            width: (self.device(width) as i32 - 2 * padding).max(1),
            height: (self.device(height) as i32 - 2 * padding).max(1),
        })
    }

    /// The pixels between the two sides of a split.
    pub(super) fn gap(&self) -> i32 {
        self.device(2 * super::PADDING) as i32
    }

    fn lay_out(&self, root: &Node) -> Placed {
        self.area()
            .map(|area| panes::place(root, area, self.gap()))
            .unwrap_or_default()
    }

    /// Where the panes of `tab` are shown.
    fn lay_out_tab(&self, tab: &Tab) -> Placed {
        if tab.zoomed {
            self.lay_out(&Node::Pane {
                terminal: tab.focused,
            })
        } else {
            self.lay_out(&tab.root)
        }
    }

    /// Where the panes of the tab shown are.
    pub(super) fn placed(&self) -> Placed {
        self.tabs
            .get(self.selected)
            .map(|tab| self.lay_out_tab(tab))
            .unwrap_or_default()
    }

    /// The size of a grid filling `rect`.
    fn size_of(&self, rect: Rect) -> Size {
        let cell = self.fonts.cell();
        let fit = |pixels: i32, cell: i32| {
            u16::try_from((pixels / cell.max(1)).max(1)).unwrap_or(u16::MAX)
        };
        Size {
            columns: fit(rect.width, cell.width),
            lines: fit(rect.height, cell.height),
            cell_width: u16::try_from(cell.width).unwrap_or(u16::MAX),
            cell_height: u16::try_from(cell.height).unwrap_or(u16::MAX),
        }
    }

    /// Tells the server the size of each pane of the tab at `index` that changed.
    pub(super) fn resize_panes(&mut self, index: usize) {
        let Some(tab) = self.tabs.get(index) else {
            return;
        };
        let placed = self.lay_out_tab(tab);
        let sizes: Vec<(TerminalId, Size)> = placed
            .panes
            .iter()
            .map(|&(terminal, rect)| (terminal, self.size_of(rect)))
            .collect();
        let Some(tab) = self.tabs.get_mut(index) else {
            return;
        };
        for (terminal, size) in sizes {
            if let Some(pane) = tab.panes.get_mut(&terminal)
                && pane.size != Some(size)
            {
                pane.size = Some(size);
                self.server.send(&Request::Resize { terminal, size });
            }
        }
    }

    /// Where the tabs of `session` start among the window's, and how many it has.
    fn group(&self, session: SessionId) -> (usize, usize) {
        let order = |session: SessionId| {
            self.sessions
                .iter()
                .position(|&shown| shown == session)
                .unwrap_or(usize::MAX)
        };
        let own = order(session);
        let start = self
            .tabs
            .iter()
            .take_while(|tab| order(tab.session) < own)
            .count();
        let count = self
            .tabs
            .iter()
            .filter(|tab| tab.session == session)
            .count();
        (start, count)
    }

    /// The session of the tab shown, or the one the window started with.
    pub(super) fn shown_session(&self) -> Option<SessionId> {
        self.tabs
            .get(self.selected)
            .map(|tab| tab.session)
            .or_else(|| self.sessions.first().copied())
    }

    /// The session as the user knows it: by the name they gave it, or its number.
    pub(super) fn session_label(&self, session: SessionId) -> String {
        self.session_names
            .get(&session)
            .cloned()
            .unwrap_or_else(|| format!("Session {}", session.0))
    }

    /// Shows `session`'s tabs in the window too, after the others.
    pub(super) fn show_session(&mut self, session: SessionId) {
        if self.server.version < PANES {
            self.toast("Showing sessions together needs a newer terminal server".to_owned());
            return;
        }
        if !self.sessions.contains(&session) {
            self.sessions.push(session);
            self.server.send(&Request::Attach { session });
        }
    }

    /// Stops showing `session`, which keeps running in the server; the window goes with
    /// its last session.
    pub(super) fn detach_session(&mut self, session: SessionId) {
        if self.sessions == [session] {
            self.end(true);
            return;
        }
        self.server.send(&Request::Detach { session });
        self.session_gone(session);
    }

    /// Lets go of `session`'s tabs, which the window no longer shows.
    pub(super) fn session_gone(&mut self, session: SessionId) {
        let keys: Vec<TerminalId> = self
            .tabs
            .iter()
            .filter(|tab| tab.session == session)
            .map(|tab| tab.key)
            .collect();
        for key in keys {
            self.tab_closed(key);
        }
        self.sessions.retain(|&shown| shown != session);
        self.session_names.remove(&session);
        if self.sessions.is_empty() {
            self.exit = true;
        }
        self.retitle();
    }

    /// A tab the server opened in `session`, at `index` of its tabs.
    pub(super) fn tab_opened(&mut self, session: SessionId, index: u32, tab: caret_protocol::Tab) {
        let (start, count) = self.group(session);
        let index = start + usize::try_from(index).map_or(count, |index| index.min(count));
        if index <= self.selected && !self.tabs.is_empty() {
            self.selected += 1;
        }
        let key = tab.terminal;
        self.tabs.insert(
            index,
            Tab {
                key,
                session,
                root: Node::Pane { terminal: key },
                focused: key,
                panes: HashMap::from([(key, Pane::new(tab.title))]),
                name: tab.name,
                attention: false,
                zoomed: false,
            },
        );
        self.resize_panes(index);
        if self.tabs.len() == 1 || self.opened == Some(key) {
            self.opened = None;
            self.show(index);
        }
        self.retitle();
    }

    pub(super) fn tab_closed(&mut self, terminal: TerminalId) {
        let Some(index) = self.tab_of(terminal) else {
            return;
        };
        let closed = self.tabs.remove(index);
        for pane in closed.panes.keys() {
            self.forget_confirming(*pane);
        }
        self.forget_confirming(closed.key);
        if self.tabs.is_empty() {
            return;
        }
        if index < self.selected {
            self.selected -= 1;
        } else if index == self.selected {
            // The closed tab's focused cells were the window's: the next tab's take their
            // place.
            self.selected = index.min(self.tabs.len() - 1);
            self.swap_focused();
            self.reveal();
        }
        self.retitle();
    }

    /// The panes the server says the tab `key` has now.
    pub(super) fn tab_layout(&mut self, key: TerminalId, root: Node) {
        let Some(index) = self.tabs.iter().position(|tab| tab.key == key) else {
            return;
        };
        let shown = index == self.selected;
        let panes = root.panes();
        let tab = &self.tabs[index];
        let lost_focus = !root.contains(tab.focused);
        let opened = self.opened_pane.filter(|&pane| root.contains(pane));
        let focus = if let Some(opened) = opened {
            opened
        } else if lost_focus {
            // The pane that was before it in order, which took its room or was next to it.
            let order = tab.root.panes();
            let at = order
                .iter()
                .position(|&pane| pane == tab.focused)
                .unwrap_or(0);
            order[..at]
                .iter()
                .rev()
                .chain(&order[at..])
                .find(|pane| root.contains(**pane))
                .copied()
                .unwrap_or_else(|| root.first())
        } else {
            tab.focused
        };
        if opened.is_some() {
            self.opened_pane = None;
        }
        let changes_focus = focus != tab.focused;
        if shown && changes_focus {
            self.leave_focused();
        }
        let tab = &mut self.tabs[index];
        tab.root = root;
        tab.panes.retain(|terminal, _| panes.contains(terminal));
        for terminal in &panes {
            tab.panes
                .entry(*terminal)
                .or_insert_with(|| Pane::new(String::new()));
        }
        tab.focused = focus;
        tab.zoomed = false;
        if shown && changes_focus {
            self.swap_focused();
        }
        self.resize_panes(index);
        // A pane taken out to a tab of its own is shown there.
        let moved = self.moved_pane.map(|terminal| Node::Pane { terminal });
        if moved.as_ref() == Some(&self.tabs[index].root) {
            self.moved_pane = None;
            if !shown {
                self.show(index);
                return;
            }
        }
        if shown {
            self.reveal();
        }
        self.retitle();
    }

    /// Moves `terminal`'s tab to `index` of its session's tabs, keeping the one shown.
    pub(super) fn tab_moved(&mut self, terminal: TerminalId, index: u32) {
        let Some(from) = self.tab_of(terminal) else {
            return;
        };
        let shown = self.tabs.get(self.selected).map(|tab| tab.key);
        let tab = self.tabs.remove(from);
        let (start, count) = self.group(tab.session);
        let index = start + usize::try_from(index).map_or(count, |index| index.min(count));
        self.tabs.insert(index, tab);
        if let Some(index) = shown.and_then(|shown| self.tab_of(shown)) {
            self.selected = index;
        }
        self.retitle();
    }

    /// Moves `terminal`'s tab to where `x` is in the strip, as it is dragged there: among
    /// its session's tabs, or into another session's.
    pub(super) fn drag_tab(&mut self, terminal: TerminalId, x: f64) {
        let (Some(from), Some((width, _))) = (self.tab_of(terminal), self.size) else {
            return;
        };
        let Some(target) = self
            .chrome
            .tab_at(x, width)
            .filter(|&target| target != from && target < self.tabs.len())
        else {
            return;
        };
        let session = self.tabs[target].session;
        let (start, _) = self.group(session);
        let index = target - start;
        self.server.send(&Request::MoveTab {
            terminal,
            session,
            index: u32::try_from(index).unwrap_or(u32::MAX),
        });
        if self.tabs[from].session != session {
            // The server tells only the other clients of a tab moved between sessions.
            let shown = self.tabs.get(self.selected).map(|tab| tab.key);
            let mut tab = self.tabs.remove(from);
            tab.session = session;
            let (start, count) = self.group(session);
            self.tabs.insert(start + index.min(count), tab);
            if let Some(index) = shown.and_then(|shown| self.tab_of(shown)) {
                self.selected = index;
            }
            self.retitle();
        }
    }

    pub(super) fn tab_named(&mut self, terminal: TerminalId, name: Option<String>) {
        if let Some(index) = self.tab_of(terminal) {
            self.tabs[index].name = name;
            self.retitle();
        }
    }

    pub(super) fn tab_titled(&mut self, terminal: TerminalId, title: String) {
        if let Some(index) = self.tab_of(terminal)
            && let Some(pane) = self.tabs[index].panes.get_mut(&terminal)
        {
            pane.title = title;
            self.retitle();
        }
    }

    /// Takes in a frame of `terminal`'s: into the window's cells if its pane is the one
    /// focused, into its own otherwise.
    pub(super) fn tab_frame(&mut self, terminal: TerminalId, frame: Frame) {
        let Some(index) = self.tab_of(terminal) else {
            return;
        };
        let focused = self.tabs[index].focused == terminal;
        let Some(pane) = self.tabs[index].panes.get_mut(&terminal) else {
            return;
        };
        pane.frame_asked = false;
        if index != self.selected {
            // Its glyphs are looked up when it is shown.
            pane.grid.apply(frame, Unresolved);
            return;
        }
        if focused {
            // The server sends a frame only once something changed, if only the cursor,
            // as after a typed space.
            crate::timings::output();
            self.grid.apply(frame, self.atlas.lookup(&mut self.fonts));
        } else {
            pane.grid.apply(frame, self.atlas.lookup(&mut self.fonts));
        }
        self.renew_full_atlas();
        self.show_link();
        self.dirty = true;
        self.draw();
    }

    /// Looks every glyph of the panes shown up again, in the atlas as it is now.
    pub(super) fn refill_shown(&mut self) {
        self.grid.refill(self.atlas.lookup(&mut self.fonts));
        if let Some(tab) = self.tabs.get_mut(self.selected) {
            for (terminal, pane) in &mut tab.panes {
                if *terminal != tab.focused {
                    pane.grid.refill(self.atlas.lookup(&mut self.fonts));
                }
            }
        }
    }

    /// Asks for the next frame of each pane shown.
    pub(super) fn ask_frame(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.selected) else {
            return;
        };
        for (&terminal, pane) in &mut tab.panes {
            if !pane.frame_asked {
                self.server.send(&Request::Frame { terminal });
                pane.frame_asked = true;
            }
        }
    }

    /// Tells the focused pane it lost the focus and the pointer, before another takes
    /// them, and takes its cells out of the window's.
    fn leave_focused(&mut self) {
        if let Some(terminal) = self.terminal() {
            self.server.send(&Request::Focus {
                terminal,
                focused: false,
            });
            self.unhover(terminal);
        }
        self.swap_focused();
    }

    /// Shows the tab at `index`.
    pub(super) fn show(&mut self, index: usize) {
        if index >= self.tabs.len() {
            return;
        }
        if index != self.selected {
            self.leave_focused();
            self.selected = index;
            self.swap_focused();
        }
        self.tabs[index].attention = false;
        self.reveal();
        self.retitle();
    }

    /// Gives the keys to `terminal`'s pane of the tab shown.
    pub(super) fn focus_pane(&mut self, terminal: TerminalId) {
        let Some(tab) = self.tabs.get(self.selected) else {
            return;
        };
        if tab.focused == terminal || !tab.panes.contains_key(&terminal) {
            return;
        }
        self.leave_focused();
        self.tabs[self.selected].focused = terminal;
        self.unzoom();
        self.swap_focused();
        self.reveal();
        self.retitle();
    }

    /// Gives the keys to the pane next to the focused one towards `direction`; false
    /// when there is none.
    pub(super) fn focus_towards(&mut self, direction: Direction) -> bool {
        let Some(from) = self.terminal() else {
            return false;
        };
        match panes::neighbour(&self.placed(), from, direction) {
            Some(terminal) => {
                self.focus_pane(terminal);
                true
            }
            None => false,
        }
    }

    /// Draws the shown tab's cells, which the GPU's copies do not hold, and asks for the
    /// rows it missed while hidden.
    fn reveal(&mut self) {
        self.refill_shown();
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
                self.asked.insert(id, Asked::NextCommand(None));
            }
            None => self.open_tab(Command::default()),
        }
    }

    /// Splits the focused pane, the new one beside it or under it, running a shell
    /// where the focused one's is.
    pub(super) fn split(&mut self, vertical: bool) {
        if self.server.version < PANES {
            self.toast("Splitting needs a newer terminal server".to_owned());
            return;
        }
        if let Some(terminal) = self.terminal() {
            let id = self.server.ask(Question::NextCommand { terminal });
            self.asked.insert(id, Asked::NextCommand(Some(vertical)));
        }
    }

    /// A program, as its window's command line gives it unless it has its own.
    fn filled(&self, mut command: Command) -> Command {
        if command.argv.is_empty() {
            command.argv = self.config.argv();
        }
        // The server's own environment is the user manager's, without the session's.
        if command.env.is_empty() {
            command.env = std::env::vars().collect();
        }
        command
    }

    /// Asks the server to start `command` in a pane after the focused one, which the
    /// two share.
    pub(super) fn open_pane(&mut self, vertical: bool, command: Command) {
        let Some(at) = self.terminal() else {
            return;
        };
        let Some(tab) = self.tabs.get(self.selected) else {
            return;
        };
        // The room the new pane will have, which its program is started at.
        let added = TerminalId(u32::MAX);
        let mut root = tab.root.clone();
        root.split(at, added, vertical);
        let Some(rect) = self.lay_out(&root).rect(added) else {
            return;
        };
        let size = self.size_of(rect);
        let id = self.server.ask(Question::Split {
            terminal: at,
            vertical,
            command: self.filled(command),
            size,
        });
        self.asked.insert(id, Asked::Pane);
    }

    /// Asks the server to start `command` in a tab after the shown one: the custom
    /// command or the shell, in this program's environment unless it has its own.
    pub(super) fn open_tab(&mut self, mut command: Command) {
        let (Some(session), Some(size)) = (self.shown_session(), self.grid_size) else {
            return;
        };
        if command.directory.is_none() && self.tabs.is_empty() {
            command.directory = std::env::current_dir()
                .ok()
                .map(|path| Bytes(path.as_os_str().as_bytes().to_vec()));
        }
        let command = self.filled(command);
        let (start, count) = self.group(session);
        let index = if self
            .tabs
            .get(self.selected)
            .is_some_and(|tab| tab.session == session)
        {
            self.selected - start + 1
        } else {
            count
        };
        let id = self.server.ask(Question::OpenTab {
            session,
            index: u32::try_from(index).unwrap_or(u32::MAX),
            command,
            size,
        });
        self.asked.insert(id, Asked::Tab(size));
    }

    pub(super) fn tab_title(&self, terminal: TerminalId) -> String {
        self.tab_of(terminal)
            .map_or_else(String::new, |index| self.tabs[index].label().to_owned())
    }

    /// Whether `terminal`'s pane is in sight: in the tab shown, in the window that has
    /// the focus.
    pub(super) fn seen(&self, terminal: TerminalId) -> bool {
        self.chrome.active
            && self
                .tabs
                .get(self.selected)
                .is_some_and(|tab| tab.root.contains(terminal))
    }

    /// Marks `terminal`'s tab for something that happened in it, unless it is in sight;
    /// whether it was out of sight.
    pub(super) fn draw_attention(&mut self, terminal: TerminalId) -> bool {
        let Some(index) = self.tab_of(terminal).filter(|_| !self.seen(terminal)) else {
            return false;
        };
        if !self.tabs[index].attention {
            self.tabs[index].attention = true;
            self.retitle();
        }
        true
    }

    /// Takes the mark off the shown tab, which is in sight again.
    pub(super) fn attended(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.selected)
            && std::mem::take(&mut tab.attention)
        {
            self.retitle();
        }
    }

    /// Shows the shown tab's title on the window and every tab's in the strip.
    pub(super) fn retitle(&mut self) {
        let several = self.sessions.len() > 1;
        let labels: Vec<TabLabel> = self
            .tabs
            .iter()
            .map(|tab| TabLabel {
                title: if several {
                    format!("{} · {}", self.session_label(tab.session), tab.label())
                } else {
                    tab.label().to_owned()
                },
                attention: tab.attention,
            })
            .collect();
        let title = labels
            .get(self.selected)
            .map_or_else(|| "Caret".to_owned(), |label| label.title.clone());
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
