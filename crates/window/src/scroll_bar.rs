//! Where a pane's scroll bar is and what a place along it stands for, in device pixels.

use crate::panes::Rect;

/// The thumb of the scroll bar along a pane.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScrollBar {
    /// Where the thumb runs, the whole height of the pane.
    pub track: Rect,
    pub top: i32,
    pub height: i32,
}

impl ScrollBar {
    /// Where the thumb of the pane at `pane` runs: `width` wide, ending `clearance` short of
    /// where a line `thickness` wide goes in the middle of a `gap` beside the pane. A thick
    /// line pushes it into the pane rather than under the line.
    pub fn track(pane: Rect, gap: i32, thickness: i32, width: i32, clearance: i32) -> Rect {
        let line = pane.x + pane.width + (gap - thickness).div_euclid(2);
        Rect {
            x: line - clearance - width,
            width,
            ..pane
        }
    }

    /// The bar of a view of `lines` rows that is `up` rows above the bottom of `history`
    /// rows, the thumb being at least `shortest` long; none with no history to scroll.
    pub fn new(track: Rect, shortest: i32, history: u32, lines: usize, up: f64) -> Option<Self> {
        if history == 0 || track.height <= 0 {
            return None;
        }
        let history = f64::from(history);
        let lines = lines as f64;
        let height = (f64::from(track.height) * lines / (history + lines))
            .round()
            .max(f64::from(shortest))
            .min(f64::from(track.height)) as i32;
        let free = f64::from(track.height - height);
        let top = track.y + (free * (1.0 - (up / history).clamp(0.0, 1.0))).round() as i32;
        Some(Self { track, top, height })
    }

    pub fn thumb_contains(&self, y: f64) -> bool {
        y >= f64::from(self.top) && y < f64::from(self.top + self.height)
    }

    /// The line of `history`, from its top, that has the thumb's top `y` pixels down.
    pub fn line_at(&self, y: f64, history: u32) -> u32 {
        let free = f64::from((self.track.height - self.height).max(1));
        let along = ((y - f64::from(self.track.y)) / free).clamp(0.0, 1.0);
        (along * f64::from(history)).round() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TRACK: Rect = Rect {
        x: 500,
        y: 100,
        width: 8,
        height: 400,
    };

    fn bar(history: u32, lines: usize, up: f64) -> ScrollBar {
        ScrollBar::new(TRACK, 40, history, lines, up).unwrap()
    }

    #[test]
    fn the_thumb_stays_clear_of_the_line_between_panes() {
        let pane = Rect {
            x: 10,
            y: 20,
            width: 400,
            height: 300,
        };
        for gap in [12, 18, 24] {
            for thickness in 1..=gap + 4 {
                let track = ScrollBar::track(pane, gap, thickness, 4, 1);
                let line = pane.x + pane.width + (gap - thickness).div_euclid(2);
                assert!(track.x + track.width < line, "{gap} {thickness}");
                assert_eq!(track.height, pane.height);
            }
        }
    }

    #[test]
    fn no_history_leaves_nothing_to_scroll() {
        assert_eq!(ScrollBar::new(TRACK, 40, 0, 24, 0.0), None);
    }

    #[test]
    fn the_thumb_ends_on_the_track_at_either_end_of_the_history() {
        assert_eq!(bar(76, 24, 76.0).top, 100);
        assert_eq!(bar(76, 24, 76.0).height, 96);
        let bottom = bar(76, 24, 0.0);
        assert_eq!(bottom.top + bottom.height, 500);
    }

    #[test]
    fn a_long_history_keeps_the_thumb_big_enough_to_hold() {
        let thumb = bar(1_000_000, 24, 0.0);
        assert_eq!(thumb.height, 40);
        assert_eq!(thumb.top + thumb.height, 500);
    }

    #[test]
    fn a_view_scrolled_further_than_the_history_stays_on_the_track() {
        assert_eq!(bar(76, 24, 80.0).top, 100);
    }

    #[test]
    fn the_thumb_moved_to_a_line_shows_that_line() {
        let history = 76;
        for line in 0..=history {
            let up = f64::from(history - line);
            let at = bar(history, 24, up);
            assert_eq!(at.line_at(f64::from(at.top), history), line);
        }
    }

    #[test]
    fn past_the_track_are_its_ends() {
        let thumb = bar(76, 24, 0.0);
        assert_eq!(thumb.line_at(-50.0, 76), 0);
        assert_eq!(thumb.line_at(5000.0, 76), 76);
    }
}
