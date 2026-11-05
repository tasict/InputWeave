//! The repeats of a key held while input is captured, which Linux capture
//! makes up itself: X11's raw events carry none (the server makes them for
//! the focused window only), and Wayland leaves repeating to the client.
//! The last repeating key pressed repeats until it is released, at the
//! pace the backend reads from its display server; a key that does not
//! repeat leaves it be. Pure, so it is tested on every OS.

use std::time::{Duration, Instant};

use iw_core::HidUsage;
use iw_keymap::usage;

/// Whether `key` never repeats, on any keyboard: the modifiers and the lock
/// keys.
pub(crate) fn never_repeats(key: HidUsage) -> bool {
    key.is_modifier() || [usage::CAPS_LOCK, usage::NUM_LOCK, SCROLL_LOCK].contains(&key)
}

const SCROLL_LOCK: HidUsage = HidUsage::new(0x47);

/// How keys repeat, as the display server says: the key `id` stands for in
/// the backend (an X key code, or the key itself).
pub(crate) trait RepeatPace<K> {
    /// Whether `key` repeats at all.
    fn repeats(&self, id: K, key: HidUsage) -> bool;
    /// How long a key is held before it repeats.
    fn delay(&self) -> Duration;
    /// How long between repeats.
    fn interval(&self) -> Duration;
}

/// The key held that repeats, and when next.
#[derive(Debug)]
pub(crate) struct KeyRepeat<K> {
    held: Option<(K, HidUsage)>,
    next: Option<Instant>,
}

impl<K> Default for KeyRepeat<K> {
    fn default() -> Self {
        Self {
            held: None,
            next: None,
        }
    }
}

impl<K: Copy + PartialEq> KeyRepeat<K> {
    /// `key` went down at `now`: it repeats after the delay, if it repeats.
    pub(crate) fn press(&mut self, id: K, key: HidUsage, pace: &impl RepeatPace<K>, now: Instant) {
        if pace.repeats(id, key) {
            self.held = Some((id, key));
            self.next = Some(now + pace.delay());
        }
    }

    /// The key `id` went up: if it was the one repeating, nothing repeats.
    pub(crate) fn release(&mut self, id: K) {
        if self.held.is_some_and(|(held, _)| held == id) {
            self.clear();
        }
    }

    pub(crate) fn clear(&mut self) {
        self.held = None;
        self.next = None;
    }

    /// When the next repeat is due, if a key repeats.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.next
    }

    /// The key to repeat now, if its time came. One at a time: a repeat
    /// that is late is followed by the next an interval later, not at once,
    /// nor by a burst making up for those missed.
    pub(crate) fn due(&mut self, now: Instant, pace: &impl RepeatPace<K>) -> Option<HidUsage> {
        let (next, (_, key)) = (self.next?, self.held?);
        if now < next {
            return None;
        }
        let after = next + pace.interval();
        self.next = Some(if after > now {
            after
        } else {
            now + pace.interval()
        });
        Some(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Pace;

    impl RepeatPace<u8> for Pace {
        fn repeats(&self, _: u8, key: HidUsage) -> bool {
            !never_repeats(key)
        }

        fn delay(&self) -> Duration {
            Duration::from_millis(500)
        }

        fn interval(&self) -> Duration {
            Duration::from_millis(100)
        }
    }

    #[test]
    fn modifiers_and_lock_keys_never_repeat() {
        for key in [
            usage::LEFT_SHIFT,
            usage::RIGHT_ALT,
            usage::CAPS_LOCK,
            usage::NUM_LOCK,
            SCROLL_LOCK,
        ] {
            assert!(never_repeats(key), "{key:?}");
        }
        assert!(!never_repeats(usage::A));
    }

    #[test]
    fn the_last_key_held_repeats_one_at_a_time() {
        let start = Instant::now();
        let ms = |n| start + Duration::from_millis(n);
        let mut r = KeyRepeat::default();
        r.press(38, usage::A, &Pace, start);
        assert_eq!(r.due(ms(499), &Pace), None);
        assert_eq!(r.due(ms(500), &Pace), Some(usage::A));
        assert_eq!(r.deadline(), Some(ms(600)));
        // Late: once now, the next an interval from now.
        assert_eq!(r.due(ms(2000), &Pace), Some(usage::A));
        assert_eq!(r.due(ms(2000), &Pace), None);
        assert_eq!(r.deadline(), Some(ms(2100)));
        // A modifier leaves it be; another key's up too; its own up stops it.
        r.press(50, usage::LEFT_SHIFT, &Pace, ms(2050));
        r.release(50);
        assert_eq!(r.deadline(), Some(ms(2100)));
        r.release(38);
        assert_eq!(r.deadline(), None);
    }
}
