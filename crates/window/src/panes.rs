//! Where a tab's panes go in the window: the room each takes, the dividers between them,
//! and which pane is next to which.

use caret_protocol::TerminalId;
use caret_protocol::layout::{Node, Side, WHOLE};

/// A rectangle in device pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

impl Rect {
    pub fn contains(&self, (x, y): (f64, f64)) -> bool {
        x >= f64::from(self.x)
            && y >= f64::from(self.y)
            && x < f64::from(self.x + self.width)
            && y < f64::from(self.y + self.height)
    }

    /// How far `point` is outside the rectangle, by the farther of the two axes.
    fn distance(&self, (x, y): (f64, f64)) -> f64 {
        let along = |at: f64, start: i32, length: i32| {
            (f64::from(start) - at)
                .max(at - f64::from(start + length))
                .max(0.0)
        };
        along(x, self.x, self.width).max(along(y, self.y, self.height))
    }

    fn center(&self) -> (i32, i32) {
        (self.x + self.width / 2, self.y + self.height / 2)
    }
}

/// The gap between two sides of a split, which drags the split's divider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Divider {
    /// The first pane after it, which names it to the server.
    pub before: TerminalId,
    pub vertical: bool,
    pub gap: Rect,
    /// Where the split's room starts across the divider, and how long it is, gap
    /// included.
    pub start: i32,
    pub length: i32,
}

impl Divider {
    /// The share of the room the first side takes with the divider's middle at `at`.
    pub fn share_at(&self, at: f64, gap: i32) -> u16 {
        let room = f64::from((self.length - gap).max(1));
        let first = at - f64::from(self.start) - f64::from(gap) / 2.0;
        (first / room * f64::from(WHOLE))
            .round()
            .clamp(1.0, f64::from(WHOLE - 1)) as u16
    }

    /// The share of the room the first side takes with the divider following `point`.
    pub fn share_under(&self, (x, y): (f64, f64), gap: i32) -> u16 {
        self.share_at(if self.vertical { x } else { y }, gap)
    }
}

/// The square where an upright and a level divider meet, when one ends against the other.
fn junction(upright: &Divider, level: &Divider) -> Option<Rect> {
    let (u, l) = (upright.gap, level.gap);
    let level_ends = (l.x + l.width == u.x || l.x == u.x + u.width)
        && l.y >= u.y
        && l.y + l.height <= u.y + u.height;
    let upright_ends = (u.y + u.height == l.y || u.y == l.y + l.height)
        && u.x >= l.x
        && u.x + u.width <= l.x + l.width;
    (level_ends || upright_ends).then_some(Rect {
        x: u.x,
        y: l.y,
        width: u.width,
        height: l.height,
    })
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Placed {
    pub panes: Vec<(TerminalId, Rect)>,
    pub dividers: Vec<Divider>,
}

impl Placed {
    pub fn rect(&self, terminal: TerminalId) -> Option<Rect> {
        self.panes
            .iter()
            .find(|(pane, _)| *pane == terminal)
            .map(|&(_, rect)| rect)
    }

    pub fn pane_at(&self, point: (f64, f64)) -> Option<TerminalId> {
        self.panes
            .iter()
            .find(|(_, rect)| rect.contains(point))
            .map(|&(pane, _)| pane)
    }

    fn divider_at(&self, point: (f64, f64)) -> Option<Divider> {
        self.dividers
            .iter()
            .find(|divider| divider.gap.contains(point))
            .copied()
    }

    /// The dividers a drag from `point` moves: those meeting at the junction nearest it,
    /// if one is within `reach` pixels, or else the divider under it. Dividers lined up
    /// across a junction all meet there and stay lined up.
    pub fn dividers_at(&self, point: (f64, f64), reach: f64) -> Vec<Divider> {
        let meetings: Vec<(Rect, Divider, Divider)> = self
            .dividers
            .iter()
            .filter(|upright| upright.vertical)
            .flat_map(|upright| {
                self.dividers
                    .iter()
                    .filter(|level| !level.vertical)
                    .filter_map(|level| Some((junction(upright, level)?, *upright, *level)))
            })
            .collect();
        let nearest = meetings
            .iter()
            .map(|&(square, ..)| (square, square.distance(point)))
            .filter(|&(_, distance)| distance <= reach)
            .min_by(|one, other| one.1.total_cmp(&other.1));
        let Some((nearest, _)) = nearest else {
            return self.divider_at(point).into_iter().collect();
        };
        let mut held = Vec::new();
        for &(square, upright, level) in &meetings {
            if square == nearest {
                for divider in [upright, level] {
                    if !held.contains(&divider) {
                        held.push(divider);
                    }
                }
            }
        }
        held
    }
}

/// Lays `root` out in `area`, `gap` pixels between the sides of each split.
pub fn place(root: &Node, area: Rect, gap: i32) -> Placed {
    let mut placed = Placed::default();
    place_into(root, area, gap, &mut placed);
    placed
}

fn place_into(node: &Node, area: Rect, gap: i32, placed: &mut Placed) {
    match node {
        Node::Pane { terminal } => placed.panes.push((*terminal, area)),
        Node::Split {
            vertical,
            share,
            first,
            second,
        } => {
            let length = if *vertical { area.width } else { area.height };
            let room = (length - gap).max(0);
            let before = ((i64::from(room) * i64::from(*share) + i64::from(WHOLE) / 2)
                / i64::from(WHOLE)) as i32;
            let after = room - before;
            let (one, divider, two) = if *vertical {
                (
                    Rect {
                        width: before,
                        ..area
                    },
                    Rect {
                        x: area.x + before,
                        width: gap,
                        ..area
                    },
                    Rect {
                        x: area.x + before + gap,
                        width: after,
                        ..area
                    },
                )
            } else {
                (
                    Rect {
                        height: before,
                        ..area
                    },
                    Rect {
                        y: area.y + before,
                        height: gap,
                        ..area
                    },
                    Rect {
                        y: area.y + before + gap,
                        height: after,
                        ..area
                    },
                )
            };
            placed.dividers.push(Divider {
                before: second.first(),
                vertical: *vertical,
                gap: divider,
                start: if *vertical { area.x } else { area.y },
                length,
            });
            place_into(first, one, gap, placed);
            place_into(second, two, gap, placed);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    Left,
    Right,
    Up,
    Down,
}

/// The pane next to `from` towards `direction`: of those beside it that way, the one
/// nearest its middle.
pub fn neighbour(placed: &Placed, from: TerminalId, direction: Direction) -> Option<TerminalId> {
    let start = placed.rect(from)?;
    let (x, y) = start.center();
    placed
        .panes
        .iter()
        .filter(|&&(pane, rect)| {
            pane != from
                && match direction {
                    Direction::Left => rect.x + rect.width <= start.x,
                    Direction::Right => rect.x >= start.x + start.width,
                    Direction::Up => rect.y + rect.height <= start.y,
                    Direction::Down => rect.y >= start.y + start.height,
                }
                && match direction {
                    Direction::Left | Direction::Right => {
                        rect.y < start.y + start.height && rect.y + rect.height > start.y
                    }
                    Direction::Up | Direction::Down => {
                        rect.x < start.x + start.width && rect.x + rect.width > start.x
                    }
                }
        })
        .min_by_key(|(_, rect)| {
            let (other_x, other_y) = rect.center();
            (other_x - x).abs() + (other_y - y).abs()
        })
        .map(|&(pane, _)| pane)
}

/// The side of `rect` nearest `(x, y)`, unless the point is in its middle.
pub fn side_near(rect: Rect, (x, y): (f64, f64)) -> Option<Side> {
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
    (nearest <= 0.25).then_some(side)
}

/// The divider that moves `from`'s edge towards `direction` by a `cell` (wide, high), and
/// the share it gives the side before it then: the edge on that side, or the other one
/// where the pane has none there.
pub fn resized(
    placed: &Placed,
    from: TerminalId,
    direction: Direction,
    cell: (f64, f64),
    gap: i32,
) -> Option<(Divider, u16)> {
    let rect = placed.rect(from)?;
    let vertical = matches!(direction, Direction::Left | Direction::Right);
    let forward = matches!(direction, Direction::Right | Direction::Down);
    let divider = edge(placed, rect, vertical, forward)?;
    let step = if vertical { cell.0 } else { cell.1 };
    let (start, length, ..) = along(divider.gap, vertical);
    let middle = f64::from(start) + f64::from(length) / 2.0;
    let share = divider.share_at(
        if forward {
            middle + step
        } else {
            middle - step
        },
        gap,
    );
    Some((divider, share))
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
    use super::*;

    fn tree() -> Node {
        // 1 on the left; 2 over 3 on the right.
        let mut root = Node::Pane {
            terminal: TerminalId(1),
        };
        root.split(TerminalId(1), TerminalId(2), true);
        root.split(TerminalId(2), TerminalId(3), false);
        root
    }

    const AREA: Rect = Rect {
        x: 10,
        y: 20,
        width: 210,
        height: 110,
    };

    #[test]
    fn panes_share_the_room_around_their_dividers() {
        let placed = place(&tree(), AREA, 10);
        assert_eq!(
            placed.panes,
            [
                (
                    TerminalId(1),
                    Rect {
                        x: 10,
                        y: 20,
                        width: 100,
                        height: 110
                    }
                ),
                (
                    TerminalId(2),
                    Rect {
                        x: 120,
                        y: 20,
                        width: 100,
                        height: 50
                    }
                ),
                (
                    TerminalId(3),
                    Rect {
                        x: 120,
                        y: 80,
                        width: 100,
                        height: 50
                    }
                ),
            ]
        );
        let divider = placed.dividers_at((115.0, 50.0), 0.0)[0];
        assert_eq!(divider.before, TerminalId(2));
        assert_eq!(divider.share_at(115.0, 10), WHOLE / 2);
        assert_eq!(divider.share_at(65.0, 10), WHOLE / 4);
        assert_eq!(placed.pane_at((130.0, 100.0)), Some(TerminalId(3)));
        assert_eq!(placed.pane_at((115.0, 100.0)), None);
    }

    #[test]
    fn a_drag_from_where_two_dividers_meet_moves_both() {
        let mut root = tree();
        let placed = place(&root, AREA, 10);
        let held = placed.dividers_at((115.0, 75.0), 0.0);
        assert_eq!(held.len(), 2);
        for divider in &held {
            assert!(root.divide(divider.before, divider.share_under((65.0, 50.0), 10)));
        }
        let moved = place(&root, AREA, 10);
        assert_eq!(moved.rect(TerminalId(1)).unwrap().width, 50);
        assert_eq!(moved.rect(TerminalId(2)).unwrap().height, 25);
    }

    #[test]
    fn a_junction_reaches_a_little_past_its_square_and_no_further() {
        let placed = place(&tree(), AREA, 10);
        assert_eq!(placed.dividers_at((125.0, 65.0), 5.0).len(), 2);
        assert_eq!(placed.dividers_at((125.0, 65.0), 4.0).len(), 0);
        assert_eq!(placed.dividers_at((115.0, 50.0), 5.0).len(), 1);
        assert_eq!(placed.dividers_at((130.0, 100.0), 5.0).len(), 0);
    }

    #[test]
    fn dividers_lined_up_across_a_junction_move_together() {
        let grid = |right_share| {
            let mut root = Node::Pane {
                terminal: TerminalId(1),
            };
            root.split(TerminalId(1), TerminalId(2), true);
            root.split(TerminalId(1), TerminalId(3), false);
            root.split(TerminalId(2), TerminalId(4), false);
            root.divide(TerminalId(4), right_share);
            root
        };
        let held = place(&grid(WHOLE / 2), AREA, 10).dividers_at((115.0, 75.0), 0.0);
        assert_eq!(held.len(), 3);
        let held = place(&grid(WHOLE / 4), AREA, 10).dividers_at((115.0, 75.0), 0.0);
        assert_eq!(held.len(), 2);
    }

    #[test]
    fn the_pane_beside_is_found_each_way() {
        let placed = place(&tree(), AREA, 10);
        let next = |from, direction| neighbour(&placed, TerminalId(from), direction);
        assert_eq!(next(1, Direction::Right), Some(TerminalId(2)));
        assert_eq!(next(3, Direction::Left), Some(TerminalId(1)));
        assert_eq!(next(2, Direction::Down), Some(TerminalId(3)));
        assert_eq!(next(3, Direction::Up), Some(TerminalId(2)));
        assert_eq!(next(1, Direction::Left), None);
        assert_eq!(next(1, Direction::Up), None);
    }

    #[test]
    fn a_pane_resizes_by_the_edge_towards_the_key_or_else_its_other_edge() {
        let root = tree();
        let area = Rect {
            x: 0,
            y: 0,
            width: 210,
            height: 110,
        };
        let placed = place(&root, area, 10);
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

    #[test]
    fn a_step_of_a_cell_moves_a_divider_by_a_cell() {
        let mut root = tree();
        let area = Rect {
            x: 0,
            y: 0,
            width: 109,
            height: 27,
        };
        let width = |root: &Node| place(root, area, 1).rect(TerminalId(1)).unwrap().width;
        let before = width(&root);
        let (divider, share) = resized(
            &place(&root, area, 1),
            TerminalId(1),
            Direction::Right,
            (1.0, 1.0),
            1,
        )
        .unwrap();
        assert!(root.divide(divider.before, share));
        assert_eq!(width(&root), before + 1);
    }
}
