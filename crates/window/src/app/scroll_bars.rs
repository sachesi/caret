//! The panes' scroll bars, in the padding to their right so that they cover no text: which
//! show, and scrolling with the pointer on one.

use caret_protocol::{Mode, Request, Scroll, TerminalId};

use super::App;
use crate::chrome;
use crate::config::ScrollBars;
use crate::panes::Rect;
use crate::scroll_bar::ScrollBar;

/// The thumb's width, and what it leaves clear of the line between panes, in logical pixels.
const WIDTH: u32 = 4;
const CLEARANCE: u32 = 1;
/// The shortest a thumb gets, in logical pixels.
const SHORTEST: u32 = 24;
/// How far into a pane a press still takes hold of its bar, in logical pixels: the padding
/// alone is too narrow to hit.
const REACH: u32 = 6;

/// A scroll bar the left button holds.
#[derive(Debug, Clone, Copy)]
pub(super) struct HeldBar {
    terminal: TerminalId,
    /// How far under the thumb's top it was taken, in device pixels.
    grab: f64,
    /// The line last asked for.
    line: Option<u32>,
}

impl App {
    /// Whether the pane of `terminal` shows its bar now.
    fn shows_bar(&self, terminal: TerminalId) -> bool {
        match self.config.scroll_bars {
            ScrollBars::Hidden => false,
            ScrollBars::Visible => true,
            ScrollBars::Hover => {
                self.pointer.over_pane == Some(terminal)
                    || self
                        .pointer
                        .held_bar
                        .is_some_and(|held| held.terminal == terminal)
            }
        }
    }

    /// The bar of the pane of `terminal` at `rect`, if it has history to scroll.
    fn bar_of(&self, terminal: TerminalId, rect: Rect) -> Option<ScrollBar> {
        let grid = self.grid_of(terminal)?;
        if grid.mode.contains(Mode::ALT_SCREEN) {
            return None;
        }
        let track = ScrollBar::track(
            rect,
            self.gap(),
            self.pane_line_thickness(),
            self.device(WIDTH) as i32,
            self.device(CLEARANCE) as i32,
        );
        let height = f64::from(self.fonts.cell().height);
        let up = f64::from(grid.offset) - f64::from(self.glide_shift(terminal)) / height;
        ScrollBar::new(
            track,
            self.device(SHORTEST) as i32,
            grid.history,
            grid.lines,
            up,
        )
    }

    /// The thumbs of the panes shown, drawn over them.
    pub(super) fn bar_quads(&self, placed: &crate::panes::Placed) -> Vec<chrome::Quad> {
        placed
            .panes
            .iter()
            .filter(|&&(terminal, _)| self.shows_bar(terminal))
            .filter_map(|&(terminal, rect)| {
                let bar = self.bar_of(terminal, rect)?;
                let held = self
                    .pointer
                    .held_bar
                    .is_some_and(|held| held.terminal == terminal);
                let color = match (self.chrome.dark, held) {
                    (true, false) => 0xffff_ff66,
                    (true, true) => 0xffff_ffb3,
                    (false, false) => 0x0000_0066,
                    (false, true) => 0x0000_00b3,
                };
                Some(chrome::Quad {
                    x: bar.track.x as f32,
                    y: bar.top as f32,
                    width: bar.track.width as f32,
                    height: bar.height as f32,
                    color,
                    radius: bar.track.width as f32 / 2.0,
                    corners: chrome::ALL_CORNERS,
                    ..chrome::Quad::default()
                })
            })
            .collect()
    }

    /// The pane whose room the pointer is in, its padding to the right included.
    fn pane_by_pointer(&self) -> Option<TerminalId> {
        let padding = self.device(super::PADDING) as i32;
        let at = self.device_point();
        self.placed()
            .panes
            .into_iter()
            .find(|(_, rect)| {
                Rect {
                    width: rect.width + padding,
                    ..*rect
                }
                .contains(at)
            })
            .map(|(terminal, _)| terminal)
    }

    /// Shows the bar of the pane the pointer has come to, with `scroll-bars` at `"hover"`.
    pub(super) fn follow_pane_under_pointer(&mut self) {
        if self.config.scroll_bars != ScrollBars::Hover {
            return;
        }
        let over = if self.chrome.hovered.is_none() {
            self.pane_by_pointer()
        } else {
            None
        };
        self.pointer_over_pane(over);
    }

    /// The pointer is over the room of `pane`.
    pub(super) fn pointer_over_pane(&mut self, pane: Option<TerminalId>) {
        if self.pointer.over_pane != pane {
            self.pointer.over_pane = pane;
            self.dirty = true;
            self.draw();
        }
    }

    /// The bar shown where a press at the pointer takes hold of it: a pane's own edge, and
    /// the padding beyond.
    fn bar_by_pointer(&self) -> Option<(TerminalId, ScrollBar)> {
        let (x, y) = self.device_point();
        let reach = f64::from(self.device(REACH));
        let padding = f64::from(self.device(super::PADDING));
        let (terminal, rect) = self.placed().panes.into_iter().find(|&(terminal, rect)| {
            let edge = f64::from(rect.x + rect.width);
            self.shows_bar(terminal)
                && (edge - reach..edge + padding).contains(&x)
                && (f64::from(rect.y)..f64::from(rect.y + rect.height)).contains(&y)
        })?;
        Some((terminal, self.bar_of(terminal, rect)?))
    }

    /// Whether a thumb is under the pointer.
    pub(super) fn over_thumb(&self) -> bool {
        let (_, y) = self.device_point();
        self.bar_by_pointer()
            .is_some_and(|(_, bar)| bar.thumb_contains(y))
    }

    /// Whether a press where the pointer is would take hold of a bar.
    pub(super) fn over_bar(&self) -> bool {
        self.bar_by_pointer().is_some()
    }

    /// Takes hold of the bar under the pointer, its thumb going there if it is elsewhere.
    pub(super) fn hold_bar(&mut self) {
        let Some((terminal, bar)) = self.bar_by_pointer() else {
            return;
        };
        let (_, y) = self.device_point();
        let grab = if bar.thumb_contains(y) {
            y - f64::from(bar.top)
        } else {
            f64::from(bar.height) / 2.0
        };
        self.pointer.held_bar = Some(HeldBar {
            terminal,
            grab,
            line: None,
        });
        self.set_cursor();
        self.drag_bar();
        self.dirty = true;
        self.draw();
    }

    /// Scrolls the pane whose bar is held to where the pointer has taken the thumb.
    pub(super) fn drag_bar(&mut self) {
        let Some(held) = self.pointer.held_bar else {
            return;
        };
        let Some(rect) = self.placed().rect(held.terminal) else {
            return;
        };
        let Some(bar) = self.bar_of(held.terminal, rect) else {
            return;
        };
        let Some(history) = self.grid_of(held.terminal).map(|grid| grid.history) else {
            return;
        };
        let (_, y) = self.device_point();
        let line = bar.line_at(y - held.grab, history);
        if held.line == Some(line) {
            return;
        }
        self.pointer.held_bar = Some(HeldBar {
            line: Some(line),
            ..held
        });
        // A spring still going would ask for rows of its own, against the pointer.
        if let Some(pane) = self
            .tabs
            .get_mut(self.selected)
            .and_then(|tab| tab.panes.get_mut(&held.terminal))
        {
            pane.glide = None;
        }
        self.server.send(&Request::Scroll {
            terminal: held.terminal,
            scroll: Scroll::To { line },
        });
    }

    /// Lets go of the bar held, saying whether there was one.
    pub(super) fn let_go_of_bar(&mut self) -> bool {
        if self.pointer.held_bar.take().is_none() {
            return false;
        }
        self.follow_pane_under_pointer();
        self.set_cursor();
        self.dirty = true;
        self.draw();
        true
    }
}
