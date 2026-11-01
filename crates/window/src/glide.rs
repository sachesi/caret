//! A view of the history on its way to where the wheel or a finger sent it, by less than a
//! row at a time: a spring that the server's rows, which come whole, are shifted to follow.

use std::f64::consts::TAU;
use std::time::{Duration, Instant};

/// How long the spring seems to take, in seconds; it is nearly there in about this.
const RESPONSE: f64 = 0.4;

const PULL: f64 = TAU / RESPONSE;

/// Lines from where it is going, and lines a second, under which it is there: a fraction of
/// a pixel at any size of text.
const NEAR: f64 = 1.0 / 64.0;
const SLOW: f64 = 1.0 / 4.0;

/// How long after the last input a place between two rows is left for the nearest row.
pub const REST: Duration = Duration::from_millis(120);

/// How long rows asked for can take before the view is taken to be somewhere else.
const PATIENCE: Duration = Duration::from_millis(100);

#[derive(Debug, Clone, Copy)]
pub struct Glide {
    /// Rows up from the bottom, as drawn, and where it is going.
    position: f64,
    target: f64,
    /// Rows up a second.
    velocity: f64,
    stepped: Instant,
    touched: Instant,
    /// The rows at this offset were asked of the server, and when, if they have not come.
    asked: u32,
    waiting: Option<Instant>,
}

impl Glide {
    pub fn new(offset: u32, now: Instant) -> Self {
        Self {
            position: f64::from(offset),
            target: f64::from(offset),
            velocity: 0.0,
            stepped: now,
            touched: now,
            asked: offset,
            waiting: None,
        }
    }

    /// Sends it `up` rows further, up to `most`, at the speed it goes.
    pub fn turn(&mut self, up: f64, most: u32, now: Instant) {
        self.step(now);
        self.touched = now;
        self.target = (self.target + up).clamp(0.0, f64::from(most));
    }

    /// Moves it `up` rows at once, as what touches it does.
    pub fn drag(&mut self, up: f64, most: u32, now: Instant) {
        self.step(now);
        self.touched = now;
        self.position = (self.position + up).clamp(0.0, f64::from(most));
        self.target = self.position;
        self.velocity = 0.0;
    }

    /// Stops it where it is, to end on the nearest row.
    pub fn settle(&mut self, now: Instant) {
        self.step(now);
        self.target = self.position.round();
        self.velocity = 0.0;
    }

    /// Has it go on to the row nearest where it was going.
    pub fn rest(&mut self, now: Instant) {
        self.step(now);
        self.target = self.target.round();
    }

    /// Whether it is left between two rows and no input came for a while.
    pub fn left_between_rows(&self, now: Instant) -> bool {
        self.target.fract() != 0.0 && now.duration_since(self.touched) >= REST
    }

    /// Goes on to `now` along the spring, which holds at any time between two steps.
    pub fn step(&mut self, now: Instant) {
        let after = now.duration_since(self.stepped).as_secs_f64();
        self.stepped = now;
        let away = self.position - self.target;
        let drift = self.velocity + PULL * away;
        let decay = (-PULL * after).exp();
        let away = (away + drift * after) * decay;
        self.velocity = (self.velocity - PULL * drift * after) * decay;
        self.position = self.target + away;
        if away.abs() < NEAR && self.velocity.abs() < SLOW {
            self.position = self.target;
            self.velocity = 0.0;
        }
    }

    /// How many rows up to move the view the server shows so that it covers the position;
    /// the offset is then waited for.
    pub fn ask(&mut self, now: Instant) -> i32 {
        let whole = self.position.ceil() as u32;
        let moved = whole as i32 - self.asked as i32;
        if moved != 0 {
            self.asked = whole;
            self.waiting = Some(now);
        }
        moved
    }

    /// The server's frames say the view is at `offset`. Other than asked, with nothing on
    /// the way, it moved by itself: history grew under it, or keys sent it elsewhere.
    pub fn shown(&mut self, offset: u32, now: Instant) {
        if offset == self.asked {
            self.waiting = None;
            return;
        }
        if let Some(since) = self.waiting {
            if now.duration_since(since) < PATIENCE {
                return;
            }
            self.waiting = None;
        }
        let moved = f64::from(offset) - f64::from(self.asked);
        self.position = (self.position + moved).max(0.0);
        self.target = (self.target + moved).max(0.0);
        self.asked = offset;
    }

    pub fn is_waiting(&self) -> bool {
        self.waiting.is_some()
    }

    /// How many rows the view at `offset` is drawn up from its place, a row at most.
    pub fn shift(&self, offset: u32) -> f64 {
        (f64::from(offset) - self.position).clamp(0.0, 1.0)
    }

    /// Whether it will look different in the next frame.
    pub fn is_moving(&self) -> bool {
        self.position - self.target != 0.0 || self.waiting.is_some()
    }

    /// Whether it ended on a row, with that row's offset shown.
    pub fn is_done(&self) -> bool {
        !self.is_moving() && self.target.fract() == 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FRAME: Duration = Duration::from_micros(16_667);

    /// Steps a frame at a time, as if the server answered at once, until it is done, and
    /// gives the positions it went through.
    fn run(glide: &mut Glide, now: &mut Instant) -> Vec<f64> {
        let mut positions = Vec::new();
        for _ in 0..600 {
            *now += FRAME;
            glide.step(*now);
            glide.ask(*now);
            glide.shown(glide.asked, *now);
            positions.push(glide.position);
            if glide.is_done() {
                return positions;
            }
        }
        panic!("it never ended: {glide:?}");
    }

    #[test]
    fn it_ends_exactly_on_the_row_it_was_sent_to_without_going_past() {
        let mut now = Instant::now();
        let mut glide = Glide::new(10, now);
        glide.turn(6.0, 100, now);
        let positions = run(&mut glide, &mut now);
        assert_eq!(positions.last().copied(), Some(16.0));
        assert_eq!(glide.asked, 16);
        assert!(
            positions.windows(2).all(|pair| pair[0] <= pair[1]),
            "{positions:?}"
        );
        assert!(positions.iter().all(|&position| position <= 16.0));
    }

    #[test]
    fn it_starts_right_away_and_is_nearly_there_in_its_response() {
        let mut now = Instant::now();
        let mut glide = Glide::new(0, now);
        glide.turn(10.0, 100, now);
        let positions = run(&mut glide, &mut now);
        assert!(positions[0] > 0.0);
        let at_response = positions[(RESPONSE / FRAME.as_secs_f64()) as usize];
        assert!(at_response > 9.8, "{at_response}");
        assert!(positions.len() < 60, "{}", positions.len());
    }

    #[test]
    fn another_notch_on_the_way_keeps_its_speed() {
        let mut now = Instant::now();
        let mut glide = Glide::new(0, now);
        glide.turn(3.0, 100, now);
        now += FRAME * 6;
        glide.step(now);
        let speed = glide.velocity;
        assert!(speed > 0.0);
        glide.turn(3.0, 100, now);
        assert!((glide.velocity - speed).abs() < f64::EPSILON);
        assert_eq!(run(&mut glide, &mut now).last().copied(), Some(6.0));
    }

    #[test]
    fn the_time_between_steps_does_not_change_where_it_is() {
        let start = Instant::now();
        let mut often = Glide::new(0, start);
        let mut rarely = Glide::new(0, start);
        often.turn(20.0, 100, start);
        rarely.turn(20.0, 100, start);
        for frame in 1..=12 {
            often.step(start + FRAME * frame);
        }
        rarely.step(start + FRAME * 12);
        assert!((often.position - rarely.position).abs() < 1e-9);
        assert!((often.velocity - rarely.velocity).abs() < 1e-9);
    }

    #[test]
    fn it_stays_within_the_history() {
        let mut now = Instant::now();
        let mut glide = Glide::new(2, now);
        glide.turn(-10.0, 100, now);
        assert_eq!(run(&mut glide, &mut now).last().copied(), Some(0.0));
        glide.turn(500.0, 7, now);
        assert_eq!(run(&mut glide, &mut now).last().copied(), Some(7.0));
    }

    #[test]
    fn the_rows_asked_for_always_cover_where_it_is_drawn() {
        let mut now = Instant::now();
        let mut glide = Glide::new(5, now);
        glide.turn(30.0, 100, now);
        for _ in 0..40 {
            now += FRAME;
            glide.step(now);
            glide.ask(now);
            glide.shown(glide.asked, now);
            let shift = f64::from(glide.asked) - glide.position;
            assert!((0.0..1.0).contains(&shift), "{shift}");
            assert!((glide.shift(glide.asked) - shift).abs() < f64::EPSILON);
        }
    }

    #[test]
    fn rows_not_there_yet_hold_it_in_what_the_rows_can_show() {
        let now = Instant::now();
        let mut glide = Glide::new(5, now);
        glide.position = 7.5;
        assert!(glide.shift(5) == 0.0, "the rows above have not come");
        glide.position = 2.5;
        assert!(
            (glide.shift(5) - 1.0).abs() < f64::EPSILON,
            "one row is what there is"
        );
    }

    #[test]
    fn a_finger_moves_it_at_once_and_it_ends_on_a_row_once_left() {
        let mut now = Instant::now();
        let mut glide = Glide::new(5, now);
        glide.drag(1.25, 100, now);
        assert!((glide.position - 6.25).abs() < f64::EPSILON);
        assert!(!glide.is_moving(), "it stays where the finger is");
        assert!(!glide.left_between_rows(now));
        now += REST;
        assert!(glide.left_between_rows(now));
        glide.rest(now);
        assert_eq!(run(&mut glide, &mut now).last().copied(), Some(6.0));
    }

    #[test]
    fn a_press_at_speed_stops_it_on_the_nearest_row() {
        let mut now = Instant::now();
        let mut glide = Glide::new(0, now);
        glide.turn(40.0, 100, now);
        now += FRAME * 6;
        glide.step(now);
        let pressed_at = glide.position;
        assert!(glide.velocity > 30.0, "{glide:?}");
        glide.settle(now);
        let positions = run(&mut glide, &mut now);
        assert!(
            positions.iter().all(|&at| (at - pressed_at).abs() <= 0.5),
            "{pressed_at} {positions:?}"
        );
    }

    #[test]
    fn a_notch_ending_between_rows_goes_on_to_the_row_nearest_its_end() {
        let mut now = Instant::now();
        let mut glide = Glide::new(0, now);
        glide.turn(5.1, 100, now);
        now += REST;
        glide.step(now);
        assert!(glide.position < 4.0, "{glide:?}");
        assert!(glide.left_between_rows(now));
        glide.rest(now);
        assert_eq!(run(&mut glide, &mut now).last().copied(), Some(5.0));
    }

    #[test]
    fn the_view_moving_by_itself_moves_it_along() {
        let mut now = Instant::now();
        let mut glide = Glide::new(5, now);
        glide.turn(4.0, 100, now);
        glide.shown(8, now);
        assert!(
            (glide.target - 12.0).abs() < f64::EPSILON,
            "history grew by three"
        );
        glide.shown(0, now);
        assert_eq!(run(&mut glide, &mut now).last().copied(), Some(4.0));
    }

    #[test]
    fn a_stale_frame_is_waited_past_and_a_view_that_never_arrives_is_believed() {
        let mut now = Instant::now();
        let mut glide = Glide::new(5, now);
        glide.turn(4.0, 100, now);
        now += FRAME * 8;
        glide.step(now);
        glide.ask(now);
        let asked = glide.asked;
        assert!(asked > 5);
        glide.shown(5, now);
        assert_eq!(glide.asked, asked, "the old frame was on its way");
        assert!(glide.is_waiting());
        now += PATIENCE;
        glide.shown(5, now);
        assert_eq!(glide.asked, 5);
        assert!(!glide.is_waiting());
    }
}
