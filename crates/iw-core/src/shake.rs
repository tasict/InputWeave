//! Shaking the mouse to find the cursor.
//!
//! A shake is the pointer turning back and forth sideways, quickly: at least
//! [`ShakeDetector::TURNS`] turns within [`ShakeDetector::WINDOW`], where a
//! turn counts only once the pointer has gone [`ShakeDetector::STROKE`]
//! logical units back the other way, so every stroke between two turns is
//! at least that long. Vertical movement plays no part. One shake is
//! recognised once; the next can be recognised only after
//! [`ShakeDetector::COOLDOWN`]. The caller passes the time of every
//! movement.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::geometry::Point;

/// Recognises a shake in the pointer's movements.
#[derive(Clone, Debug, Default)]
pub struct ShakeDetector {
    /// Horizontal position: the sum of the movements so far.
    x: f64,
    /// The way the current stroke goes: 1 to the right, -1 to the left, 0
    /// before the first stroke.
    heading: f64,
    /// How far the current stroke has reached; before the first stroke,
    /// where the movement started.
    reach: f64,
    /// When the recent turns happened, oldest first.
    turns: VecDeque<Instant>,
    /// When a shake was last recognised.
    fired: Option<Instant>,
}

impl ShakeDetector {
    /// How many turns make a shake.
    pub const TURNS: usize = 4;
    /// The time all of them must fall within.
    pub const WINDOW: Duration = Duration::from_secs(1);
    /// How far the pointer must move back the other way, in logical units,
    /// before turning counts.
    pub const STROKE: f64 = 20.0;
    /// After a shake, how long until another is recognised.
    pub const COOLDOWN: Duration = Duration::from_millis(1500);

    pub fn new() -> Self {
        Self::default()
    }

    /// Notes a pointer movement by `delta` at `now`. Returns whether it
    /// completes a shake.
    pub fn moved(&mut self, delta: Point, now: Instant) -> bool {
        if delta.x == 0.0 || !delta.x.is_finite() {
            return false;
        }
        self.x += delta.x;
        if self.heading == 0.0 {
            let travelled = self.x - self.reach;
            if travelled.abs() >= Self::STROKE {
                self.heading = travelled.signum();
                self.reach = self.x;
            }
            return false;
        }
        let beyond = (self.x - self.reach) * self.heading;
        if beyond >= 0.0 {
            self.reach = self.x;
            return false;
        }
        if -beyond < Self::STROKE {
            return false;
        }
        // Far enough back: a stroke the other way has begun.
        self.heading = -self.heading;
        self.reach = self.x;
        self.turns.push_back(now);
        while self
            .turns
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) > Self::WINDOW)
        {
            self.turns.pop_front();
        }
        let rested = self
            .fired
            .is_none_or(|at| now.saturating_duration_since(at) >= Self::COOLDOWN);
        if self.turns.len() < Self::TURNS || !rested {
            return false;
        }
        self.turns.clear();
        self.fired = Some(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(start: Instant, ms: u64) -> Instant {
        start + Duration::from_millis(ms)
    }

    /// Moves sideways by `dx` in `steps` equal movements spread over
    /// `ms` milliseconds from `from`, and returns the times a shake was
    /// recognised, in milliseconds since `start`.
    fn stroke(
        shake: &mut ShakeDetector,
        start: Instant,
        from: u64,
        ms: u64,
        dx: f64,
        steps: u32,
    ) -> Vec<u64> {
        let mut fired = Vec::new();
        for step in 1..=steps {
            let at = from + ms * u64::from(step) / u64::from(steps);
            let delta = Point::new(dx / f64::from(steps), 0.0);
            if shake.moved(delta, after(start, at)) {
                fired.push(at);
            }
        }
        fired
    }

    /// Strokes of `dx` back and forth, each taking `ms`, starting to the
    /// right; returns when shakes were recognised.
    fn back_and_forth(
        shake: &mut ShakeDetector,
        start: Instant,
        strokes: u64,
        ms: u64,
        dx: f64,
    ) -> Vec<u64> {
        let mut fired = Vec::new();
        for i in 0..strokes {
            let sign = if i % 2 == 0 { 1.0 } else { -1.0 };
            fired.extend(stroke(shake, start, i * ms, ms, sign * dx, 8));
        }
        fired
    }

    #[test]
    fn a_quick_shake_is_recognised_on_the_fourth_turn() {
        let start = Instant::now();
        let mut shake = ShakeDetector::new();
        // Strokes of 60 a tenth of a second each: the fourth turn comes
        // 20 into the fifth stroke, 0.4 s and three steps in.
        assert_eq!(back_and_forth(&mut shake, start, 5, 100, 60.0), vec![437]);
    }

    #[test]
    fn whole_strokes_in_one_movement_each_count_too() {
        let start = Instant::now();
        let mut shake = ShakeDetector::new();
        let mut fired = Vec::new();
        for (i, dx) in [30.0, -30.0, 30.0, -30.0, 30.0].into_iter().enumerate() {
            let at = 50 * i as u64;
            if shake.moved(Point::new(dx, 0.0), after(start, at)) {
                fired.push(at);
            }
        }
        assert_eq!(fired, vec![200]);
    }

    #[test]
    fn slow_back_and_forth_is_not_a_shake() {
        let start = Instant::now();
        let mut shake = ShakeDetector::new();
        // A turn every 0.4 s: never four within a second.
        assert!(back_and_forth(&mut shake, start, 12, 400, 200.0).is_empty());
    }

    #[test]
    fn small_wiggles_are_not_a_shake() {
        let start = Instant::now();
        let mut shake = ShakeDetector::new();
        assert!(back_and_forth(&mut shake, start, 20, 50, 19.0).is_empty());
    }

    #[test]
    fn long_straight_moves_are_not_a_shake() {
        let start = Instant::now();
        let mut shake = ShakeDetector::new();
        assert!(stroke(&mut shake, start, 0, 500, 2000.0, 100).is_empty());
        assert!(stroke(&mut shake, start, 500, 500, -2000.0, 100).is_empty());
        // Nor is a hand that slips back a little as it goes.
        let mut at = 1000;
        for _ in 0..20 {
            assert!(stroke(&mut shake, start, at, 20, 60.0, 3).is_empty());
            assert!(stroke(&mut shake, start, at + 20, 10, -15.0, 2).is_empty());
            at += 30;
        }
    }

    #[test]
    fn vertical_movement_is_not_a_shake() {
        let start = Instant::now();
        let mut shake = ShakeDetector::new();
        for i in 0..20u64 {
            let dy = if i % 2 == 0 { 80.0 } else { -80.0 };
            // A little sideways drift does not make it one either.
            let dx = if i % 2 == 0 { 5.0 } else { -5.0 };
            assert!(!shake.moved(Point::new(0.0, dy), after(start, 50 * i)));
            assert!(!shake.moved(Point::new(dx, dy), after(start, 50 * i + 25)));
        }
    }

    #[test]
    fn one_shake_is_recognised_once_and_the_next_after_the_cooldown() {
        let start = Instant::now();
        let mut shake = ShakeDetector::new();
        // Shaking on for 1.4 s counts as one shake.
        assert_eq!(back_and_forth(&mut shake, start, 14, 100, 60.0), vec![437]);

        // Shaking on and on: again once 1.5 s have passed since the first.
        let mut shake = ShakeDetector::new();
        assert_eq!(
            back_and_forth(&mut shake, start, 25, 100, 60.0),
            vec![437, 1937]
        );

        // A pause, then another shake well after the cooldown. The last
        // stroke went right, so the first one left is already a turn.
        let mut shake = ShakeDetector::new();
        let mut fired = back_and_forth(&mut shake, start, 5, 100, 60.0);
        for i in 0..4 {
            let sign = if i % 2 == 0 { -1.0 } else { 1.0 };
            fired.extend(stroke(
                &mut shake,
                start,
                3000 + i * 100,
                100,
                sign * 60.0,
                8,
            ));
        }
        assert_eq!(fired, vec![437, 3337]);
    }

    #[test]
    fn turns_spread_over_more_than_a_second_do_not_add_up() {
        let start = Instant::now();
        let mut shake = ShakeDetector::new();
        // Three quick turns, a rest, then three more: never four within a
        // second.
        let mut fired = back_and_forth(&mut shake, start, 4, 100, 60.0);
        for i in 0..4 {
            let sign = if i % 2 == 0 { -1.0 } else { 1.0 };
            fired.extend(stroke(
                &mut shake,
                start,
                1500 + i * 100,
                100,
                sign * 60.0,
                8,
            ));
        }
        assert!(fired.is_empty(), "{fired:?}");
    }
}
