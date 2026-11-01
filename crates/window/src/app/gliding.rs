//! Scrolling through the history by less than a row at a time, with `smooth-scroll` on: the
//! panes' springs step once a frame, the server is asked for rows as they cross into view,
//! and only the panes' places change in the frames between.

use std::time::{Duration, Instant};

use caret_protocol::{Request, Scroll, TerminalId};
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};

use super::App;
use crate::glide::{Glide, REST};

/// The first protocol version with the row under a view scrolled up.
const ROW_BELOW: u16 = 6;

/// How long a frame waits for the rows it needs, which take a fraction of this. Past it, the
/// frame shows the rows there are, as a busy server has to go on being drawn.
const ROWS_DUE: Duration = Duration::from_millis(4);

/// What moves the view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Motion {
    /// A wheel, whose notches the spring eases.
    Wheel,
    /// A touchpad or a finger, which the text stays under.
    Touch,
}

impl App {
    /// Whether the view of the history follows less than a row at a time.
    pub(super) fn glides(&self) -> bool {
        self.config.smooth_scroll && self.server.version >= ROW_BELOW
    }

    /// Scrolls the view of `terminal` by `down` rows, or up for negative ones.
    pub(super) fn glide(&mut self, terminal: TerminalId, down: f64, motion: Motion) {
        let Some(grid) = self.grid_of(terminal) else {
            return;
        };
        let (offset, history) = (grid.offset, grid.history);
        if history == 0 {
            return;
        }
        let now = Instant::now();
        let Some(pane) = self
            .tabs
            .get_mut(self.selected)
            .and_then(|tab| tab.panes.get_mut(&terminal))
        else {
            return;
        };
        let glide = pane.glide.get_or_insert_with(|| Glide::new(offset, now));
        match motion {
            Motion::Wheel => glide.turn(-down, history, now),
            Motion::Touch => glide.drag(-down, history, now),
        }
        self.rest_later();
        if !self.waiting {
            self.step_glides();
        }
        self.dirty = true;
        self.draw();
    }

    /// Ends the way of the view of `terminal`, as what sends it elsewhere does: a spring still
    /// going would ask for rows of its own, against that.
    pub(super) fn stop_glide(&mut self, terminal: TerminalId) {
        let stopped = self
            .tabs
            .get_mut(self.selected)
            .and_then(|tab| tab.panes.get_mut(&terminal))
            .is_some_and(|pane| pane.glide.take().is_some());
        if stopped {
            self.dirty = true;
        }
    }

    /// Keeps a timer going while the tab shown has views on their way, for those left
    /// between two rows when what moved them stops.
    fn rest_later(&mut self) {
        if self.glide_rest.is_some() {
            return;
        }
        let timer = self
            .events
            .insert_source(Timer::from_duration(REST), |_, (), app| {
                if !app.waiting {
                    app.step_glides();
                    app.draw();
                }
                let going = app
                    .tabs
                    .get(app.selected)
                    .is_some_and(|tab| tab.panes.values().any(|pane| pane.glide.is_some()));
                if going {
                    TimeoutAction::ToDuration(REST)
                } else {
                    app.glide_rest = None;
                    TimeoutAction::Drop
                }
            });
        match timer {
            Ok(timer) => self.glide_rest = Some(timer),
            Err(error) => tracing::warn!("timing the end of a scroll: {error}"),
        }
    }

    /// Moves the springs of the tab shown on to now, asking for the rows they come to.
    pub(super) fn step_glides(&mut self) {
        let held = self.touch.holds_a_scroll();
        let Some(tab) = self.tabs.get_mut(self.selected) else {
            return;
        };
        let now = Instant::now();
        let focused = tab.focused;
        let mut asked = false;
        let mut going = false;
        for (&terminal, pane) in &mut tab.panes {
            let Some(glide) = pane.glide.as_mut() else {
                continue;
            };
            let grid = if terminal == focused {
                &self.grid
            } else {
                &pane.grid
            };
            glide.shown(grid.offset, now);
            if !held && glide.left_between_rows(now) {
                glide.rest(now);
            }
            glide.step(now);
            let lines = glide.ask(now);
            if lines != 0 {
                self.server.send(&Request::Scroll {
                    terminal,
                    scroll: Scroll::Lines { lines },
                });
                asked = true;
            }
            going |= glide.is_moving();
            if glide.is_done() {
                going = true;
                pane.glide = None;
            }
        }
        if going {
            self.dirty = true;
        }
        if asked {
            self.ask_frame();
            self.wait_for_rows();
        }
    }

    /// Holds the next frame back for the rows just asked for, so that it shows them where
    /// the spring is now and not a frame late.
    fn wait_for_rows(&mut self) {
        if self.rows_due.is_some() {
            return;
        }
        let timer = self
            .events
            .insert_source(Timer::from_duration(ROWS_DUE), |_, (), app| {
                app.rows_due = None;
                app.draw();
                TimeoutAction::Drop
            });
        match timer {
            Ok(timer) => self.rows_due = Some(timer),
            Err(error) => tracing::warn!("timing the rows of a scroll: {error}"),
        }
    }

    /// A frame of `terminal` came with the view at `offset`: the frame held back for it goes
    /// once no pane still waits.
    pub(super) fn rows_came(&mut self, terminal: TerminalId, offset: u32) {
        let Some(tab) = self.tabs.get_mut(self.selected) else {
            return;
        };
        if let Some(glide) = tab
            .panes
            .get_mut(&terminal)
            .and_then(|pane| pane.glide.as_mut())
        {
            glide.shown(offset, Instant::now());
        }
        let waiting = tab
            .panes
            .values()
            .filter_map(|pane| pane.glide.as_ref())
            .any(Glide::is_waiting);
        if !waiting && let Some(timer) = self.rows_due.take() {
            self.events.remove(timer);
        }
    }

    /// Ends the views of the tab shown on the nearest row, as a press holds them there.
    pub(super) fn settle_glides(&mut self) {
        let now = Instant::now();
        let Some(tab) = self.tabs.get_mut(self.selected) else {
            return;
        };
        for glide in tab
            .panes
            .values_mut()
            .filter_map(|pane| pane.glide.as_mut())
        {
            glide.settle(now);
        }
    }

    /// How many pixels up from its place the pane of `terminal` is drawn.
    pub(super) fn glide_shift(&self, terminal: TerminalId) -> i32 {
        let Some(glide) = self
            .tabs
            .get(self.selected)
            .and_then(|tab| tab.panes.get(&terminal))
            .and_then(|pane| pane.glide.as_ref())
        else {
            return 0;
        };
        let Some(grid) = self.grid_of(terminal) else {
            return 0;
        };
        (glide.shift(grid.offset) * f64::from(self.fonts.cell().height)).round() as i32
    }

    /// Whether the pane of `terminal` is on its way, so that each frame shows it anew.
    pub(super) fn is_gliding(&self, terminal: TerminalId) -> bool {
        self.tabs
            .get(self.selected)
            .and_then(|tab| tab.panes.get(&terminal))
            .and_then(|pane| pane.glide.as_ref())
            .is_some_and(Glide::is_moving)
    }
}
