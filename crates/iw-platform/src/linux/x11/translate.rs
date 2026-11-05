//! The parts of the X11 backends that need no X server: key codes,
//! buttons, wheel clicks, raw valuators, the modifier states of passive key
//! grabs and the displays RandR describes.

use std::time::Duration;

use iw_core::HidUsage;
use iw_core::geometry::{Point, Rect};
use iw_core::hotkey::{Modifier, Modifiers};
use iw_core::input::Button;
use iw_core::layout::Display;
use iw_core::router::Hotkey;
use iw_keymap::usage;

use crate::linux::repeat::{RepeatPace, never_repeats};

/// X key codes are evdev codes plus 8 under the evdev rules XKB uses on
/// Linux, with both the evdev and the libinput drivers.
const EVDEV_OFFSET: u16 = 8;

/// The X key code of `key`, if the server's range has it.
pub(crate) fn keycode(key: HidUsage, min: u8, max: u8) -> Option<u8> {
    let code = key.to_evdev()?.checked_add(EVDEV_OFFSET)?;
    let code = u8::try_from(code).ok()?;
    (min..=max).contains(&code).then_some(code)
}

/// The key an X key code stands for.
pub(crate) fn key_of(keycode: u32) -> Option<HidUsage> {
    let evdev = u16::try_from(keycode).ok()?.checked_sub(EVDEV_OFFSET)?;
    HidUsage::from_evdev(evdev)
}

/// X's pointer buttons: 1 left, 2 middle, 3 right, 4–7 the wheel's clicks,
/// 8 and 9 back and forward.
pub(crate) fn x_button(button: Button) -> u8 {
    match button {
        Button::Left => 1,
        Button::Middle => 2,
        Button::Right => 3,
        Button::Back => 8,
        Button::Forward => 9,
    }
}

pub(crate) fn button_of(detail: u32) -> Option<Button> {
    Some(match detail {
        1 => Button::Left,
        2 => Button::Middle,
        3 => Button::Right,
        8 => Button::Back,
        9 => Button::Forward,
        _ => return None,
    })
}

const WHEEL_UP: u8 = 4;
const WHEEL_DOWN: u8 = 5;
const WHEEL_LEFT: u8 = 6;
const WHEEL_RIGHT: u8 = 7;

/// The notches a click of wheel button `detail` stands for, in the
/// direction of `InputEvent::Scroll`: (dx, dy), positive dy away from the
/// user (up), positive dx to the right.
pub(crate) fn wheel_notches(detail: u32) -> Option<(f64, f64)> {
    match u8::try_from(detail).ok()? {
        WHEEL_UP => Some((0.0, 1.0)),
        WHEEL_DOWN => Some((0.0, -1.0)),
        WHEEL_LEFT => Some((-1.0, 0.0)),
        WHEEL_RIGHT => Some((1.0, 0.0)),
        _ => None,
    }
}

/// Keys and buttons injection holds down. The server repeats a held key by
/// itself, so a press of a key already down, which a controlling device
/// sends as its own repeat, is not sent again: the key would repeat twice
/// as fast.
#[derive(Debug, Default)]
pub(crate) struct Held {
    down: Vec<u8>,
}

impl Held {
    /// Whether to send the press or release of `code`. Releases always go,
    /// so a key the server thinks is down is let go of whatever this
    /// remembers.
    pub(crate) fn sends(&self, code: u8, pressed: bool) -> bool {
        !pressed || !self.down.contains(&code)
    }

    /// The press or release of `code` went to the server.
    pub(crate) fn sent(&mut self, code: u8, pressed: bool) {
        self.down.retain(|c| *c != code);
        if pressed {
            self.down.push(code);
        }
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.down.is_empty()
    }

    /// What is down, now let go of.
    pub(crate) fn take(&mut self) -> Vec<u8> {
        std::mem::take(&mut self.down)
    }
}

/// How the server repeats a held key: XKB's RepeatKeys control.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RepeatSettings {
    pub on: bool,
    pub delay: Duration,
    pub interval: Duration,
    /// A bit for each X key code: whether the key repeats.
    pub per_key: [u8; 32],
}

impl Default for RepeatSettings {
    /// The X server's defaults: 660 ms, then 25 a second, every key.
    fn default() -> Self {
        Self {
            on: true,
            delay: Duration::from_millis(660),
            interval: Duration::from_millis(40),
            per_key: [0xff; 32],
        }
    }
}

impl RepeatPace<u8> for RepeatSettings {
    /// Modifiers and lock keys never repeat, whatever the bits say.
    fn repeats(&self, code: u8, key: HidUsage) -> bool {
        self.on
            && !never_repeats(key)
            && self.per_key[usize::from(code / 8)] & (1 << (code % 8)) != 0
    }

    fn delay(&self) -> Duration {
        self.delay
    }

    fn interval(&self) -> Duration {
        self.interval
    }
}

/// The repeats of a held key, which raw events do not carry: the server
/// makes them up for the window with the focus only. Capture reports them
/// while grabbed, when they are the controlled device's to have, as the
/// server would (`linux::repeat`), keyed by X key code.
pub(crate) type KeyRepeat = crate::linux::repeat::KeyRepeat<u8>;

/// What capture reports of keys and buttons. It remembers what it
/// reported down, to report it released when the connection breaks; and
/// while input waits for a grab another app holds, it reports only the
/// release of those and presses completing a hotkey the router acts on,
/// with the modifiers held for it. So input never goes to another device
/// while it also reaches local apps, yet the user can still take control
/// back meanwhile.
#[derive(Debug, Default)]
pub(crate) struct Reports {
    keys_down: Vec<HidUsage>,
    buttons_down: Vec<Button>,
    /// Modifier keys down here, reported or not.
    modifiers_down: Vec<HidUsage>,
}

impl Reports {
    /// The key events to report as `key` goes down or up.
    pub(crate) fn key(
        &mut self,
        key: HidUsage,
        pressed: bool,
        waiting: bool,
        hotkeys: &[Hotkey],
    ) -> Vec<(HidUsage, bool)> {
        if Modifier::of(key).is_some() {
            self.modifiers_down.retain(|k| *k != key);
            if pressed {
                self.modifiers_down.push(key);
            }
        }
        let was_down = self.keys_down.contains(&key);
        if !pressed {
            if waiting && !was_down {
                return Vec::new();
            }
            self.keys_down.retain(|k| *k != key);
            return vec![(key, false)];
        }
        let mut out = Vec::new();
        if waiting {
            let held = |k: HidUsage| self.modifiers_down.contains(&k);
            if !hotkeys.iter().any(|h| h.matches(key, held)) {
                return out;
            }
            for &m in &self.modifiers_down {
                if m != key && !self.keys_down.contains(&m) {
                    self.keys_down.push(m);
                    out.push((m, true));
                }
            }
        }
        if !was_down {
            self.keys_down.push(key);
        }
        out.push((key, true));
        out
    }

    /// Whether to report a button going down or up.
    pub(crate) fn button(&mut self, button: Button, pressed: bool, waiting: bool) -> bool {
        let was_down = self.buttons_down.contains(&button);
        if waiting && (pressed || !was_down) {
            return false;
        }
        self.buttons_down.retain(|b| *b != button);
        if pressed {
            self.buttons_down.push(button);
        }
        true
    }

    /// Everything reported down, let go of now, as the connection broke.
    pub(crate) fn release_all(&mut self) -> (Vec<HidUsage>, Vec<Button>) {
        self.modifiers_down.clear();
        (
            std::mem::take(&mut self.keys_down),
            std::mem::take(&mut self.buttons_down),
        )
    }
}

/// How far grabbed capture guesses the hidden cursor went since it last
/// looked, from the movements read: where it really is is read only every
/// [`Drift::CHECK`] pixels of movement, not at every burst.
#[derive(Debug, Default)]
pub(crate) struct Drift {
    since: Point,
}

impl Drift {
    /// Well under how far the cursor may wander before it is put back.
    pub(crate) const CHECK: f64 = 50.0;

    /// Adds a movement; whether to read where the cursor really is now.
    pub(crate) fn moved(&mut self, delta: Point) -> bool {
        self.since = self.since + delta;
        let due = self.since.x.abs() > Self::CHECK || self.since.y.abs() > Self::CHECK;
        if due {
            self.since = Point::new(0.0, 0.0);
        }
        due
    }

    pub(crate) fn reset(&mut self) {
        self.since = Point::new(0.0, 0.0);
    }
}

/// The root window's size from a RandR screen change: the event gives it
/// before rotation, as Xlib's `XRRUpdateConfiguration` turns it back.
pub(crate) fn root_size(width: u16, height: u16, quarter_turned: bool) -> Point {
    let (w, h) = if quarter_turned {
        (height, width)
    } else {
        (width, height)
    };
    Point::new(w.into(), h.into())
}

/// Wheel movement for injection, which X11 knows only as whole clicks of
/// buttons 4–7: fractions add up across calls, so precise scrolling
/// arrives in full.
#[derive(Debug, Default)]
pub(crate) struct WheelClicks {
    /// Notches below one click, horizontal and vertical.
    remainder: (f64, f64),
}

impl WheelClicks {
    /// The buttons to click, once each, for `dx`/`dy` notches.
    pub(crate) fn clicks(&mut self, dx: f64, dy: f64) -> Vec<u8> {
        let mut out = Vec::new();
        for (notches, remainder, positive, negative) in [
            (dy, &mut self.remainder.1, WHEEL_UP, WHEEL_DOWN),
            (dx, &mut self.remainder.0, WHEEL_RIGHT, WHEEL_LEFT),
        ] {
            if !notches.is_finite() {
                continue;
            }
            let total = notches + *remainder;
            let whole = total.trunc();
            *remainder = total - whole;
            // A wheel turning faster than this is no wheel.
            let count = whole.abs().min(1000.0) as usize;
            let button = if whole > 0.0 { positive } else { negative };
            out.extend(std::iter::repeat_n(button, count));
        }
        out
    }
}

/// Relative movements in whole pixels, which XTest takes: fractions add
/// up across calls.
#[derive(Debug, Default)]
pub(crate) struct RelativeMover {
    remainder: (f64, f64),
}

impl RelativeMover {
    /// The move by (`dx`, `dy`) pixels to send now; `None` while less than
    /// a pixel has added up.
    pub(crate) fn move_by(&mut self, dx: f64, dy: f64) -> Option<(i16, i16)> {
        if !dx.is_finite() || !dy.is_finite() {
            return None;
        }
        let x = dx + self.remainder.0;
        let y = dy + self.remainder.1;
        let (whole_x, whole_y) = (x.trunc(), y.trunc());
        self.remainder = (x - whole_x, y - whole_y);
        // The request carries 16 bits; no mouse moves further at once.
        let pixels = |v: f64| v.clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16;
        (whole_x != 0.0 || whole_y != 0.0).then(|| (pixels(whole_x), pixels(whole_y)))
    }
}

/// An X `FP3232` fixed-point number.
pub(crate) fn fixed(integral: i32, frac: u32) -> f64 {
    f64::from(integral) + f64::from(frac) / 4_294_967_296.0
}

/// The values of a raw event by axis number: the mask's set bits say
/// which axes the values, in order, belong to.
pub(crate) fn valuators(mask: &[u32], values: &[f64]) -> Vec<(u16, f64)> {
    let mut values = values.iter();
    let mut out = Vec::new();
    for (word, bits) in mask.iter().enumerate() {
        for bit in 0..32 {
            if bits & (1 << bit) == 0 {
                continue;
            }
            let Some(&value) = values.next() else {
                return out;
            };
            if let Ok(axis) = u16::try_from(word * 32 + bit) {
                out.push((axis, value));
            }
        }
    }
    out
}

/// An axis of a device that reports positions (tablets, touch screens,
/// the pointers of virtual machines), which the server maps onto the
/// whole root window. Its movement comes from its own values, scaled to
/// the screen, so it goes on where the cursor stops at an edge, and it
/// never mixes with where the cursor went for any other reason (a warp,
/// an injection).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct AbsoluteAxis {
    pub min: f64,
    pub max: f64,
    /// Its last value, if it reported one since it was last reset.
    pub last: Option<f64>,
}

impl AbsoluteAxis {
    pub(crate) fn new(min: f64, max: f64) -> Self {
        Self {
            min,
            max,
            last: None,
        }
    }

    /// How far, in pixels of a screen `span` long on this axis, a new
    /// `value` moved from the last one; nothing for the first after a
    /// reset, which would be a jump from wherever the device last was.
    pub(crate) fn delta(&mut self, value: f64, span: f64) -> f64 {
        let range = self.max - self.min;
        let last = self.last.replace(value);
        match last {
            Some(last) if range > 0.0 && value.is_finite() => (value - last) / range * span,
            _ => 0.0,
        }
    }
}

/// A scrolling axis of a pointer device (XI 2.1 smooth scrolling).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct ScrollAxis {
    pub number: u16,
    pub vertical: bool,
    /// The movement of the axis that one wheel click stands for. Its sign
    /// is the driver's: the evdev driver turns natural scrolling on by
    /// making it negative.
    pub increment: f64,
}

/// What capture needs to know of a pointer device to read its raw events.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Axes {
    /// Its first two axes report a position, as tablets, touch screens and
    /// the pointers of virtual machines do, rather than a movement.
    pub absolute: bool,
    pub scroll: Vec<ScrollAxis>,
    /// libinput's natural scrolling is on for it, which reverses the
    /// scrolling axes' values.
    pub natural: bool,
}

impl Axes {
    /// The movement raw `values` report, for a device whose axes report
    /// movements. The values are the ones after the server's processing,
    /// as the cursor moves by them, and never stop at the edge of the
    /// screen, unlike the cursor.
    pub(crate) fn motion(&self, values: &[(u16, f64)]) -> Option<Point> {
        if self.absolute {
            return None;
        }
        let axis = |n| values.iter().find(|(a, _)| *a == n).map(|(_, v)| *v);
        let (x, y) = (axis(0), axis(1));
        (x.is_some() || y.is_some()).then(|| Point::new(x.unwrap_or(0.0), y.unwrap_or(0.0)))
    }

    /// The notches raw `values` scroll by, in the direction of
    /// `InputEvent::Scroll` (positive dy away from the user, positive dx
    /// to the right), with natural scrolling undone: a positive movement
    /// of an axis is down or right. `None` when no scrolling axis moved.
    pub(crate) fn scroll(&self, values: &[(u16, f64)]) -> Option<(f64, f64)> {
        let mut out = None;
        for axis in &self.scroll {
            let Some((_, value)) = values.iter().find(|(a, _)| *a == axis.number) else {
                continue;
            };
            if axis.increment == 0.0 || !value.is_finite() {
                continue;
            }
            let mut notches = value / axis.increment.abs();
            if self.natural {
                notches = -notches;
            }
            let (dx, dy) = out.get_or_insert((0.0, 0.0));
            if axis.vertical {
                *dy -= notches;
            } else {
                *dx += notches;
            }
        }
        out
    }

    /// Whether the server makes up clicks of wheel button `detail` from
    /// this device's scrolling axes, which `scroll` already counted.
    pub(crate) fn scrolls_smoothly(&self, detail: u32) -> bool {
        let vertical = match u8::try_from(detail) {
            Ok(WHEEL_UP | WHEEL_DOWN) => true,
            Ok(WHEEL_LEFT | WHEEL_RIGHT) => false,
            _ => return false,
        };
        self.scroll.iter().any(|a| a.vertical == vertical)
    }
}

/// X modifier masks.
pub(crate) const SHIFT_MASK: u16 = 1;
pub(crate) const LOCK_MASK: u16 = 1 << 1;
pub(crate) const CONTROL_MASK: u16 = 1 << 2;

/// Which of X's modifiers Mod1–Mod5 the keys InputWeave's hotkeys use
/// are on, as the server's modifier mapping says. Shift and Control
/// have masks of their own.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ModifierMasks {
    pub alt: u16,
    pub gui: u16,
    pub num_lock: u16,
}

impl ModifierMasks {
    /// From the key codes of `GetModifierMapping`: eight rows (Shift,
    /// Lock, Control, Mod1–Mod5) of the same length.
    pub(crate) fn from_mapping(keycodes: &[u8]) -> Self {
        let mut masks = Self::default();
        let per = keycodes.len() / 8;
        if per == 0 {
            return masks;
        }
        for (row, codes) in keycodes.chunks(per).enumerate().skip(3).take(5) {
            let mask = 1u16 << row;
            for key in codes.iter().filter_map(|&c| key_of(c.into())) {
                let slot = match key {
                    k if k == usage::LEFT_ALT || k == usage::RIGHT_ALT => &mut masks.alt,
                    k if k == usage::LEFT_GUI || k == usage::RIGHT_GUI => &mut masks.gui,
                    k if k == usage::NUM_LOCK => &mut masks.num_lock,
                    _ => continue,
                };
                // The first row a key is on; another key may share it.
                if *slot == 0 {
                    *slot = mask;
                }
            }
        }
        masks
    }

    fn mask(&self, modifier: Modifier) -> u16 {
        match modifier {
            Modifier::Control => CONTROL_MASK,
            Modifier::Shift => SHIFT_MASK,
            Modifier::Alt => self.alt,
            Modifier::Gui => self.gui,
        }
    }

    /// Every modifier state a press of a hotkey with `modifiers` may come
    /// with: its own modifiers, any of the others (the router takes a
    /// hotkey with more modifiers down too), and Caps Lock and Num Lock on
    /// or off. A passive grab matches one state exactly, so each needs one.
    /// `None` when one of `modifiers` is on no X modifier here.
    pub(crate) fn grab_states(&self, modifiers: Modifiers) -> Option<Vec<u16>> {
        let mut base = 0;
        for m in modifiers.iter() {
            let mask = self.mask(m);
            if mask == 0 {
                return None;
            }
            base |= mask;
        }
        let others: Vec<u16> = Modifier::ALL
            .into_iter()
            .filter(|m| !modifiers.contains(*m))
            .map(|m| self.mask(m))
            .filter(|&mask| mask != 0 && mask & base == 0)
            .collect();
        let locks = [0, LOCK_MASK, self.num_lock, LOCK_MASK | self.num_lock];
        let mut states = Vec::new();
        for subset in 0..1u32 << others.len() {
            let extra = others
                .iter()
                .enumerate()
                .filter(|(i, _)| subset & (1 << i) != 0)
                .fold(0, |acc, (_, m)| acc | m);
            for lock in locks {
                let state = base | extra | lock;
                if !states.contains(&state) {
                    states.push(state);
                }
            }
        }
        Some(states)
    }
}

/// Where grabbed capture holds the hidden cursor: the middle of the
/// display it is on, or of the primary one when it is on none.
pub(crate) fn hold_point(displays: &[Display], at: Point) -> Option<Point> {
    displays
        .iter()
        .find(|d| d.bounds.contains(at))
        .or_else(|| displays.iter().find(|d| d.primary))
        .or_else(|| displays.first())
        .map(|d| {
            let c = d.bounds.center();
            Point::new(c.x.floor(), c.y.floor())
        })
}

/// Whether a device is one of the server's XTest devices, through which
/// every injection by XTest, this process's own included, arrives.
pub(crate) fn is_xtest_device(name: &[u8]) -> bool {
    name.ends_with(b" XTEST pointer") || name.ends_with(b" XTEST keyboard")
}

/// A monitor as RandR reports it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Monitor {
    pub name: String,
    pub primary: bool,
    pub x: i16,
    pub y: i16,
    pub width: u16,
    pub height: u16,
}

/// The displays of the monitors, in root window coordinates, which are
/// the logical units too: X11 has no scale of its own for each monitor.
/// Without monitors, as on a server without RandR 1.5, the whole screen
/// is one display. One display is always primary.
pub(crate) fn displays_of(monitors: Vec<Monitor>, screen: (u16, u16)) -> Vec<Display> {
    let mut displays: Vec<Display> = monitors
        .into_iter()
        .filter(|m| m.width > 0 && m.height > 0)
        .map(|m| Display {
            id: m.name,
            bounds: Rect::new(
                f64::from(m.x),
                f64::from(m.y),
                f64::from(m.width),
                f64::from(m.height),
            ),
            scale: 1.0,
            primary: m.primary,
        })
        .collect();
    if displays.is_empty() && screen.0 > 0 && screen.1 > 0 {
        displays.push(Display {
            id: "screen".to_owned(),
            bounds: Rect::new(0.0, 0.0, f64::from(screen.0), f64::from(screen.1)),
            scale: 1.0,
            primary: true,
        });
    }
    // Several may say so after a misconfiguration; the first one is it.
    let mut primary_seen = false;
    for d in &mut displays {
        if d.primary && std::mem::replace(&mut primary_seen, true) {
            d.primary = false;
        }
    }
    if !primary_seen {
        let origin = displays
            .iter()
            .position(|d| d.bounds.contains(Point::new(0.0, 0.0)))
            .unwrap_or(0);
        if let Some(d) = displays.get_mut(origin) {
            d.primary = true;
        }
    }
    displays
}

#[cfg(test)]
mod tests {
    use std::time::Instant;

    use super::*;

    #[test]
    fn keys_are_evdev_codes_plus_eight() {
        // A is KEY_A (30), X's 38; Left Control KEY_LEFTCTRL (29), 37.
        assert_eq!(keycode(usage::A, 8, 255), Some(38));
        assert_eq!(keycode(usage::LEFT_CONTROL, 8, 255), Some(37));
        assert_eq!(keycode(usage::A, 40, 255), None);
        assert_eq!(key_of(38), Some(usage::A));
        assert_eq!(key_of(133), Some(usage::LEFT_GUI));
        assert_eq!(key_of(3), None);
        assert_eq!(key_of(70_000), None);
    }

    #[test]
    fn buttons_round_trip() {
        for b in [
            Button::Left,
            Button::Middle,
            Button::Right,
            Button::Back,
            Button::Forward,
        ] {
            assert_eq!(button_of(x_button(b).into()), Some(b));
        }
        assert_eq!(button_of(4), None);
        assert_eq!(wheel_notches(4), Some((0.0, 1.0)));
        assert_eq!(wheel_notches(7), Some((1.0, 0.0)));
        assert_eq!(wheel_notches(1), None);
    }

    #[test]
    fn wheel_fractions_add_up() {
        let mut w = WheelClicks::default();
        assert_eq!(w.clicks(0.0, 2.0), vec![WHEEL_UP, WHEEL_UP]);
        assert_eq!(w.clicks(0.0, -1.0), vec![WHEEL_DOWN]);
        assert!(w.clicks(0.4, 0.4).is_empty());
        assert_eq!(w.clicks(0.7, 0.7), vec![WHEEL_UP, WHEEL_RIGHT]);
        // The tenth left over to the right counts against going left.
        assert!(w.clicks(-1.0, f64::NAN).is_empty());
        assert_eq!(w.clicks(-0.5, 0.0), vec![WHEEL_LEFT]);
    }

    #[test]
    fn a_key_down_already_is_not_pressed_again() {
        let mut held = Held::default();
        assert!(held.sends(38, true));
        // Not sent: not down either.
        assert!(held.sends(38, true));
        held.sent(38, true);
        // The controlling device's repeat: the server repeats by itself.
        assert!(!held.sends(38, true));
        held.sent(39, true);
        assert!(held.sends(38, false));
        held.sent(38, false);
        // A release goes whether or not the key was down.
        assert!(held.sends(38, false));
        assert!(!held.is_empty());
        assert_eq!(held.take(), vec![39]);
        assert!(held.is_empty());
    }

    #[test]
    fn the_last_repeating_key_repeats_after_the_delay() {
        let settings = RepeatSettings::default();
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut r = KeyRepeat::default();
        assert_eq!(r.deadline(), None);
        r.press(38, usage::A, &settings, t0);
        assert_eq!(r.deadline(), Some(ms(660)));
        assert_eq!(r.due(ms(659), &settings), None);
        assert_eq!(r.due(ms(660), &settings), Some(usage::A));
        assert_eq!(r.due(ms(699), &settings), None);
        assert_eq!(r.due(ms(700), &settings), Some(usage::A));
        // A modifier pressed meanwhile does not stop it.
        r.press(50, usage::LEFT_SHIFT, &settings, ms(710));
        assert_eq!(r.due(ms(740), &settings), Some(usage::A));
        // Another key takes over, from its own delay.
        let b = HidUsage::from_evdev(48).unwrap();
        r.press(56, b, &settings, ms(750));
        assert_eq!(r.due(ms(780), &settings), None);
        assert_eq!(r.due(ms(1410), &settings), Some(b));
        // Letting go of the first key changes nothing; of the second, ends.
        r.release(38);
        assert_eq!(r.due(ms(1450), &settings), Some(b));
        r.release(56);
        assert_eq!(r.due(ms(5000), &settings), None);
        assert_eq!(r.deadline(), None);
    }

    #[test]
    fn repeats_do_not_make_up_for_a_stall() {
        let settings = RepeatSettings::default();
        let t0 = Instant::now();
        let mut r = KeyRepeat::default();
        r.press(38, usage::A, &settings, t0);
        let late = t0 + Duration::from_secs(5);
        assert_eq!(r.due(late, &settings), Some(usage::A));
        assert_eq!(r.due(late, &settings), None);
        assert_eq!(r.deadline(), Some(late + settings.interval));
    }

    #[test]
    fn keys_the_server_does_not_repeat_do_not_repeat() {
        let t0 = Instant::now();
        let mut no_a = RepeatSettings::default();
        no_a.per_key[38 / 8] &= !(1 << (38 % 8));
        let mut r = KeyRepeat::default();
        r.press(38, usage::A, &no_a, t0);
        assert_eq!(r.deadline(), None);
        r.press(66, usage::CAPS_LOCK, &RepeatSettings::default(), t0);
        assert_eq!(r.deadline(), None);
        let off = RepeatSettings {
            on: false,
            ..RepeatSettings::default()
        };
        r.press(38, usage::A, &off, t0);
        assert_eq!(r.deadline(), None);
    }

    fn lock() -> Hotkey {
        Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::L)
    }

    #[test]
    fn what_went_out_down_is_let_go_of() {
        let mut r = Reports::default();
        assert_eq!(r.key(usage::A, true, false, &[]), vec![(usage::A, true)]);
        assert!(r.button(Button::Left, true, false));
        assert_eq!(r.release_all(), (vec![usage::A], vec![Button::Left]));
        assert_eq!(r.release_all(), (vec![], vec![]));
    }

    #[test]
    fn while_waiting_for_a_grab_only_releases_and_hotkeys_go_out() {
        let mut r = Reports::default();
        // Down before the wait, released during it.
        r.key(usage::A, true, false, &[]);
        assert!(r.button(Button::Left, true, false));
        assert!(r.key(usage::C, true, true, &[lock()]).is_empty());
        assert!(!r.button(Button::Right, true, true));
        assert_eq!(
            r.key(usage::A, false, true, &[lock()]),
            vec![(usage::A, false)]
        );
        assert!(r.key(usage::C, false, true, &[lock()]).is_empty());
        assert!(r.button(Button::Left, false, true));
        assert!(!r.button(Button::Right, false, true));
        // The modifiers alone go nowhere; the hotkey goes out with them.
        for m in [usage::LEFT_CONTROL, usage::LEFT_ALT, usage::LEFT_SHIFT] {
            assert!(r.key(m, true, true, &[lock()]).is_empty());
        }
        assert_eq!(
            r.key(usage::L, true, true, &[lock()]),
            vec![
                (usage::LEFT_CONTROL, true),
                (usage::LEFT_ALT, true),
                (usage::LEFT_SHIFT, true),
                (usage::L, true),
            ]
        );
        // And their releases, reported down now.
        assert_eq!(
            r.key(usage::L, false, true, &[lock()]),
            vec![(usage::L, false)]
        );
        assert_eq!(
            r.key(usage::LEFT_SHIFT, false, true, &[lock()]),
            vec![(usage::LEFT_SHIFT, false)]
        );
        // A key that is no hotkey, with the same modifiers, does not.
        assert!(r.key(usage::A, true, true, &[lock()]).is_empty());
    }

    #[test]
    fn the_cursor_is_looked_for_every_so_often() {
        let mut d = Drift::default();
        assert!(!d.moved(Point::new(30.0, 0.0)));
        assert!(!d.moved(Point::new(20.0, -40.0)));
        assert!(d.moved(Point::new(1.0, 0.0)));
        assert!(!d.moved(Point::new(-49.0, 0.0)));
        d.reset();
        assert!(!d.moved(Point::new(0.0, 50.0)));
    }

    #[test]
    fn a_quarter_turned_screen_is_turned_back() {
        assert_eq!(root_size(1080, 1920, true), Point::new(1920.0, 1080.0));
        assert_eq!(root_size(1920, 1080, false), Point::new(1920.0, 1080.0));
    }

    #[test]
    fn relative_moves_carry_fractions() {
        let mut m = RelativeMover::default();
        assert_eq!(m.move_by(0.5, -0.5), None);
        assert_eq!(m.move_by(0.5, -0.75), Some((1, -1)));
        assert_eq!(m.move_by(1e9, 0.0), Some((i16::MAX, 0)));
        assert_eq!(m.move_by(f64::INFINITY, 0.0), None);
    }

    #[test]
    fn raw_values_follow_the_mask() {
        assert_eq!(fixed(-2, 1 << 31), -1.5);
        // Axes 0, 1 and 33 set.
        let values = valuators(&[0b11, 0b10], &[1.0, 2.0, 3.0]);
        assert_eq!(values, vec![(0, 1.0), (1, 2.0), (33, 3.0)]);
        // Fewer values than the mask says: what there is.
        assert_eq!(valuators(&[0b111], &[1.0]), vec![(0, 1.0)]);
    }

    fn wheel_mouse() -> Axes {
        Axes {
            absolute: false,
            scroll: vec![
                ScrollAxis {
                    number: 2,
                    vertical: false,
                    increment: 15.0,
                },
                ScrollAxis {
                    number: 3,
                    vertical: true,
                    increment: 15.0,
                },
            ],
            natural: false,
        }
    }

    #[test]
    fn motion_and_scrolling_from_raw_values() {
        let mouse = wheel_mouse();
        assert_eq!(
            mouse.motion(&[(0, 3.5), (1, -2.0)]),
            Some(Point::new(3.5, -2.0))
        );
        assert_eq!(mouse.motion(&[(1, 4.0)]), Some(Point::new(0.0, 4.0)));
        assert_eq!(mouse.motion(&[(3, 15.0)]), None);
        // Down a click: towards the user.
        assert_eq!(mouse.scroll(&[(3, 15.0)]), Some((0.0, -1.0)));
        assert_eq!(mouse.scroll(&[(2, -7.5)]), Some((-0.5, 0.0)));
        assert_eq!(mouse.scroll(&[(0, 1.0)]), None);
        let natural = Axes {
            natural: true,
            ..mouse.clone()
        };
        assert_eq!(natural.scroll(&[(3, 15.0)]), Some((0.0, 1.0)));
        // The evdev driver's natural scrolling: values as they were.
        let mut evdev = mouse.clone();
        evdev.scroll[1].increment = -15.0;
        assert_eq!(evdev.scroll(&[(3, 15.0)]), Some((0.0, -1.0)));
        let tablet = Axes {
            absolute: true,
            ..Axes::default()
        };
        assert_eq!(tablet.motion(&[(0, 100.0), (1, 100.0)]), None);
    }

    #[test]
    fn positions_move_by_their_own_values_scaled_to_the_screen() {
        let mut x = AbsoluteAxis::new(0.0, 32767.0);
        // The first value after a reset moves nothing.
        assert_eq!(x.delta(16383.5, 3840.0), 0.0);
        let d = x.delta(16383.5 + 32767.0 / 3840.0 * 10.0, 3840.0);
        assert!((d - 10.0).abs() < 1e-9, "{d}");
        x.last = None;
        assert_eq!(x.delta(0.0, 3840.0), 0.0);
        let mut flat = AbsoluteAxis::new(5.0, 5.0);
        flat.delta(5.0, 100.0);
        assert_eq!(flat.delta(5.0, 100.0), 0.0);
    }

    #[test]
    fn wheel_clicks_made_up_from_smooth_scrolling_are_not_counted_twice() {
        let mouse = wheel_mouse();
        assert!(mouse.scrolls_smoothly(4));
        assert!(mouse.scrolls_smoothly(7));
        assert!(!mouse.scrolls_smoothly(1));
        let old = Axes::default();
        assert!(!old.scrolls_smoothly(5));
    }

    /// A US keyboard's mapping under the evdev rules, two keys a row.
    fn mapping() -> Vec<u8> {
        vec![
            50, 62, // Shift
            66, 0, // Lock: Caps Lock
            37, 105, // Control
            64, 108, // Mod1: Alt
            77, 0, // Mod2: Num Lock
            0, 0, // Mod3
            133, 134, // Mod4: Super
            92, 0, // Mod5: ISO_Level3_Shift
        ]
    }

    #[test]
    fn modifier_masks_come_from_the_mapping() {
        let masks = ModifierMasks::from_mapping(&mapping());
        assert_eq!(
            masks,
            ModifierMasks {
                alt: 1 << 3,
                gui: 1 << 6,
                num_lock: 1 << 4,
            }
        );
        assert_eq!(ModifierMasks::from_mapping(&[]), ModifierMasks::default());
    }

    #[test]
    fn a_hotkey_is_grabbed_in_every_state_it_fires_in() {
        let masks = ModifierMasks::from_mapping(&mapping());
        let lock = Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::L);
        let states = masks.grab_states(lock.modifiers).unwrap();
        let base = CONTROL_MASK | SHIFT_MASK | (1 << 3);
        // With or without Super, Caps Lock and Num Lock.
        assert_eq!(states.len(), 2 * 4);
        for extra in [0, 1 << 6] {
            for l in [0, LOCK_MASK, 1 << 4, LOCK_MASK | (1 << 4)] {
                assert!(
                    states.contains(&(base | extra | l)),
                    "{:#x}",
                    base | extra | l
                );
            }
        }
        // Shift alone: any of the other three as well.
        let shift = Modifiers::NONE.with(Modifier::Shift);
        assert_eq!(masks.grab_states(shift).unwrap().len(), 8 * 4);
        // No Super on this server: such a hotkey cannot be grabbed.
        let no_super = ModifierMasks { gui: 0, ..masks };
        assert_eq!(
            no_super.grab_states(Modifiers::NONE.with(Modifier::Gui)),
            None
        );
        // Nor does a missing Num Lock add states that are not there.
        let no_num = ModifierMasks {
            num_lock: 0,
            ..masks
        };
        assert_eq!(no_num.grab_states(lock.modifiers).unwrap().len(), 2 * 2);
    }

    fn display(id: &str, x: f64, w: f64, primary: bool) -> Display {
        Display {
            id: id.to_owned(),
            bounds: Rect::new(x, 0.0, w, 1080.0),
            scale: 1.0,
            primary,
        }
    }

    #[test]
    fn the_cursor_is_held_in_the_middle_of_its_display() {
        let displays = [
            display("A", 0.0, 1920.0, false),
            display("B", 1920.0, 1281.0, true),
        ];
        assert_eq!(
            hold_point(&displays, Point::new(10.0, 10.0)),
            Some(Point::new(960.0, 540.0))
        );
        assert_eq!(
            hold_point(&displays, Point::new(2000.0, 10.0)),
            Some(Point::new(2560.0, 540.0))
        );
        // Off every display: the primary one.
        assert_eq!(
            hold_point(&displays, Point::new(-5.0, -5.0)),
            Some(Point::new(2560.0, 540.0))
        );
        assert_eq!(hold_point(&[], Point::new(0.0, 0.0)), None);
    }

    #[test]
    fn xtest_devices_are_told_by_name() {
        assert!(is_xtest_device(b"Virtual core XTEST pointer"));
        assert!(is_xtest_device(b"Virtual core XTEST keyboard"));
        assert!(!is_xtest_device(b"Logitech USB Receiver"));
        assert!(!is_xtest_device(b"Virtual core pointer"));
    }

    fn monitor(name: &str, x: i16, primary: bool) -> Monitor {
        Monitor {
            name: name.to_owned(),
            primary,
            x,
            y: 0,
            width: 1920,
            height: 1080,
        }
    }

    #[test]
    fn displays_from_monitors() {
        let displays = displays_of(
            vec![monitor("DP-1", 0, false), monitor("HDMI-1", 1920, true)],
            (3840, 1080),
        );
        assert_eq!(displays.len(), 2);
        assert_eq!(displays[0].id, "DP-1");
        assert_eq!(displays[1].bounds, Rect::new(1920.0, 0.0, 1920.0, 1080.0));
        assert!(!displays[0].primary && displays[1].primary);
        // None primary: the one at the origin.
        let displays = displays_of(
            vec![monitor("B", 1920, false), monitor("A", 0, false)],
            (3840, 1080),
        );
        assert!(!displays[0].primary && displays[1].primary);
        // Two primary: the first.
        let displays = displays_of(
            vec![monitor("A", 0, true), monitor("B", 1920, true)],
            (3840, 1080),
        );
        assert!(displays[0].primary && !displays[1].primary);
        // No monitors: the screen.
        let displays = displays_of(Vec::new(), (1024, 768));
        assert_eq!(displays.len(), 1);
        assert_eq!(displays[0].bounds, Rect::new(0.0, 0.0, 1024.0, 768.0));
        assert!(displays[0].primary);
        assert!(displays_of(Vec::new(), (0, 0)).is_empty());
    }
}
