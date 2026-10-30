//! Control mode: the keys move, resize, zoom and close panes until it is left, a bar
//! naming them over the bottom of the window.

use caret_protocol::encode::Modifiers;
use caret_protocol::layout::Side;
use caret_protocol::{Request, TerminalId};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use super::App;
use crate::chrome::{self, Quad};
use crate::panes::{self, Direction, Divider, Placed, Rect};

const HINTS: &str =
    "Arrows focus · Shift swap · Ctrl resize · V S split · X close · Z zoom · T tab · D detach";

impl App {
    pub(super) fn enter_control(&mut self) {
        self.control = true;
        self.chrome.hints = Some(HINTS.to_owned());
        self.dirty = true;
        self.draw();
    }

    fn leave_control(&mut self) {
        self.control = false;
        self.chrome.hints = None;
        self.dirty = true;
        self.draw();
    }

    /// Does what a key pressed in control mode says; any other key leaves the mode.
    pub(super) fn control_key(&mut self, event: &KeyEvent) {
        let key = event.keysym;
        if key.is_modifier_key() {
            return;
        }
        let Modifiers { ctrl, shift, .. } = self.modifiers;
        let direction = match key {
            Keysym::Left | Keysym::h | Keysym::H => Some(Direction::Left),
            Keysym::Right | Keysym::l | Keysym::L => Some(Direction::Right),
            Keysym::Up | Keysym::k | Keysym::K => Some(Direction::Up),
            Keysym::Down | Keysym::j | Keysym::J => Some(Direction::Down),
            _ => None,
        };
        if let Some(direction) = direction {
            if ctrl {
                self.resize_towards(direction);
            } else if shift {
                self.swap_towards(direction);
            } else {
                self.focus_towards(direction);
            }
            return;
        }
        match key {
            Keysym::v | Keysym::V => self.split(true),
            Keysym::s | Keysym::S => self.split(false),
            Keysym::x | Keysym::X => {
                if let Some(terminal) = self.terminal() {
                    self.close_pane(terminal);
                }
            }
            Keysym::z | Keysym::Z => self.toggle_zoom(),
            Keysym::t | Keysym::T => self.pane_to_tab(),
            Keysym::d | Keysym::D => {
                self.leave_control();
                self.end(true);
            }
            _ => self.leave_control(),
        }
    }

    /// The ring around the focused pane while in control mode.
    pub(super) fn control_ring(&self, placed: &Placed) -> Option<Quad> {
        if !self.control {
            return None;
        }
        let rect = placed.rect(self.terminal()?)?;
        let ring = self.device(2) as f32;
        let (x, y) = (rect.x as f32, rect.y as f32);
        let (width, height) = (rect.width as f32, rect.height as f32);
        Some(Quad {
            x: x - ring,
            y: y - ring,
            width: width + 2.0 * ring,
            height: height + 2.0 * ring,
            color: chrome::ACCENT,
            cut: [x, y, width, height],
            ..Quad::default()
        })
    }

    /// Whether the server can move panes, saying so when it cannot.
    fn moves_panes(&mut self) -> bool {
        let moves = self.server.version >= 2;
        if !moves {
            self.toast("Moving panes needs a newer terminal server".to_owned());
        }
        moves
    }

    /// Trades the focused pane's place with the one next to it towards `direction`.
    fn swap_towards(&mut self, direction: Direction) {
        let Some(from) = self.terminal() else {
            return;
        };
        if let Some(other) = panes::neighbour(&self.placed(), from, direction)
            && self.moves_panes()
        {
            self.server.send(&Request::SwapPanes {
                terminal: from,
                other,
            });
        }
    }

    /// Moves the edge of the focused pane towards `direction` by a cell: the edge on that
    /// side, or the other one where the pane has none there.
    fn resize_towards(&mut self, direction: Direction) {
        let Some(terminal) = self.terminal() else {
            return;
        };
        let placed = self.placed();
        let Some(rect) = placed.rect(terminal) else {
            return;
        };
        let vertical = matches!(direction, Direction::Left | Direction::Right);
        let forward = matches!(direction, Direction::Right | Direction::Down);
        let Some(divider) = edge(&placed, rect, vertical, forward) else {
            return;
        };
        let cell = self.fonts.cell();
        let step = f64::from(if vertical { cell.width } else { cell.height });
        let (start, length, ..) = along(divider.gap, vertical);
        let middle = f64::from(start) + f64::from(length) / 2.0;
        let share = divider.share_at(
            if forward {
                middle + step
            } else {
                middle - step
            },
            self.gap(),
        );
        if let Some(tab) = self.tabs.get_mut(self.selected)
            && tab.root.divide(divider.before, share)
        {
            self.resize_panes(self.selected);
            self.server.send(&Request::Divide {
                terminal: divider.before,
                share,
            });
            self.dirty = true;
            self.draw();
        }
    }

    /// Lets the focused pane take the whole tab, or gives the others their room back.
    fn toggle_zoom(&mut self) {
        let Some(tab) = self.tabs.get_mut(self.selected) else {
            return;
        };
        if tab.panes.len() < 2 && !tab.zoomed {
            return;
        }
        tab.zoomed = !tab.zoomed;
        self.resize_panes(self.selected);
        self.dirty = true;
        self.draw();
    }

    /// Gives the other panes of the tab shown their room back.
    pub(super) fn unzoom(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.selected)
            && std::mem::take(&mut tab.zoomed)
        {
            self.resize_panes(self.selected);
        }
    }

    /// Where the pane held would go if let go of where the pointer is.
    fn landing(&self, placed: &Placed) -> Option<Landing> {
        let moving = self.pointer.moving?;
        let point = self.device_point();
        // Over the header or the tab strip, the window's or the compositor's.
        if point.1 < f64::from(self.area()?.y) {
            return Some(Landing::Tab);
        }
        let target = placed.pane_at(point).filter(|&pane| pane != moving)?;
        Some(landing_in(placed.rect(target)?, point, target))
    }

    /// The part of the pane where the one held would go, tinted.
    pub(super) fn landing_quad(&self, placed: &Placed) -> Option<Quad> {
        let (target, side) = match self.landing(placed)? {
            Landing::Swap(target) => (target, None),
            Landing::Beside(target, side) => (target, Some(side)),
            Landing::Tab => return None,
        };
        let rect = placed.rect(target)?;
        let (mut x, mut y) = (rect.x as f32, rect.y as f32);
        let (mut width, mut height) = (rect.width as f32, rect.height as f32);
        match side {
            Some(Side::Left) => width /= 2.0,
            Some(Side::Right) => {
                width /= 2.0;
                x += width;
            }
            Some(Side::Above) => height /= 2.0,
            Some(Side::Below) => {
                height /= 2.0;
                y += height;
            }
            None => {}
        }
        Some(Quad {
            x,
            y,
            width,
            height,
            color: chrome::ACCENT & !0xff | 0x40,
            ..Quad::default()
        })
    }

    /// Moves the pane held to where the pointer let go of it.
    pub(super) fn drop_pane(&mut self) {
        let landing = self.landing(&self.placed());
        let Some(moving) = self.pointer.moving.take() else {
            return;
        };
        self.dirty = true;
        self.draw();
        let Some(landing) = landing else {
            return;
        };
        if !self.moves_panes() {
            return;
        }
        match landing {
            Landing::Swap(other) => self.server.send(&Request::SwapPanes {
                terminal: moving,
                other,
            }),
            Landing::Beside(beside, side) => self.server.send(&Request::MovePane {
                terminal: moving,
                beside,
                side,
            }),
            Landing::Tab => self.pane_to_tab(),
        }
    }

    /// Takes the focused pane out to a tab of its own.
    fn pane_to_tab(&mut self) {
        let Some(terminal) = self.terminal() else {
            return;
        };
        if self
            .tabs
            .get(self.selected)
            .is_some_and(|tab| tab.panes.len() > 1)
            && self.moves_panes()
        {
            self.moved_pane = Some(terminal);
            self.server.send(&Request::PaneToTab { terminal });
        }
    }
}

/// Where a pane dragged goes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Landing {
    /// In the place of this pane, which takes the dragged one's.
    Swap(TerminalId),
    /// To this side of this pane, the two sharing its room.
    Beside(TerminalId, Side),
    /// To a tab of its own.
    Tab,
}

/// Where a pane let go of at `point` over `target`'s `rect` goes: beside it at the edge
/// nearest, unless the point is in its middle.
fn landing_in(rect: Rect, (x, y): (f64, f64), target: TerminalId) -> Landing {
    let across = (x - f64::from(rect.x)) / f64::from(rect.width.max(1));
    let down = (y - f64::from(rect.y)) / f64::from(rect.height.max(1));
    let (nearest, side) = [
        (across, Side::Left),
        (1.0 - across, Side::Right),
        (down, Side::Above),
        (1.0 - down, Side::Below),
    ]
    .into_iter()
    .fold((f64::INFINITY, Side::Left), |near, edge| {
        if edge.0 < near.0 { edge } else { near }
    });
    if nearest > 0.25 {
        Landing::Swap(target)
    } else {
        Landing::Beside(target, side)
    }
}

/// A rectangle's start, length, and start and length across, along an axis.
fn along(rect: Rect, vertical: bool) -> (i32, i32, i32, i32) {
    if vertical {
        (rect.x, rect.width, rect.y, rect.height)
    } else {
        (rect.y, rect.height, rect.x, rect.width)
    }
}

/// The divider at `rect`'s edge after it, `forward`, or before it, falling back to the
/// other edge.
fn edge(placed: &Placed, rect: Rect, vertical: bool, forward: bool) -> Option<Divider> {
    let (start, length, across, span) = along(rect, vertical);
    let edges = placed
        .dividers
        .iter()
        .filter(|divider| divider.vertical == vertical)
        .filter_map(|divider| {
            let (gap_start, gap_length, gap_across, gap_span) = along(divider.gap, vertical);
            if gap_across >= across + span || gap_across + gap_span <= across {
                return None;
            }
            if gap_start == start + length {
                Some((true, *divider))
            } else if gap_start + gap_length == start {
                Some((false, *divider))
            } else {
                None
            }
        });
    let (mut after, mut before) = (None, None);
    for (is_after, divider) in edges {
        if is_after {
            after.get_or_insert(divider);
        } else {
            before.get_or_insert(divider);
        }
    }
    if forward {
        after.or(before)
    } else {
        before.or(after)
    }
}

#[cfg(test)]
mod tests {
    use caret_protocol::TerminalId;
    use caret_protocol::layout::Node;

    use super::*;

    #[test]
    fn a_pane_let_go_of_near_an_edge_goes_beside_and_in_the_middle_swaps() {
        let rect = Rect {
            x: 100,
            y: 100,
            width: 200,
            height: 100,
        };
        let target = TerminalId(7);
        assert_eq!(
            landing_in(rect, (110.0, 150.0), target),
            Landing::Beside(target, Side::Left)
        );
        assert_eq!(
            landing_in(rect, (200.0, 195.0), target),
            Landing::Beside(target, Side::Below)
        );
        assert_eq!(
            landing_in(rect, (200.0, 150.0), target),
            Landing::Swap(target)
        );
    }

    #[test]
    fn a_pane_resizes_by_the_edge_towards_the_key_or_else_its_other_edge() {
        // 1 on the left; 2 over 3 on the right.
        let mut root = Node::Pane {
            terminal: TerminalId(1),
        };
        root.split(TerminalId(1), TerminalId(2), true);
        root.split(TerminalId(2), TerminalId(3), false);
        let area = Rect {
            x: 0,
            y: 0,
            width: 210,
            height: 110,
        };
        let placed = panes::place(&root, area, 10);
        let left = placed.rect(TerminalId(1)).unwrap();
        let top_right = placed.rect(TerminalId(2)).unwrap();
        let before = |divider: Option<Divider>| divider.map(|divider| divider.before);
        assert_eq!(before(edge(&placed, left, true, true)), Some(TerminalId(2)));
        assert_eq!(
            before(edge(&placed, left, true, false)),
            Some(TerminalId(2))
        );
        assert_eq!(before(edge(&placed, left, false, true)), None);
        assert_eq!(
            before(edge(&placed, top_right, false, false)),
            Some(TerminalId(3))
        );
    }
}
