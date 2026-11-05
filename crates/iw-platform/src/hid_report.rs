//! The reports of the virtual keyboard and mouse that inject input where
//! macOS ignores synthesized events: its lock screen and login window take
//! only input that looks like hardware (docs/architecture.md §2). The
//! device itself is created in `macos::hid`; this module has no FFI, so it
//! is tested on every OS.
//!
//! One composite device: report 1 is a boot-style keyboard (modifier bits,
//! a reserved byte, six keys), report 2 a mouse (five buttons, relative X
//! and Y, a wheel and horizontal pan). There is no output report, so the
//! system never asks the device to light Caps Lock and nothing has to
//! answer it.

use iw_core::HidUsage;
use iw_core::geometry::Point;
use iw_core::input::Button;

pub(crate) const KEYBOARD_REPORT_ID: u8 = 1;
pub(crate) const MOUSE_REPORT_ID: u8 = 2;

/// The HID report descriptor of the device.
#[rustfmt::skip]
pub(crate) const DESCRIPTOR: &[u8] = &[
    0x05, 0x01,             // Usage Page (Generic Desktop)
    0x09, 0x06,             // Usage (Keyboard)
    0xA1, 0x01,             // Collection (Application)
    0x85, KEYBOARD_REPORT_ID, //   Report ID
    0x05, 0x07,             //   Usage Page (Keyboard/Keypad)
    0x19, 0xE0,             //   Usage Minimum (Left Control)
    0x29, 0xE7,             //   Usage Maximum (Right GUI)
    0x15, 0x00,             //   Logical Minimum (0)
    0x25, 0x01,             //   Logical Maximum (1)
    0x75, 0x01,             //   Report Size (1)
    0x95, 0x08,             //   Report Count (8)
    0x81, 0x02,             //   Input (Data, Variable, Absolute): modifiers
    0x75, 0x08,             //   Report Size (8)
    0x95, 0x01,             //   Report Count (1)
    0x81, 0x01,             //   Input (Constant): reserved
    0x75, 0x08,             //   Report Size (8)
    0x95, 0x06,             //   Report Count (6)
    0x15, 0x00,             //   Logical Minimum (0)
    0x26, 0xFF, 0x00,       //   Logical Maximum (255)
    0x05, 0x07,             //   Usage Page (Keyboard/Keypad)
    0x19, 0x00,             //   Usage Minimum (0)
    0x2A, 0xFF, 0x00,       //   Usage Maximum (255)
    0x81, 0x00,             //   Input (Data, Array): the keys held
    0xC0,                   // End Collection

    0x05, 0x01,             // Usage Page (Generic Desktop)
    0x09, 0x02,             // Usage (Mouse)
    0xA1, 0x01,             // Collection (Application)
    0x85, MOUSE_REPORT_ID,  //   Report ID
    0x09, 0x01,             //   Usage (Pointer)
    0xA1, 0x00,             //   Collection (Physical)
    0x05, 0x09,             //     Usage Page (Button)
    0x19, 0x01,             //     Usage Minimum (1)
    0x29, 0x05,             //     Usage Maximum (5)
    0x15, 0x00,             //     Logical Minimum (0)
    0x25, 0x01,             //     Logical Maximum (1)
    0x75, 0x01,             //     Report Size (1)
    0x95, 0x05,             //     Report Count (5)
    0x81, 0x02,             //     Input (Data, Variable, Absolute): buttons
    0x75, 0x03,             //     Report Size (3)
    0x95, 0x01,             //     Report Count (1)
    0x81, 0x01,             //     Input (Constant): padding
    0x05, 0x01,             //     Usage Page (Generic Desktop)
    0x09, 0x30,             //     Usage (X)
    0x09, 0x31,             //     Usage (Y)
    0x16, 0x01, 0x80,       //     Logical Minimum (-32767)
    0x26, 0xFF, 0x7F,       //     Logical Maximum (32767)
    0x75, 0x10,             //     Report Size (16)
    0x95, 0x02,             //     Report Count (2)
    0x81, 0x06,             //     Input (Data, Variable, Relative)
    0x09, 0x38,             //     Usage (Wheel)
    0x15, 0x81,             //     Logical Minimum (-127)
    0x25, 0x7F,             //     Logical Maximum (127)
    0x75, 0x08,             //     Report Size (8)
    0x95, 0x01,             //     Report Count (1)
    0x81, 0x06,             //     Input (Data, Variable, Relative)
    0x05, 0x0C,             //     Usage Page (Consumer)
    0x0A, 0x38, 0x02,       //     Usage (AC Pan)
    0x15, 0x81,             //     Logical Minimum (-127)
    0x25, 0x7F,             //     Logical Maximum (127)
    0x75, 0x08,             //     Report Size (8)
    0x95, 0x01,             //     Report Count (1)
    0x81, 0x06,             //     Input (Data, Variable, Relative)
    0xC0,                   //   End Collection
    0xC0,                   // End Collection
];

/// A keyboard report: id, modifiers, reserved, six keys.
pub(crate) type KeyboardBytes = [u8; 9];
/// A mouse report: id, buttons, X and Y (little-endian), wheel, pan.
pub(crate) type MouseBytes = [u8; 8];

/// Keys that fit in one report at once besides the modifiers.
const KEYS: usize = 6;

/// What the virtual keyboard holds, turned into reports as keys change.
#[derive(Debug, Default)]
pub(crate) struct Keyboard {
    modifiers: u8,
    keys: Vec<u8>,
}

impl Keyboard {
    /// The report after `usage` went down or up, or `None` when nothing
    /// changes: a key that cannot be sent (beyond the array's range), a
    /// press of a key already down (macOS repeats a held hardware key
    /// itself), the release of a key not down, or a seventh key, which is
    /// left out until one of the six is released.
    pub(crate) fn key(&mut self, usage: HidUsage, pressed: bool) -> Option<KeyboardBytes> {
        let id = usage.id();
        if usage.is_modifier() {
            let bit = 1 << (id - 0xE0);
            let modifiers = if pressed {
                self.modifiers | bit
            } else {
                self.modifiers & !bit
            };
            if modifiers == self.modifiers {
                return None;
            }
            self.modifiers = modifiers;
            return Some(self.report());
        }
        let key = u8::try_from(id).ok().filter(|&k| k != 0)?;
        let held = self.keys.contains(&key);
        match (pressed, held) {
            (true, false) if self.keys.len() < KEYS => self.keys.push(key),
            (false, true) => self.keys.retain(|&k| k != key),
            _ => return None,
        }
        Some(self.report())
    }

    /// The keys held, modifiers first.
    pub(crate) fn held(&self) -> Vec<HidUsage> {
        (0..8)
            .filter(|bit| self.modifiers & (1 << bit) != 0)
            .map(|bit| HidUsage::new(0xE0 + bit))
            .chain(self.keys.iter().map(|&k| HidUsage::new(u16::from(k))))
            .collect()
    }

    /// The report that lets go of everything, if anything is held.
    pub(crate) fn release_all(&mut self) -> Option<KeyboardBytes> {
        if self.modifiers == 0 && self.keys.is_empty() {
            return None;
        }
        *self = Self::default();
        Some(self.report())
    }

    fn report(&self) -> KeyboardBytes {
        let mut bytes = [0; 9];
        bytes[0] = KEYBOARD_REPORT_ID;
        bytes[1] = self.modifiers;
        bytes[3..3 + self.keys.len()].copy_from_slice(&self.keys);
        bytes
    }
}

/// What the virtual mouse holds, and the fractions of motion and scrolling
/// not sent yet.
#[derive(Debug, Default)]
pub(crate) struct Mouse {
    buttons: u8,
    motion: (f64, f64),
    scroll: (f64, f64),
}

fn button_bit(button: Button) -> u8 {
    match button {
        Button::Left => 1,
        Button::Right => 1 << 1,
        Button::Middle => 1 << 2,
        Button::Back => 1 << 3,
        Button::Forward => 1 << 4,
    }
}

impl Mouse {
    /// The report after `button` went down or up, or `None` when it already
    /// was.
    pub(crate) fn button(&mut self, button: Button, pressed: bool) -> Option<MouseBytes> {
        let bit = button_bit(button);
        let buttons = if pressed {
            self.buttons | bit
        } else {
            self.buttons & !bit
        };
        if buttons == self.buttons {
            return None;
        }
        self.buttons = buttons;
        Some(self.report(0, 0, 0, 0))
    }

    /// Reports moving by (`dx`, `dy`) counts, in as many as the range of a
    /// report needs; fractions carry over to the next move.
    pub(crate) fn motion(&mut self, dx: f64, dy: f64) -> Vec<MouseBytes> {
        let (mut x, mut y) = whole(&mut self.motion, dx, dy);
        let mut reports = Vec::new();
        while x != 0 || y != 0 {
            let (step_x, step_y) = (clamp(x, i16::MAX), clamp(y, i16::MAX));
            reports.push(self.report(step_x as i16, step_y as i16, 0, 0));
            x -= step_x;
            y -= step_y;
        }
        reports
    }

    /// Reports turning the wheels by (`dx`, `dy`) notches, in the direction
    /// they physically turn: positive `dy` away from the user, positive
    /// `dx` to the right. The system applies natural scrolling to hardware
    /// itself, so nothing is undone here. Fractions carry over.
    pub(crate) fn scroll(&mut self, dx: f64, dy: f64) -> Vec<MouseBytes> {
        let (mut pan, mut wheel) = whole(&mut self.scroll, dx, dy);
        let mut reports = Vec::new();
        while pan != 0 || wheel != 0 {
            let (step_pan, step_wheel) = (clamp(pan, 127), clamp(wheel, 127));
            reports.push(self.report(0, 0, step_wheel as i8, step_pan as i8));
            pan -= step_pan;
            wheel -= step_wheel;
        }
        reports
    }

    /// The buttons held.
    pub(crate) fn held(&self) -> Vec<Button> {
        [
            Button::Left,
            Button::Right,
            Button::Middle,
            Button::Back,
            Button::Forward,
        ]
        .into_iter()
        .filter(|&b| self.buttons & button_bit(b) != 0)
        .collect()
    }

    /// The report that lets go of every button, if any is held.
    pub(crate) fn release_all(&mut self) -> Option<MouseBytes> {
        if self.buttons == 0 {
            return None;
        }
        self.buttons = 0;
        Some(self.report(0, 0, 0, 0))
    }

    fn report(&self, x: i16, y: i16, wheel: i8, pan: i8) -> MouseBytes {
        let (x, y) = (x.to_le_bytes(), y.to_le_bytes());
        [
            MOUSE_REPORT_ID,
            self.buttons,
            x[0],
            x[1],
            y[0],
            y[1],
            wheel as u8,
            pan as u8,
        ]
    }
}

/// Adds (`dx`, `dy`) to `carry` and takes out its whole part.
fn whole(carry: &mut (f64, f64), dx: f64, dy: f64) -> (i64, i64) {
    let x = carry.0 + if dx.is_finite() { dx } else { 0.0 };
    let y = carry.1 + if dy.is_finite() { dy } else { 0.0 };
    let (whole_x, whole_y) = (x.trunc(), y.trunc());
    *carry = (x - whole_x, y - whole_y);
    // Far beyond any real move; saturates instead of overflowing.
    (whole_x as i64, whole_y as i64)
}

fn clamp(value: i64, limit: i16) -> i64 {
    value.clamp(-i64::from(limit), i64::from(limit))
}

/// How much of the distance left each step covers. The system accelerates
/// a mouse's motion, so a step of n counts moves the cursor by anything
/// from about half to a few times n; covering half the distance keeps the
/// cursor from overshooting by more than it closed.
const APPROACH_GAIN: f64 = 0.5;
/// The longest step on each axis, in counts. Longer steps would be
/// accelerated more, and the cursor would swing around the target.
const APPROACH_MAX_STEP: f64 = 32.0;
/// Closer than this on both axes counts as there.
const APPROACH_SLACK: f64 = 0.5;

/// The relative step, in counts, that moves a mouse cursor at `current`
/// towards `target`, or `None` once it is there. A mouse can only move the
/// cursor by an amount the system then accelerates, so a target is reached
/// over several steps, each from where the cursor really is.
pub(crate) fn approach(current: Point, target: Point) -> Option<(i16, i16)> {
    let axis = |error: f64| -> i16 {
        if !error.is_finite() || error.abs() < APPROACH_SLACK {
            return 0;
        }
        let step = (error * APPROACH_GAIN).clamp(-APPROACH_MAX_STEP, APPROACH_MAX_STEP);
        // At least one count, or the last half point is never closed.
        let step = if step.abs() < 1.0 {
            step.signum()
        } else {
            step.round()
        };
        step as i16
    };
    let step = (axis(target.x - current.x), axis(target.y - current.y));
    (step != (0, 0)).then_some(step)
}

#[cfg(test)]
mod tests {
    use iw_keymap::usage;

    use super::*;

    /// Walks the items of a short-form report descriptor and checks that
    /// collections close and every item has its data, so the device would
    /// not be refused for a malformed descriptor.
    #[test]
    fn the_descriptor_is_well_formed() {
        let mut depth = 0i32;
        let mut ids = Vec::new();
        let mut i = 0;
        while i < DESCRIPTOR.len() {
            let prefix = DESCRIPTOR[i];
            let size = match prefix & 0b11 {
                3 => 4,
                n => n as usize,
            };
            assert!(i + size < DESCRIPTOR.len(), "item at {i} runs past the end");
            match prefix & 0xFC {
                0xA0 => depth += 1,
                0xC0 => depth -= 1,
                0x84 => ids.push(DESCRIPTOR[i + 1]),
                _ => {}
            }
            assert!(depth >= 0, "a collection closes before it opens at {i}");
            i += 1 + size;
        }
        assert_eq!(depth, 0);
        assert_eq!(ids, [KEYBOARD_REPORT_ID, MOUSE_REPORT_ID]);
    }

    #[test]
    fn modifiers_are_bits_and_keys_fill_the_array() {
        let mut keyboard = Keyboard::default();
        let shift = keyboard.key(usage::LEFT_SHIFT, true).unwrap();
        assert_eq!(shift, [1, 0b10, 0, 0, 0, 0, 0, 0, 0]);
        let a = keyboard.key(HidUsage::new(0x04), true).unwrap();
        assert_eq!(a, [1, 0b10, 0, 0x04, 0, 0, 0, 0, 0]);
        let gui = keyboard.key(usage::RIGHT_GUI, true).unwrap();
        assert_eq!(gui[1], 0b1000_0010);
        let up = keyboard.key(HidUsage::new(0x04), false).unwrap();
        assert_eq!(up, [1, 0b1000_0010, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(keyboard.held(), [usage::LEFT_SHIFT, usage::RIGHT_GUI]);
        assert_eq!(keyboard.release_all(), Some([1, 0, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(keyboard.release_all(), None);
        assert!(keyboard.held().is_empty());
    }

    #[test]
    fn repeats_and_strangers_change_nothing() {
        let mut keyboard = Keyboard::default();
        assert!(keyboard.key(HidUsage::new(0x05), true).is_some());
        // The controller's key repeat: macOS repeats a held key itself.
        assert_eq!(keyboard.key(HidUsage::new(0x05), true), None);
        assert_eq!(keyboard.key(HidUsage::new(0x06), false), None);
        assert_eq!(keyboard.key(usage::LEFT_SHIFT, false), None);
        // Beyond the array's range, and the reserved zero.
        assert_eq!(keyboard.key(HidUsage::new(0x0100), true), None);
        assert_eq!(keyboard.key(HidUsage::new(0), true), None);
    }

    #[test]
    fn a_seventh_key_waits_until_one_is_released() {
        let mut keyboard = Keyboard::default();
        for key in 0x04..0x0A {
            assert!(keyboard.key(HidUsage::new(key), true).is_some());
        }
        assert_eq!(keyboard.key(HidUsage::new(0x0A), true), None);
        // Never the "too many keys" error code in the array.
        assert_eq!(keyboard.key(HidUsage::new(0x0B), true), None, "still six");
        let up = keyboard.key(HidUsage::new(0x04), false).unwrap();
        assert_eq!(&up[3..], &[0x05, 0x06, 0x07, 0x08, 0x09, 0]);
        // The dropped key was never down, so its release is nothing.
        assert_eq!(keyboard.key(HidUsage::new(0x0A), false), None);
        let pressed = keyboard.key(HidUsage::new(0x0A), true).unwrap();
        assert_eq!(&pressed[3..], &[0x05, 0x06, 0x07, 0x08, 0x09, 0x0A]);
    }

    #[test]
    fn buttons_are_bits() {
        let mut mouse = Mouse::default();
        assert_eq!(
            mouse.button(Button::Left, true),
            Some([2, 1, 0, 0, 0, 0, 0, 0])
        );
        assert_eq!(mouse.button(Button::Left, true), None);
        assert_eq!(
            mouse.button(Button::Forward, true),
            Some([2, 0b1_0001, 0, 0, 0, 0, 0, 0])
        );
        assert_eq!(
            mouse.button(Button::Left, false),
            Some([2, 0b1_0000, 0, 0, 0, 0, 0, 0])
        );
        // Motion keeps the buttons held, as a drag.
        assert_eq!(mouse.motion(1.0, 0.0)[0][1], 0b1_0000);
        assert_eq!(mouse.held(), [Button::Forward]);
        assert_eq!(mouse.release_all(), Some([2, 0, 0, 0, 0, 0, 0, 0]));
        assert_eq!(mouse.release_all(), None);
        assert!(mouse.held().is_empty());
    }

    #[test]
    fn motion_is_split_and_fractions_carry() {
        let mut mouse = Mouse::default();
        let reports = mouse.motion(-3.0, 70_000.0);
        assert_eq!(reports.len(), 3);
        let y = |r: &MouseBytes| i16::from_le_bytes([r[4], r[5]]);
        let x = |r: &MouseBytes| i16::from_le_bytes([r[2], r[3]]);
        assert_eq!(reports.iter().map(y).map(i64::from).sum::<i64>(), 70_000);
        assert_eq!(reports.iter().map(x).map(i64::from).sum::<i64>(), -3);
        assert!(mouse.motion(0.4, 0.0).is_empty());
        let r = mouse.motion(0.7, 0.0);
        assert_eq!(r.len(), 1);
        assert_eq!(x(&r[0]), 1);
        assert!(mouse.motion(f64::NAN, f64::INFINITY).is_empty());
    }

    #[test]
    fn scrolling_keeps_the_physical_direction() {
        let mut mouse = Mouse::default();
        // Away from the user is a positive wheel; to the right a positive pan.
        assert_eq!(mouse.scroll(-1.0, 2.0), vec![[2, 0, 0, 0, 0, 0, 2, 0xFF]]);
        assert!(mouse.scroll(0.0, 0.5).is_empty());
        assert_eq!(mouse.scroll(0.0, 0.5), vec![[2, 0, 0, 0, 0, 0, 1, 0]]);
        let many = mouse.scroll(0.0, -300.0);
        assert_eq!(many.len(), 3);
        assert_eq!(
            many.iter().map(|r| i64::from(r[6] as i8)).sum::<i64>(),
            -300
        );
    }

    /// Moves a simulated cursor by each step times what the system's
    /// acceleration makes of it, and returns where it ends up.
    fn settle(from: Point, target: Point, steps: usize, gain: impl Fn(f64) -> f64) -> Point {
        let mut at = from;
        for _ in 0..steps {
            let Some((dx, dy)) = approach(at, target) else {
                break;
            };
            let (dx, dy) = (f64::from(dx), f64::from(dy));
            at = Point::new(at.x + dx * gain(dx.abs()), at.y + dy * gain(dy.abs()));
        }
        at
    }

    fn off(a: Point, b: Point) -> f64 {
        (a.x - b.x).abs().max((a.y - b.y).abs())
    }

    #[test]
    fn approaching_converges_whatever_the_acceleration() {
        let from = Point::new(10.0, 900.0);
        let target = Point::new(1500.0, 20.0);
        for gain in [0.5, 1.0, 2.0] {
            let at = settle(from, target, 400, |_| gain);
            assert!(off(at, target) <= 1.5, "gain {gain}: ended at {at:?}");
        }
        // Accelerated more the longer the step, as the system does, up to
        // four times; a single count moves the cursor less than a point.
        let at = settle(from, target, 400, |n| (0.4 + n / 16.0).min(4.0));
        assert!(off(at, target) <= 1.5, "ended at {at:?}");
        // Even three times at every step it swings no further than this.
        let at = settle(from, target, 400, |_| 3.0);
        assert!(off(at, target) <= 2.0, "ended at {at:?}");
    }

    #[test]
    fn approaching_stops_once_there() {
        let p = Point::new(100.0, 100.0);
        assert_eq!(approach(p, p), None);
        assert_eq!(approach(p, Point::new(100.4, 99.6)), None);
        assert_eq!(approach(p, Point::new(101.0, 100.0)), Some((1, 0)));
        assert_eq!(approach(p, Point::new(0.0, 10_000.0)), Some((-32, 32)));
        assert_eq!(approach(p, Point::new(f64::NAN, 100.0)), None);
    }
}
