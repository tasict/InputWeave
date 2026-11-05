//! Guards against stuck keys and lost control.
//!
//! A receiver tracks every key and button it has pressed on behalf of a
//! remote controller and releases all of them when the controller leaves,
//! disconnects or stops sending heartbeats.

use std::collections::BTreeSet;
use std::time::{Duration, Instant};

use crate::HidUsage;
use crate::input::Button;

/// Keys and buttons currently held down by injected input.
#[derive(Clone, Debug, Default)]
pub struct PressedTracker {
    keys: BTreeSet<HidUsage>,
    buttons: BTreeSet<Button>,
}

/// What `PressedTracker::release_all` let go of.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Released {
    pub keys: Vec<HidUsage>,
    pub buttons: Vec<Button>,
}

impl Released {
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.buttons.is_empty()
    }
}

impl PressedTracker {
    /// Records a key event. Returns whether it changes state; a release of a
    /// key that is not held should not be injected.
    pub fn key(&mut self, usage: HidUsage, pressed: bool) -> bool {
        if pressed {
            self.keys.insert(usage);
            true
        } else {
            self.keys.remove(&usage)
        }
    }

    /// Records a button event. Returns whether it changes state.
    pub fn button(&mut self, button: Button, pressed: bool) -> bool {
        if pressed {
            self.buttons.insert(button)
        } else {
            self.buttons.remove(&button)
        }
    }

    /// Whether any mouse button is held.
    pub fn buttons_held(&self) -> bool {
        !self.buttons.is_empty()
    }

    /// Forgets everything held and returns it, so the caller can inject the
    /// matching releases.
    pub fn release_all(&mut self) -> Released {
        Released {
            keys: std::mem::take(&mut self.keys).into_iter().collect(),
            buttons: std::mem::take(&mut self.buttons).into_iter().collect(),
        }
    }
}

/// Tracks whether a peer is still alive, based on when it was last heard
/// from. The caller supplies the current time.
#[derive(Clone, Copy, Debug)]
pub struct Liveness {
    timeout: Duration,
    last_seen: Instant,
}

impl Liveness {
    pub fn new(now: Instant, timeout: Duration) -> Self {
        Self {
            timeout,
            last_seen: now,
        }
    }

    pub fn seen(&mut self, now: Instant) {
        self.last_seen = self.last_seen.max(now);
    }

    pub fn is_expired(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.last_seen) > self.timeout
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iw_keymap::usage;

    #[test]
    fn releases_everything_held_once() {
        let mut t = PressedTracker::default();
        t.key(usage::LEFT_SHIFT, true);
        t.key(usage::A, true);
        t.button(Button::Left, true);
        assert_eq!(
            t.release_all(),
            Released {
                keys: vec![usage::A, usage::LEFT_SHIFT],
                buttons: vec![Button::Left],
            }
        );
        assert!(t.release_all().is_empty());
    }

    #[test]
    fn released_keys_are_not_released_again() {
        let mut t = PressedTracker::default();
        t.key(usage::A, true);
        assert!(t.key(usage::A, false));
        assert!(!t.key(usage::A, false));
        assert!(t.release_all().is_empty());
    }

    #[test]
    fn repeated_presses_are_held_once() {
        let mut t = PressedTracker::default();
        assert!(t.key(usage::A, true));
        assert!(t.key(usage::A, true));
        assert!(t.button(Button::Right, true));
        assert!(!t.button(Button::Right, true));
        assert_eq!(t.release_all().keys, vec![usage::A]);
    }

    #[test]
    fn liveness_expires_after_timeout() {
        let start = Instant::now();
        let mut l = Liveness::new(start, Duration::from_millis(1000));
        assert!(!l.is_expired(start + Duration::from_millis(1000)));
        assert!(l.is_expired(start + Duration::from_millis(1001)));
        l.seen(start + Duration::from_millis(900));
        assert!(!l.is_expired(start + Duration::from_millis(1800)));
        assert!(l.is_expired(start + Duration::from_millis(1901)));
    }

    #[test]
    fn liveness_ignores_out_of_order_times() {
        let start = Instant::now();
        let mut l = Liveness::new(
            start + Duration::from_millis(500),
            Duration::from_millis(1000),
        );
        l.seen(start);
        assert!(!l.is_expired(start + Duration::from_millis(1500)));
    }
}
