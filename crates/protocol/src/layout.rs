//! A tab's panes: a tree of splits whose leaves are terminals, as the server keeps it
//! and its clients draw it.

use crate::messages::TerminalId;
use crate::wire::variants;

/// What a `share` is out of.
pub const WHOLE: u16 = 10_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Pane {
        terminal: TerminalId,
    },
    Split {
        /// The divider is upright, so the two sides are next to each other.
        vertical: bool,
        /// The first side's part of the room, out of [`WHOLE`].
        share: u16,
        first: Box<Node>,
        second: Box<Node>,
    },
}
variants!(Node {
    0 => Pane { terminal },
    1 => Split { vertical, share, first, second },
});

/// Which side of a pane another goes to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Side {
    Left,
    Right,
    Above,
    Below,
}
variants!(Side {
    0 => Left,
    1 => Right,
    2 => Above,
    3 => Below,
});

impl Node {
    /// Every pane's terminal, left to right and top to bottom.
    #[must_use]
    pub fn panes(&self) -> Vec<TerminalId> {
        let mut panes = Vec::new();
        self.collect(&mut panes);
        panes
    }

    fn collect(&self, panes: &mut Vec<TerminalId>) {
        match self {
            Self::Pane { terminal } => panes.push(*terminal),
            Self::Split { first, second, .. } => {
                first.collect(panes);
                second.collect(panes);
            }
        }
    }

    #[must_use]
    pub fn first(&self) -> TerminalId {
        match self {
            Self::Pane { terminal } => *terminal,
            Self::Split { first, .. } => first.first(),
        }
    }

    #[must_use]
    pub fn contains(&self, terminal: TerminalId) -> bool {
        match self {
            Self::Pane { terminal: pane } => *pane == terminal,
            Self::Split { first, second, .. } => {
                first.contains(terminal) || second.contains(terminal)
            }
        }
    }

    /// Splits `at`'s pane in two halves, `added` after it; false when there is no such
    /// pane.
    pub fn split(&mut self, at: TerminalId, added: TerminalId, vertical: bool) -> bool {
        match self {
            Self::Pane { terminal } if *terminal == at => {
                *self = Self::Split {
                    vertical,
                    share: WHOLE / 2,
                    first: Box::new(Self::Pane { terminal: at }),
                    second: Box::new(Self::Pane { terminal: added }),
                };
                true
            }
            Self::Pane { .. } => false,
            Self::Split { first, second, .. } => {
                first.split(at, added, vertical) || second.split(at, added, vertical)
            }
        }
    }

    /// Puts `added` in half of `at`'s room, on its `side`; false when there is no such
    /// pane.
    pub fn beside(&mut self, at: TerminalId, added: TerminalId, side: Side) -> bool {
        let vertical = matches!(side, Side::Left | Side::Right);
        if !self.split(at, added, vertical) {
            return false;
        }
        if matches!(side, Side::Left | Side::Above) {
            self.swap(at, added);
        }
        true
    }

    /// Trades the places of two panes; false unless both are in the tree.
    pub fn swap(&mut self, one: TerminalId, other: TerminalId) -> bool {
        if !self.contains(one) || !self.contains(other) {
            return false;
        }
        self.trade(one, other);
        true
    }

    fn trade(&mut self, one: TerminalId, other: TerminalId) {
        match self {
            Self::Pane { terminal } if *terminal == one => *terminal = other,
            Self::Pane { terminal } if *terminal == other => *terminal = one,
            Self::Pane { .. } => {}
            Self::Split { first, second, .. } => {
                first.trade(one, other);
                second.trade(one, other);
            }
        }
    }

    /// The tree without `terminal`'s pane, its neighbour taking the room; none when it
    /// was the last.
    #[must_use]
    pub fn without(self, terminal: TerminalId) -> Option<Self> {
        match self {
            Self::Pane { terminal: pane } if pane == terminal => None,
            Self::Pane { .. } => Some(self),
            Self::Split {
                vertical,
                share,
                first,
                second,
            } => match (first.without(terminal), second.without(terminal)) {
                (Some(first), Some(second)) => Some(Self::Split {
                    vertical,
                    share,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(only), None) | (None, Some(only)) => Some(only),
                (None, None) => None,
            },
        }
    }

    /// The share of the side before the divider just before `terminal`'s pane.
    #[must_use]
    pub fn share(&self, terminal: TerminalId) -> Option<u16> {
        match self {
            Self::Pane { .. } => None,
            Self::Split {
                share,
                first,
                second,
                ..
            } => {
                if second.first() == terminal {
                    return Some(*share);
                }
                first.share(terminal).or_else(|| second.share(terminal))
            }
        }
    }

    /// Moves the divider just before `terminal`'s pane, giving the side before it
    /// `share`; false when no divider comes just before it.
    pub fn divide(&mut self, terminal: TerminalId, share: u16) -> bool {
        match self {
            Self::Pane { .. } => false,
            Self::Split {
                share: own,
                first,
                second,
                ..
            } => {
                if second.first() == terminal {
                    *own = share.clamp(1, WHOLE - 1);
                    return true;
                }
                first.divide(terminal, share) || second.divide(terminal, share)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(id: u32) -> Node {
        Node::Pane {
            terminal: TerminalId(id),
        }
    }

    #[test]
    fn panes_split_close_and_divide_in_order() {
        let mut root = pane(1);
        assert!(root.split(TerminalId(1), TerminalId(2), true));
        assert!(root.split(TerminalId(1), TerminalId(3), false));
        assert!(!root.split(TerminalId(9), TerminalId(4), false));
        assert_eq!(root.panes(), [TerminalId(1), TerminalId(3), TerminalId(2)]);

        assert!(root.divide(TerminalId(2), 7000));
        assert!(root.divide(TerminalId(3), 0));
        assert!(!root.divide(TerminalId(1), 5000));
        let Node::Split { share, first, .. } = &root else {
            panic!("{root:?}");
        };
        assert_eq!(*share, 7000);
        assert_eq!(root.share(TerminalId(2)), Some(7000));
        assert!(matches!(**first, Node::Split { share: 1, .. }));

        let root = root.without(TerminalId(1)).unwrap();
        assert_eq!(root.panes(), [TerminalId(3), TerminalId(2)]);
        assert_eq!(root.first(), TerminalId(3));
        let root = root.without(TerminalId(2)).unwrap();
        assert_eq!(root, pane(3));
        assert_eq!(root.without(TerminalId(3)), None);
    }

    #[test]
    fn panes_swap_and_go_beside_others() {
        let mut root = pane(1);
        root.split(TerminalId(1), TerminalId(2), true);
        assert!(root.swap(TerminalId(1), TerminalId(2)));
        assert_eq!(root.panes(), [TerminalId(2), TerminalId(1)]);
        assert!(!root.swap(TerminalId(1), TerminalId(9)));

        assert!(root.beside(TerminalId(1), TerminalId(3), Side::Above));
        let Node::Split { second, .. } = &root else {
            panic!("{root:?}");
        };
        assert!(matches!(
            **second,
            Node::Split {
                vertical: false,
                ..
            }
        ));
        assert_eq!(root.panes(), [TerminalId(2), TerminalId(3), TerminalId(1)]);
        assert!(root.beside(TerminalId(2), TerminalId(4), Side::Right));
        assert_eq!(
            root.panes(),
            [TerminalId(2), TerminalId(4), TerminalId(3), TerminalId(1)]
        );
        assert!(!root.beside(TerminalId(9), TerminalId(5), Side::Left));
    }

    #[test]
    fn a_tree_goes_through_the_wire_whole() {
        let mut root = pane(1);
        root.split(TerminalId(1), TerminalId(2), true);
        root.split(TerminalId(2), TerminalId(3), false);
        assert_eq!(
            crate::wire::decode::<Node>(&crate::wire::encode(&root)),
            Ok(root)
        );
    }
}
