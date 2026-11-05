//! Translation between Quartz event fields and InputWeave's input events,
//! kept free of FFI so it can be tested without any permission.

use std::time::{Duration, Instant};

use iw_core::HidUsage;
use iw_core::geometry::{Point, Rect};
use iw_core::input::{Button, InputEvent};
use iw_core::router::Hotkey;
use iw_keymap::usage;

/// `CGEventType` values this backend handles.
pub(crate) mod kind {
    pub const LEFT_DOWN: u32 = 1;
    pub const LEFT_UP: u32 = 2;
    pub const RIGHT_DOWN: u32 = 3;
    pub const RIGHT_UP: u32 = 4;
    pub const MOVED: u32 = 5;
    pub const LEFT_DRAGGED: u32 = 6;
    pub const RIGHT_DRAGGED: u32 = 7;
    pub const KEY_DOWN: u32 = 10;
    pub const KEY_UP: u32 = 11;
    pub const FLAGS_CHANGED: u32 = 12;
    pub const SCROLL: u32 = 22;
    pub const OTHER_DOWN: u32 = 25;
    pub const OTHER_UP: u32 = 26;
    pub const OTHER_DRAGGED: u32 = 27;

    /// Whether events of kind `k` move the pointer.
    pub fn is_motion(k: u32) -> bool {
        matches!(k, MOVED | LEFT_DRAGGED | RIGHT_DRAGGED | OTHER_DRAGGED)
    }
}

/// Device-dependent modifier bits (`NX_DEVICE*KEYMASK`) and the generic
/// ones (`kCGEventFlagMask*`) in `CGEventFlags`.
mod flag {
    pub const LEFT_CONTROL: u64 = 0x0000_0001;
    pub const LEFT_SHIFT: u64 = 0x0000_0002;
    pub const RIGHT_SHIFT: u64 = 0x0000_0004;
    pub const LEFT_COMMAND: u64 = 0x0000_0008;
    pub const RIGHT_COMMAND: u64 = 0x0000_0010;
    pub const LEFT_OPTION: u64 = 0x0000_0020;
    pub const RIGHT_OPTION: u64 = 0x0000_0040;
    pub const RIGHT_CONTROL: u64 = 0x0000_2000;
    pub const DEVICE: u64 = LEFT_CONTROL
        | LEFT_SHIFT
        | RIGHT_SHIFT
        | LEFT_COMMAND
        | RIGHT_COMMAND
        | LEFT_OPTION
        | RIGHT_OPTION
        | RIGHT_CONTROL;
    pub const CAPS_LOCK: u64 = 0x0001_0000;
    pub const SHIFT: u64 = 0x0002_0000;
    pub const CONTROL: u64 = 0x0004_0000;
    pub const OPTION: u64 = 0x0008_0000;
    pub const COMMAND: u64 = 0x0010_0000;
}

/// For each modifier key: its HID usage, its device bit and its generic bit.
const MODIFIERS: [(HidUsage, u64, u64); 8] = [
    (usage::LEFT_CONTROL, flag::LEFT_CONTROL, flag::CONTROL),
    (usage::LEFT_SHIFT, flag::LEFT_SHIFT, flag::SHIFT),
    (usage::LEFT_ALT, flag::LEFT_OPTION, flag::OPTION),
    (usage::LEFT_GUI, flag::LEFT_COMMAND, flag::COMMAND),
    (usage::RIGHT_CONTROL, flag::RIGHT_CONTROL, flag::CONTROL),
    (usage::RIGHT_SHIFT, flag::RIGHT_SHIFT, flag::SHIFT),
    (usage::RIGHT_ALT, flag::RIGHT_OPTION, flag::OPTION),
    (usage::RIGHT_GUI, flag::RIGHT_COMMAND, flag::COMMAND),
];

/// Pixels of continuous (trackpad) scrolling counted as one wheel notch,
/// matching the default `CGEventSourceGetPixelsPerLine`.
pub(crate) const PIXELS_PER_NOTCH: f64 = 10.0;

/// The fields of a Quartz event that capture looks at.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct RawEvent {
    pub kind: u32,
    pub location: Point,
    pub delta: Point,
    /// `kCGMouseEventButtonNumber`: 0 left, 1 right, 2 middle, 3 back, 4 forward.
    pub button_number: i64,
    pub keycode: u16,
    pub autorepeat: bool,
    pub flags: u64,
    pub scroll_continuous: bool,
    /// Vertical and horizontal scroll, in lines (`FixedPtDeltaAxis1/2`).
    pub scroll_lines: Point,
    /// The same in pixels (`PointDeltaAxis1/2`), for continuous scrolling.
    pub scroll_pixels: Point,
}

/// Turns captured Quartz events into input events, tracking which
/// modifiers are down so `flagsChanged` becomes key presses and releases.
#[derive(Debug, Default)]
pub(crate) struct Translator {
    held_modifiers: Vec<HidUsage>,
    /// Whether the user has "natural" scrolling on, which macOS has already
    /// applied to the deltas it reports.
    pub natural_scrolling: bool,
}

impl Translator {
    pub(crate) fn new(natural_scrolling: bool) -> Self {
        Self {
            natural_scrolling,
            ..Self::default()
        }
    }

    pub(crate) fn translate(&mut self, e: &RawEvent) -> Vec<InputEvent> {
        use kind::*;
        match e.kind {
            MOVED | LEFT_DRAGGED | RIGHT_DRAGGED | OTHER_DRAGGED => {
                vec![InputEvent::PointerMotion {
                    position: e.location,
                    delta: e.delta,
                }]
            }
            LEFT_DOWN | RIGHT_DOWN | OTHER_DOWN | LEFT_UP | RIGHT_UP | OTHER_UP => {
                let pressed = matches!(e.kind, LEFT_DOWN | RIGHT_DOWN | OTHER_DOWN);
                let button = match e.kind {
                    LEFT_DOWN | LEFT_UP => Some(Button::Left),
                    RIGHT_DOWN | RIGHT_UP => Some(Button::Right),
                    _ => button_from_number(e.button_number),
                };
                button
                    .map(|button| InputEvent::Button { button, pressed })
                    .into_iter()
                    .collect()
            }
            KEY_DOWN | KEY_UP => HidUsage::from_mac(e.keycode)
                .map(|usage| InputEvent::Key {
                    usage,
                    pressed: e.kind == KEY_DOWN,
                })
                .into_iter()
                .collect(),
            FLAGS_CHANGED => self.flags_changed(e),
            SCROLL => {
                let lines = if e.scroll_continuous {
                    Point::new(
                        e.scroll_pixels.x / PIXELS_PER_NOTCH,
                        e.scroll_pixels.y / PIXELS_PER_NOTCH,
                    )
                } else {
                    e.scroll_lines
                };
                let (dx, dy) = to_physical(lines, self.natural_scrolling);
                if dx == 0.0 && dy == 0.0 {
                    return Vec::new();
                }
                vec![InputEvent::Scroll { dx, dy }]
            }
            _ => Vec::new(),
        }
    }

    /// The modifier keys down as of the last event translated.
    pub(crate) fn held_modifiers(&self) -> &[HidUsage] {
        &self.held_modifiers
    }

    fn flags_changed(&mut self, e: &RawEvent) -> Vec<InputEvent> {
        let Some(key) = HidUsage::from_mac(e.keycode) else {
            return Vec::new();
        };
        if key == usage::CAPS_LOCK {
            // macOS reports Caps Lock as a toggled state, once per press.
            return vec![
                InputEvent::Key {
                    usage: key,
                    pressed: true,
                },
                InputEvent::Key {
                    usage: key,
                    pressed: false,
                },
            ];
        }
        let Some(&(_, device_bit, generic_bit)) = MODIFIERS.iter().find(|(u, ..)| *u == key) else {
            return Vec::new();
        };
        let was_down = self.held_modifiers.contains(&key);
        // The device bit says exactly which side is down; events without
        // device bits only have the generic one, so fall back to toggling.
        let down = if e.flags & flag::DEVICE != 0 || e.flags & generic_bit == 0 {
            e.flags & device_bit != 0
        } else {
            !was_down
        };
        if down == was_down {
            return Vec::new();
        }
        if down {
            self.held_modifiers.push(key);
        } else {
            self.held_modifiers.retain(|k| *k != key);
        }
        vec![InputEvent::Key {
            usage: key,
            pressed: down,
        }]
    }
}

/// Decides which key events of reserved hotkeys to keep from local apps
/// while input is not grabbed: the press that completes a hotkey, then that
/// key's auto-repeats and its release, so apps never see half a keystroke.
#[derive(Debug, Default)]
pub(crate) struct HeldBack {
    /// Keys whose press was held back and that are still down.
    down: Vec<HidUsage>,
}

impl HeldBack {
    /// Whether to hold back the event that translated to `events`, with
    /// `modifiers` down. `autorepeat` is the event's own flag: the repeat of
    /// a key that went down before the modifiers belongs to apps that saw
    /// that key go down, so it must not start holding the key back.
    pub(crate) fn holds(
        &mut self,
        hotkeys: &[Hotkey],
        events: &[InputEvent],
        modifiers: &[HidUsage],
        autorepeat: bool,
    ) -> bool {
        let [InputEvent::Key { usage, pressed }] = *events else {
            return false;
        };
        if self.down.contains(&usage) {
            if !pressed {
                self.down.retain(|k| *k != usage);
            }
            return true;
        }
        let triggers = pressed
            && !autorepeat
            && hotkeys
                .iter()
                .any(|h| h.matches(usage, |k| modifiers.contains(&k)));
        if triggers {
            self.down.push(usage);
        }
        triggers
    }
}

/// A warp of the cursor (`CGWarpMouseCursorPosition`) that no motion event
/// has followed yet. macOS measures a motion event's delta from where the
/// last event left the cursor, so the first motion after a warp also
/// carries the jump from `from` to `to`. Left in, the warp that brings the
/// cursor back to this machine at the seam reads as a fling back across it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct Warp {
    /// Where the last event left the cursor.
    pub from: Point,
    pub to: Point,
}

impl Warp {
    /// A further warp to `to` before any motion: the jump still starts
    /// where the first one did.
    pub(crate) fn then(self, to: Point) -> Self {
        Self {
            from: self.from,
            to,
        }
    }

    /// Takes the jump out of the motion event `e`, if it came after the
    /// warp: one that left the cursor nearer where the warp put it than
    /// where it was. Its motion is then measured from where the warp put
    /// the cursor, which holds whether or not macOS added the jump. Returns
    /// whether it did, and so whether the warp is done with; motion still
    /// on its way from before the warp is left as it is.
    pub(crate) fn discount(&self, e: &mut RawEvent) -> bool {
        if distance(e.location, self.to) > distance(e.location, self.from) {
            return false;
        }
        e.delta = e.location - self.to;
        true
    }
}

fn button_from_number(n: i64) -> Option<Button> {
    match n {
        0 => Some(Button::Left),
        1 => Some(Button::Right),
        2 => Some(Button::Middle),
        3 => Some(Button::Back),
        4 => Some(Button::Forward),
        _ => None,
    }
}

pub(crate) fn button_number(button: Button) -> i64 {
    match button {
        Button::Left => 0,
        Button::Right => 1,
        Button::Middle => 2,
        Button::Back => 3,
        Button::Forward => 4,
    }
}

/// Whether capture should pass an event on unreported, as this agent's
/// own: one it posted (`tagged`), or any while the virtual keyboard and
/// mouse inject (`device_input`), whose events look like hardware.
pub(crate) fn is_own(tagged: bool, device_input: bool) -> bool {
    tagged || device_input
}

/// Quartz scroll deltas (positive Axis1 scrolls content down, i.e. "up";
/// positive Axis2 scrolls "left") to wheel direction (positive dy turns the
/// wheel away from the user, positive dx is to the right), undoing natural
/// scrolling.
fn to_physical(lines: Point, natural: bool) -> (f64, f64) {
    let sign = if natural { -1.0 } else { 1.0 };
    (-lines.x * sign, lines.y * sign)
}

/// The inverse of [`to_physical`], for injection: Quartz pixel deltas for
/// (horizontal, vertical) wheel notches on a machine with `natural`
/// scrolling.
pub(crate) fn to_quartz_pixels(dx: f64, dy: f64, natural: bool) -> (f64, f64) {
    let sign = if natural { -1.0 } else { 1.0 };
    (-dx * sign * PIXELS_PER_NOTCH, dy * sign * PIXELS_PER_NOTCH)
}

/// One Quartz event to synthesize.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Synth {
    Mouse {
        kind: u32,
        location: Point,
        delta: Point,
        button_number: i64,
        click_state: i64,
        flags: u64,
    },
    Key {
        keycode: u16,
        down: bool,
        /// Modifier keys are posted as `flagsChanged`.
        flags_changed: bool,
        autorepeat: bool,
        flags: u64,
    },
    Scroll {
        pixels: (f64, f64),
        flags: u64,
    },
}

/// How close in time and space a press must follow the previous one to
/// count as a double (or triple) click.
#[derive(Clone, Copy, Debug)]
pub(crate) struct ClickPolicy {
    pub interval: Duration,
    pub distance: f64,
}

impl Default for ClickPolicy {
    fn default() -> Self {
        Self {
            interval: Duration::from_millis(500),
            distance: 4.0,
        }
    }
}

/// `to`, where a relative movement from `from` takes the cursor, kept on
/// `screens`: along whichever axis still is on one; `None` when neither is.
pub(crate) fn on_screens(screens: &[Rect], from: Point, to: Point) -> Option<Point> {
    [to, Point::new(to.x, from.y), Point::new(from.x, to.y)]
        .into_iter()
        .find(|&p| screens.iter().any(|s| s.contains(p)))
}

/// What injection needs to remember between calls: where the cursor is,
/// what is held, and recent clicks.
#[derive(Debug, Default)]
pub(crate) struct Injector {
    pub position: Point,
    held_buttons: Vec<Button>,
    held_keys: Vec<HidUsage>,
    last_click: Option<(Button, Instant, Point, i64)>,
    pub clicks: ClickPolicy,
    pub natural_scrolling: bool,
    /// Fractions of relative movements not yet sent (`move_by`).
    relative_rest: Point,
}

impl Injector {
    pub(crate) fn new(position: Point, clicks: ClickPolicy, natural_scrolling: bool) -> Self {
        Self {
            position,
            clicks,
            natural_scrolling,
            ..Self::default()
        }
    }

    fn flags(&self) -> u64 {
        let mut flags = 0;
        for &(key, device, generic) in &MODIFIERS {
            if self.held_keys.contains(&key) {
                flags |= device | generic;
            }
        }
        flags
    }

    pub(crate) fn move_to(&mut self, position: Point) -> Synth {
        let delta = position - self.position;
        self.motion(position, delta)
    }

    /// A relative movement by `delta` from `from`, where the cursor is: the
    /// event's delta fields carry it in whole points, which is what games
    /// holding the cursor read, and the rest waits for the next movement.
    /// The cursor goes where `fit` puts `from` plus the whole points, on a
    /// screen; the delta stays whole even where the cursor cannot follow,
    /// at an edge or held by the game.
    pub(crate) fn move_by(
        &mut self,
        delta: Point,
        from: Point,
        fit: impl Fn(Point) -> Point,
    ) -> Synth {
        let total = delta + self.relative_rest;
        let whole = Point::new(total.x.trunc(), total.y.trunc());
        self.relative_rest = total - whole;
        let location = fit(from + whole);
        self.motion(location, whole)
    }

    fn motion(&mut self, location: Point, delta: Point) -> Synth {
        self.position = location;
        // While a button is held, motion is a drag of that button.
        let (kind, button) = match self.held_buttons.first() {
            Some(Button::Left) => (kind::LEFT_DRAGGED, Button::Left),
            Some(Button::Right) => (kind::RIGHT_DRAGGED, Button::Right),
            Some(&other) => (kind::OTHER_DRAGGED, other),
            None => (kind::MOVED, Button::Left),
        };
        Synth::Mouse {
            kind,
            location,
            delta,
            button_number: button_number(button),
            click_state: 0,
            flags: self.flags(),
        }
    }

    pub(crate) fn button(&mut self, button: Button, pressed: bool, now: Instant) -> Synth {
        let kind = match (button, pressed) {
            (Button::Left, true) => kind::LEFT_DOWN,
            (Button::Left, false) => kind::LEFT_UP,
            (Button::Right, true) => kind::RIGHT_DOWN,
            (Button::Right, false) => kind::RIGHT_UP,
            (_, true) => kind::OTHER_DOWN,
            (_, false) => kind::OTHER_UP,
        };
        let click_state = if pressed {
            self.held_buttons.retain(|b| *b != button);
            self.held_buttons.push(button);
            let count = match self.last_click {
                Some((b, at, p, n))
                    if b == button
                        && now.saturating_duration_since(at) <= self.clicks.interval
                        && distance(p, self.position) <= self.clicks.distance =>
                {
                    n + 1
                }
                _ => 1,
            };
            self.last_click = Some((button, now, self.position, count));
            count
        } else {
            self.held_buttons.retain(|b| *b != button);
            // A release reports the count of the press it ends.
            match self.last_click {
                Some((b, _, _, n)) if b == button => n,
                _ => 1,
            }
        };
        Synth::Mouse {
            kind,
            location: self.position,
            delta: Point::default(),
            button_number: button_number(button),
            click_state,
            flags: self.flags(),
        }
    }

    /// `None` for keys macOS has no code for.
    pub(crate) fn key(&mut self, key: HidUsage, pressed: bool) -> Option<Synth> {
        let keycode = key.to_mac()?;
        let was_held = self.held_keys.contains(&key);
        if pressed && !was_held {
            self.held_keys.push(key);
        } else if !pressed {
            self.held_keys.retain(|k| *k != key);
        }
        let mut flags = self.flags();
        if key == usage::CAPS_LOCK {
            flags |= flag::CAPS_LOCK;
        }
        Some(Synth::Key {
            keycode,
            down: pressed,
            flags_changed: key.is_modifier() || key == usage::CAPS_LOCK,
            autorepeat: pressed && was_held && !key.is_modifier(),
            flags,
        })
    }

    pub(crate) fn scroll(&self, dx: f64, dy: f64) -> Synth {
        Synth::Scroll {
            pixels: to_quartz_pixels(dx, dy, self.natural_scrolling),
            flags: self.flags(),
        }
    }

    /// The keys held, modifiers first, and the buttons held.
    pub(crate) fn held(&self) -> (Vec<HidUsage>, Vec<Button>) {
        let (mut keys, others): (Vec<_>, Vec<_>) =
            self.held_keys.iter().partition(|k| k.is_modifier());
        keys.extend(others);
        (keys, self.held_buttons.clone())
    }

    /// The events that let go of every button and key held, which are then
    /// no longer held: injection moves on to another way of reaching the
    /// system, and nothing may stay down on this one.
    pub(crate) fn release_all(&mut self, now: Instant) -> Vec<Synth> {
        let mut synths = Vec::new();
        for button in self.held_buttons.clone() {
            synths.push(self.button(button, false, now));
        }
        // Modifiers last, so the other keys go up with them still applied.
        let (modifiers, keys): (Vec<_>, Vec<_>) = self
            .held_keys
            .clone()
            .into_iter()
            .partition(|k| k.is_modifier());
        for key in keys.into_iter().chain(modifiers) {
            synths.extend(self.key(key, false));
        }
        synths
    }
}

fn distance(a: Point, b: Point) -> f64 {
    let d = a - b;
    (d.x * d.x + d.y * d.y).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(kind: u32) -> RawEvent {
        RawEvent {
            kind,
            ..RawEvent::default()
        }
    }

    #[test]
    fn keys_buttons_and_motion() {
        let mut t = Translator::default();
        let key_a = RawEvent {
            keycode: 0x00,
            ..raw(kind::KEY_DOWN)
        };
        assert_eq!(
            t.translate(&key_a),
            vec![InputEvent::Key {
                usage: usage::A,
                pressed: true
            }]
        );
        let back = RawEvent {
            button_number: 3,
            ..raw(kind::OTHER_UP)
        };
        assert_eq!(
            t.translate(&back),
            vec![InputEvent::Button {
                button: Button::Back,
                pressed: false
            }]
        );
        let drag = RawEvent {
            location: Point::new(10.0, 20.0),
            delta: Point::new(-1.0, 2.0),
            ..raw(kind::LEFT_DRAGGED)
        };
        assert_eq!(
            t.translate(&drag),
            vec![InputEvent::PointerMotion {
                position: Point::new(10.0, 20.0),
                delta: Point::new(-1.0, 2.0),
            }]
        );
        // Unknown key codes and event kinds are dropped.
        let fn_key = RawEvent {
            keycode: 0x3f,
            ..raw(kind::KEY_DOWN)
        };
        assert!(t.translate(&fn_key).is_empty());
        assert!(t.translate(&raw(99)).is_empty());
    }

    #[test]
    fn reserved_hotkeys_are_held_back_whole() {
        let lock = [iw_core::hotkey::Hotkeys::default().lock];
        let mut t = Translator::default();
        let mut held = HeldBack::default();
        let mut feed = |e: RawEvent| {
            let events = t.translate(&e);
            assert!(!events.is_empty(), "{e:?}");
            held.holds(&lock, &events, t.held_modifiers(), e.autorepeat)
        };
        let modifier = |keycode, flags| RawEvent {
            keycode,
            flags,
            ..raw(kind::FLAGS_CHANGED)
        };
        let l = |kind| RawEvent {
            keycode: usage::L.to_mac().unwrap(),
            ..raw(kind)
        };
        let control = flag::CONTROL | flag::LEFT_CONTROL;
        let option = flag::OPTION | flag::LEFT_OPTION;
        let shift = flag::SHIFT | flag::LEFT_SHIFT;

        // A plain L reaches apps.
        assert!(!feed(l(kind::KEY_DOWN)));
        assert!(!feed(l(kind::KEY_UP)));

        // Modifiers reach apps; the L that completes the hotkey does not,
        // nor do its repeats and release, even once a modifier is let go.
        assert!(!feed(modifier(0x3b, control)));
        assert!(!feed(modifier(0x3a, control | option)));
        assert!(!feed(modifier(0x38, control | option | shift)));
        assert!(feed(l(kind::KEY_DOWN)));
        assert!(feed(RawEvent {
            autorepeat: true,
            ..l(kind::KEY_DOWN)
        }));
        assert!(!feed(modifier(0x38, control | option)));
        assert!(feed(l(kind::KEY_UP)));
        assert!(!feed(l(kind::KEY_DOWN)));
    }

    #[test]
    fn reserving_other_hotkeys_leaves_a_held_key_as_it_began() {
        use iw_core::hotkey::{Hotkey, Hotkeys, Modifiers};

        let lock = Hotkeys::default().lock;
        let k = HidUsage::from_name("KeyK").unwrap();
        let k_code = k.to_mac().unwrap();
        let other = [Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, k)];
        let mut t = Translator::default();
        let mut held = HeldBack::default();
        let mut feed = |e: RawEvent, reserved: &[Hotkey]| {
            let events = t.translate(&e);
            assert!(!events.is_empty(), "{e:?}");
            held.holds(reserved, &events, t.held_modifiers(), e.autorepeat)
        };
        let modifier = |keycode, flags| RawEvent {
            keycode,
            flags,
            ..raw(kind::FLAGS_CHANGED)
        };
        let key = |keycode: u16, kind| RawEvent {
            keycode,
            ..raw(kind)
        };
        let l = |kind| key(usage::L.to_mac().unwrap(), kind);
        let repeat = |e: RawEvent| RawEvent {
            autorepeat: true,
            ..e
        };
        let control = flag::CONTROL | flag::LEFT_CONTROL;
        let option = flag::OPTION | flag::LEFT_OPTION;
        let shift = flag::SHIFT | flag::LEFT_SHIFT;
        assert!(!feed(modifier(0x3b, control), &[lock]));
        assert!(!feed(modifier(0x3a, control | option), &[lock]));
        assert!(!feed(modifier(0x38, control | option | shift), &[lock]));

        // L completes the lock and is held back. Then the lock moves to K:
        // L stays held back until it is released, so apps see no stray
        // repeat or release; a fresh L reaches them.
        assert!(feed(l(kind::KEY_DOWN), &[lock]));
        assert!(feed(repeat(l(kind::KEY_DOWN)), &other));
        assert!(feed(l(kind::KEY_UP), &other));
        assert!(!feed(l(kind::KEY_DOWN), &other));
        assert!(!feed(l(kind::KEY_UP), &other));

        // K goes down while it is no hotkey, so apps see it; becoming the
        // lock's key while it is down does not take its repeats or release
        // from them.
        assert!(!feed(key(k_code, kind::KEY_DOWN), &[lock]));
        assert!(!feed(repeat(key(k_code, kind::KEY_DOWN)), &other));
        assert!(!feed(key(k_code, kind::KEY_UP), &other));
        // Pressed afresh, it is the hotkey.
        assert!(feed(key(k_code, kind::KEY_DOWN), &other));
        assert!(feed(key(k_code, kind::KEY_UP), &other));
    }

    #[test]
    fn a_key_held_before_the_hotkey_modifiers_reaches_apps_whole() {
        let lock = [iw_core::hotkey::Hotkeys::default().lock];
        let mut t = Translator::default();
        let mut held = HeldBack::default();
        let mut feed = |e: RawEvent| {
            let events = t.translate(&e);
            assert!(!events.is_empty(), "{e:?}");
            held.holds(&lock, &events, t.held_modifiers(), e.autorepeat)
        };
        let modifier = |keycode, flags| RawEvent {
            keycode,
            flags,
            ..raw(kind::FLAGS_CHANGED)
        };
        let l = |kind| RawEvent {
            keycode: usage::L.to_mac().unwrap(),
            ..raw(kind)
        };
        let repeat = RawEvent {
            autorepeat: true,
            ..l(kind::KEY_DOWN)
        };
        let control = flag::CONTROL | flag::LEFT_CONTROL;
        let option = flag::OPTION | flag::LEFT_OPTION;
        let shift = flag::SHIFT | flag::LEFT_SHIFT;

        // L goes down on its own, so apps see it and repeat it.
        assert!(!feed(l(kind::KEY_DOWN)));
        assert!(!feed(modifier(0x3b, control)));
        assert!(!feed(modifier(0x3a, control | option)));
        assert!(!feed(modifier(0x38, control | option | shift)));
        // Its repeats with the hotkey's modifiers down are not a hotkey
        // press, and apps get its release.
        assert!(!feed(repeat));
        assert!(!feed(repeat));
        assert!(!feed(l(kind::KEY_UP)));
        // A fresh press with the modifiers still down is the hotkey.
        assert!(feed(l(kind::KEY_DOWN)));
        assert!(feed(l(kind::KEY_UP)));
    }

    #[test]
    fn the_first_motion_after_a_warp_loses_the_jump() {
        // Control comes back at the left edge, while the hidden cursor had
        // wandered off to the right; macOS adds that way back to the delta.
        let warp = Warp {
            from: Point::new(1800.0, 900.0),
            to: Point::new(4.0, 300.0),
        };
        let mut moved = RawEvent {
            location: Point::new(9.0, 301.0),
            delta: Point::new(-1791.0, -599.0),
            ..raw(kind::MOVED)
        };
        assert!(warp.discount(&mut moved));
        assert_eq!(moved.delta, Point::new(5.0, 1.0));
        // Measured from the warp, it holds where macOS adds no jump too.
        let mut moved = RawEvent {
            location: Point::new(9.0, 301.0),
            delta: Point::new(5.0, 1.0),
            ..raw(kind::LEFT_DRAGGED)
        };
        assert!(warp.discount(&mut moved));
        assert_eq!(moved.delta, Point::new(5.0, 1.0));
    }

    #[test]
    fn motion_from_before_a_warp_keeps_its_delta() {
        let warp = Warp {
            from: Point::new(1800.0, 900.0),
            to: Point::new(4.0, 300.0),
        };
        let mut moved = RawEvent {
            location: Point::new(1806.0, 900.0),
            delta: Point::new(6.0, 0.0),
            ..raw(kind::MOVED)
        };
        assert!(!warp.discount(&mut moved));
        assert_eq!(moved.delta, Point::new(6.0, 0.0));
    }

    #[test]
    fn warps_in_a_row_jump_from_where_the_first_began() {
        let warp = Warp {
            from: Point::new(1800.0, 900.0),
            to: Point::new(4.0, 300.0),
        }
        .then(Point::new(10.0, 20.0));
        assert_eq!(
            warp,
            Warp {
                from: Point::new(1800.0, 900.0),
                to: Point::new(10.0, 20.0),
            }
        );
    }

    #[test]
    fn modifiers_follow_the_device_bits() {
        let mut t = Translator::default();
        let flags = |keycode, flags| RawEvent {
            keycode,
            flags,
            ..raw(kind::FLAGS_CHANGED)
        };
        let key = |usage, pressed| vec![InputEvent::Key { usage, pressed }];
        // Left shift down, then right shift down while left is held.
        assert_eq!(
            t.translate(&flags(0x38, flag::SHIFT | flag::LEFT_SHIFT)),
            key(usage::LEFT_SHIFT, true)
        );
        assert_eq!(
            t.translate(&flags(
                0x3c,
                flag::SHIFT | flag::LEFT_SHIFT | flag::RIGHT_SHIFT
            )),
            key(usage::RIGHT_SHIFT, true)
        );
        // Releasing the left one keeps the generic bit for the right one.
        assert_eq!(
            t.translate(&flags(0x38, flag::SHIFT | flag::RIGHT_SHIFT)),
            key(usage::LEFT_SHIFT, false)
        );
        assert_eq!(t.translate(&flags(0x3c, 0)), key(usage::RIGHT_SHIFT, false));
        // Without device bits, presses and releases alternate.
        assert_eq!(
            t.translate(&flags(0x37, flag::COMMAND)),
            key(usage::LEFT_GUI, true)
        );
        assert_eq!(t.translate(&flags(0x37, 0)), key(usage::LEFT_GUI, false));
        // Caps Lock is a press and release each time.
        assert_eq!(
            t.translate(&flags(0x39, flag::CAPS_LOCK)),
            vec![
                InputEvent::Key {
                    usage: usage::CAPS_LOCK,
                    pressed: true
                },
                InputEvent::Key {
                    usage: usage::CAPS_LOCK,
                    pressed: false
                },
            ]
        );
    }

    #[test]
    fn scrolling_is_reported_in_physical_wheel_notches() {
        let wheel_away = RawEvent {
            scroll_lines: Point::new(0.0, 1.0),
            ..raw(kind::SCROLL)
        };
        let mut classic = Translator::default();
        assert_eq!(
            classic.translate(&wheel_away),
            vec![InputEvent::Scroll { dx: 0.0, dy: 1.0 }]
        );
        // With natural scrolling the same turn of the wheel arrives negated.
        let mut natural = Translator {
            natural_scrolling: true,
            ..Translator::default()
        };
        let natural_away = RawEvent {
            scroll_lines: Point::new(0.0, -1.0),
            ..raw(kind::SCROLL)
        };
        assert_eq!(
            natural.translate(&natural_away),
            vec![InputEvent::Scroll { dx: 0.0, dy: 1.0 }]
        );
        // Trackpads report pixels.
        let trackpad = RawEvent {
            scroll_continuous: true,
            scroll_pixels: Point::new(-5.0, 25.0),
            ..raw(kind::SCROLL)
        };
        assert_eq!(
            classic.translate(&trackpad),
            vec![InputEvent::Scroll { dx: 0.5, dy: 2.5 }]
        );
        assert!(classic.translate(&raw(kind::SCROLL)).is_empty());
        // Injection undoes what capture did.
        assert_eq!(to_quartz_pixels(0.5, 2.5, false), (-5.0, 25.0));
        assert_eq!(to_quartz_pixels(0.0, 1.0, true), (0.0, -10.0));
    }

    #[test]
    fn motion_with_a_button_held_is_a_drag() {
        let mut i = Injector::default();
        let now = Instant::now();
        assert!(matches!(
            i.move_to(Point::new(5.0, 5.0)),
            Synth::Mouse { kind: kind::MOVED, delta, .. } if delta == Point::new(5.0, 5.0)
        ));
        i.button(Button::Left, true, now);
        assert!(matches!(
            i.move_to(Point::new(6.0, 5.0)),
            Synth::Mouse {
                kind: kind::LEFT_DRAGGED,
                ..
            }
        ));
        i.button(Button::Left, false, now);
        i.button(Button::Middle, true, now);
        assert!(matches!(
            i.move_to(Point::new(7.0, 5.0)),
            Synth::Mouse {
                kind: kind::OTHER_DRAGGED,
                button_number: 2,
                ..
            }
        ));
    }

    #[test]
    fn quick_presses_in_place_count_as_double_clicks() {
        let mut i = Injector::default();
        let t0 = Instant::now();
        let click = |i: &mut Injector, at: Duration| {
            let down = i.button(Button::Left, true, t0 + at);
            let up = i.button(Button::Left, false, t0 + at);
            match (down, up) {
                (Synth::Mouse { click_state: d, .. }, Synth::Mouse { click_state: u, .. }) => {
                    assert_eq!(d, u);
                    d
                }
                _ => unreachable!(),
            }
        };
        assert_eq!(click(&mut i, Duration::ZERO), 1);
        assert_eq!(click(&mut i, Duration::from_millis(200)), 2);
        assert_eq!(click(&mut i, Duration::from_millis(400)), 3);
        // Too late.
        assert_eq!(click(&mut i, Duration::from_millis(1500)), 1);
        // Too far.
        i.move_to(Point::new(100.0, 100.0));
        assert_eq!(click(&mut i, Duration::from_millis(1600)), 1);
    }

    #[test]
    fn the_virtual_devices_events_are_taken_as_this_agents_own() {
        assert!(!is_own(false, false), "someone at this Mac");
        assert!(is_own(true, false));
        // Untagged, as hardware, while the virtual device injects.
        assert!(is_own(false, true));
        assert!(is_own(true, true));
    }

    /// Keys and buttons held when injection moves to the virtual device
    /// are let go of here and pressed again there, and back at unlock, so
    /// Shift held through the switch stays held.
    #[test]
    fn what_is_held_moves_to_the_virtual_device_and_back() {
        use crate::hid_report::{Keyboard, Mouse};

        let mut i = Injector::default();
        i.key(usage::A, true);
        i.key(usage::LEFT_SHIFT, true);
        i.button(Button::Left, true, Instant::now());
        let (keys, buttons) = i.held();
        assert_eq!(keys, [usage::LEFT_SHIFT, usage::A]);
        assert_eq!(buttons, [Button::Left]);
        assert_eq!(i.release_all(Instant::now()).len(), 3);

        let (mut keyboard, mut mouse) = (Keyboard::default(), Mouse::default());
        let reports: Vec<_> = keys.iter().filter_map(|&k| keyboard.key(k, true)).collect();
        // Shift goes down first, so A arrives shifted.
        assert_eq!(reports[0][1], 0b10);
        assert_eq!(reports[1][1..4], [0b10, 0, 0x04]);
        assert!(buttons.iter().all(|&b| mouse.button(b, true).is_some()));
        assert_eq!(keyboard.held(), [usage::LEFT_SHIFT, usage::A]);
        assert_eq!(mouse.held(), [Button::Left]);

        // At unlock: let go on the device, press again on Quartz.
        let (keys, buttons) = (keyboard.held(), mouse.held());
        assert!(keyboard.release_all().is_some() && mouse.release_all().is_some());
        let mut back = Injector::default();
        for key in keys {
            assert!(matches!(
                back.key(key, true),
                Some(Synth::Key { down: true, .. })
            ));
        }
        for button in buttons {
            back.button(button, true, Instant::now());
        }
        assert_eq!(
            back.held(),
            (vec![usage::LEFT_SHIFT, usage::A], vec![Button::Left])
        );
    }

    #[test]
    fn everything_held_can_be_let_go() {
        let mut i = Injector::default();
        i.key(usage::LEFT_SHIFT, true);
        i.key(usage::A, true);
        i.button(Button::Left, true, Instant::now());
        let released = i.release_all(Instant::now());
        assert_eq!(released.len(), 3);
        assert!(matches!(
            released[0],
            Synth::Mouse {
                kind: kind::LEFT_UP,
                ..
            }
        ));
        assert!(matches!(
            released[1],
            Synth::Key {
                keycode: 0x00,
                down: false,
                ..
            }
        ));
        assert!(matches!(
            released[2],
            Synth::Key {
                keycode: 0x38,
                down: false,
                flags: 0,
                ..
            }
        ));
        assert!(i.release_all(Instant::now()).is_empty());
    }

    #[test]
    fn keys_carry_the_modifiers_held() {
        let mut i = Injector::default();
        let shift = i.key(usage::LEFT_SHIFT, true).unwrap();
        assert!(matches!(
            shift,
            Synth::Key { keycode: 0x38, down: true, flags_changed: true, flags, .. }
                if flags == flag::SHIFT | flag::LEFT_SHIFT
        ));
        let a = i.key(usage::A, true).unwrap();
        assert!(matches!(
            a,
            Synth::Key { keycode: 0x00, flags_changed: false, autorepeat: false, flags, .. }
                if flags == flag::SHIFT | flag::LEFT_SHIFT
        ));
        // A second press while held is a key repeat.
        assert!(matches!(
            i.key(usage::A, true).unwrap(),
            Synth::Key {
                autorepeat: true,
                ..
            }
        ));
        i.key(usage::A, false);
        let up = i.key(usage::LEFT_SHIFT, false).unwrap();
        assert!(matches!(
            up,
            Synth::Key {
                down: false,
                flags: 0,
                ..
            }
        ));
        // Keys without a macOS code are skipped.
        assert!(i.key(HidUsage::new(0x73), true).is_none());
        assert!(i.release_all(Instant::now()).is_empty());
        let wheel = i.scroll(0.0, 1.0);
        assert_eq!(
            wheel,
            Synth::Scroll {
                pixels: (0.0, 10.0),
                flags: 0
            }
        );
    }

    #[test]
    fn relative_movements_go_in_whole_points_and_keep_the_rest() {
        let mut injector = Injector::new(Point::new(100.0, 100.0), ClickPolicy::default(), false);
        let fit = |p: Point| Point::new(p.x.clamp(0.0, 1000.0), p.y.clamp(0.0, 800.0));
        let delta = |synth: Synth| match synth {
            Synth::Mouse {
                kind,
                location,
                delta,
                ..
            } => (kind, location, delta),
            other => panic!("{other:?}"),
        };
        let from = Point::new(100.0, 100.0);
        assert_eq!(
            delta(injector.move_by(Point::new(2.5, -0.75), from, fit)),
            (kind::MOVED, Point::new(102.0, 100.0), Point::new(2.0, 0.0))
        );
        // The halves and quarters add up.
        assert_eq!(
            delta(injector.move_by(Point::new(0.5, -0.5), Point::new(102.0, 100.0), fit)),
            (kind::MOVED, Point::new(103.0, 99.0), Point::new(1.0, -1.0))
        );
        // At the edge the cursor stays, and the movement still goes.
        assert_eq!(
            delta(injector.move_by(Point::new(40.0, 0.0), Point::new(990.0, 99.0), fit)),
            (kind::MOVED, Point::new(1000.0, 99.0), Point::new(40.0, 0.0))
        );
        // With a button held it is a drag.
        injector.button(Button::Left, true, Instant::now());
        // Kept on the screens along whichever axis still is.
        let screens = [
            Rect::new(0.0, 0.0, 1000.0, 800.0),
            Rect::new(1000.0, 0.0, 500.0, 400.0),
        ];
        let on = |from: (f64, f64), to: (f64, f64)| {
            on_screens(&screens, Point::new(from.0, from.1), Point::new(to.0, to.1))
        };
        assert_eq!(
            on((10.0, 10.0), (1200.0, 300.0)),
            Some(Point::new(1200.0, 300.0))
        );
        assert_eq!(
            on((990.0, 600.0), (1100.0, 610.0)),
            Some(Point::new(990.0, 610.0))
        );
        assert_eq!(on((10.0, 10.0), (-5.0, -5.0)), None);
        assert_eq!(
            delta(injector.move_by(Point::new(-1.0, 0.0), Point::new(500.0, 400.0), fit)).0,
            kind::LEFT_DRAGGED
        );
    }
}
