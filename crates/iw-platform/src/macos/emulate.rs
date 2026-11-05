use std::sync::Arc;
use std::time::{Duration, Instant};

use iw_core::HidUsage;
use iw_core::geometry::{Point, Rect};
use iw_core::input::Button;
use objc2_core_foundation::{CFRetained, CGPoint};
use objc2_core_graphics::{
    CGAssociateMouseAndMouseCursorPosition, CGEvent, CGEventField, CGEventFlags, CGEventSource,
    CGEventSourceStateID, CGEventTapLocation, CGEventType, CGMouseButton, CGScrollEventUnit,
    CGWarpMouseCursorPosition,
};
use tracing::{info, warn};

use super::hid::{self, HidState, VirtualHid};
use super::translate::{self, Injector, Synth};
use super::{INJECTED, Preferences};
use crate::hid_report::{self, approach};
use crate::{Displays, InputEmulation, PlatformError};

/// Posts synthesized events at the HID level, tagged so this agent's own
/// capture ignores them. Without Accessibility permission macOS drops them
/// silently, which the permission status reports.
///
/// The lock screen drops posted events whatever the permission, so while
/// it is up, input goes through the virtual keyboard and mouse instead
/// (`hid`), when this build may create one.
pub struct MacEmulation {
    source: EventSource,
    injector: Injector,
    preferences: Preferences,
    /// Scroll left over from rounding to whole pixels.
    scroll_remainder: (f64, f64),
    hid: Hid,
    relative: RelativeTrack,
}

/// What relative movements go by, so that one costs no CoreGraphics query
/// (`move_by`): the screens, read at most every `SCREENS_KEPT`, and the
/// cursor, read when a run of movements starts or when one would leave
/// the screens.
#[derive(Default)]
struct RelativeTrack {
    screens: Vec<Rect>,
    screens_read: Option<Instant>,
    /// When the last relative movement went.
    last: Option<Instant>,
}

/// How long the screens' bounds are taken as they were read.
const SCREENS_KEPT: Duration = Duration::from_secs(1);

/// A relative movement after this long without one starts from where the
/// cursor really is, as anything may have moved it meanwhile.
const RELATIVE_PAUSE: Duration = Duration::from_secs(1);

impl RelativeTrack {
    fn screens(&mut self, now: Instant) -> &[Rect] {
        if self
            .screens_read
            .is_none_or(|read| now.saturating_duration_since(read) >= SCREENS_KEPT)
        {
            match super::displays::MacDisplays.displays() {
                Ok(displays) => self.screens = displays.into_iter().map(|d| d.bounds).collect(),
                Err(e) => warn!(error = %e, "cannot read the screens for relative movements"),
            }
            self.screens_read = Some(now);
        }
        &self.screens
    }
}

/// An event source used by one owner at a time.
struct EventSource(CFRetained<CGEventSource>);

// SAFETY: CF objects are reference counted atomically, and an event source
// carries no thread affinity; `MacEmulation` only uses it through `&mut`.
unsafe impl Send for EventSource {}

/// When input goes through the virtual device.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HidUse {
    /// While the screen is locked, if this build is entitled to it.
    WhileLocked,
    /// Always: at the login window posted events never arrive.
    Always,
}

/// How often the lock screen is looked for, at most.
const LOCK_CHECK: Duration = Duration::from_millis(250);

// The pauses below block the engine task that called into injection. They
// happen only while the lock screen is driven through the virtual device:
// once when it is created, and around clicks.

/// How long a new virtual device is given to be attached before its first
/// report, which the system might otherwise drop. Once per lock.
const ATTACH_PAUSE: Duration = Duration::from_millis(50);

/// Steps that bring the cursor to where a click belongs before it is
/// pressed, and the pause after each, so the system has moved it before
/// it is read again.
const CLICK_STEPS: usize = 8;
const CLICK_STEP_PAUSE: Duration = Duration::from_millis(4);

/// Farther than this from where it belongs, the cursor is warped there
/// rather than stepped, while warping works.
const WARP_DISTANCE: f64 = 16.0;
/// Within this of the target counts as there.
const ARRIVED: f64 = 1.0;

/// The virtual keyboard and mouse, while they are used.
struct Hid {
    usage: HidUse,
    /// Shared with capture and the session status.
    state: Arc<HidState>,
    device: Option<VirtualHid>,
    keyboard: hid_report::Keyboard,
    mouse: hid_report::Mouse,
    /// Where the cursor was last asked to go.
    target: Option<Point>,
    /// Whether the lock screen was up, and when that was read.
    locked: Option<(bool, Instant)>,
    /// A warp did not move the cursor since the device was created: the
    /// lock screen may ignore warps, and then only the device's steps move
    /// it.
    warp_ignored: bool,
}

impl Hid {
    fn new(usage: HidUse, state: Arc<HidState>) -> Self {
        Self {
            usage,
            state,
            device: None,
            keyboard: hid_report::Keyboard::default(),
            mouse: hid_report::Mouse::default(),
            target: None,
            locked: None,
            warp_ignored: false,
        }
    }

    /// Whether input should go through the device now: while the lock
    /// screen is up, and this session is the console's. The device is the
    /// whole system's, so from a session fast user switching moved aside
    /// its input would reach another user's. `fresh` reads the session
    /// again rather than trusting the last read.
    fn wanted(&mut self, fresh: bool) -> bool {
        match self.usage {
            HidUse::Always => true,
            // Without the entitlement, or once creating the device failed,
            // there is nothing to switch to, so the lock screen is not even
            // looked for.
            HidUse::WhileLocked if !hid::entitled() || self.state.failed() => false,
            HidUse::WhileLocked => {
                let now = Instant::now();
                match self.locked {
                    Some((locked, at)) if !fresh && now.duration_since(at) < LOCK_CHECK => locked,
                    _ => {
                        let locked = super::lock_screen_up() && super::on_console();
                        self.locked = Some((locked, now));
                        locked
                    }
                }
            }
        }
    }

    fn send(&self, report: &[u8]) -> Result<(), PlatformError> {
        match &self.device {
            Some(device) => device.send(report),
            None => Err(PlatformError::Unavailable("the virtual keyboard and mouse")),
        }
    }

    /// Sends `report`, saying why it could not be sent.
    fn send_or_warn(&self, report: &[u8], doing: &str) {
        if let Err(e) = self.send(report) {
            warn!(error = %e, "cannot {doing} on the virtual keyboard and mouse");
        }
    }

    /// Presses `keys` (modifiers first) and `buttons` on the device.
    fn press(&mut self, keys: &[HidUsage], buttons: &[Button]) {
        for &key in keys {
            if let Some(report) = self.keyboard.key(key, true) {
                self.send_or_warn(&report, "press a key held");
            }
        }
        for &button in buttons {
            if let Some(report) = self.mouse.button(button, true) {
                self.send_or_warn(&report, "press a button held");
            }
        }
    }

    /// Lets go of everything the device holds and removes it. Its events
    /// still count as its own for a moment (`HidState::owns_input`).
    fn close(&mut self) {
        let releases = self
            .keyboard
            .release_all()
            .map(|r| r.to_vec())
            .into_iter()
            .chain(self.mouse.release_all().map(|r| r.to_vec()));
        for report in releases {
            self.send_or_warn(&report, "let go of a key or button");
        }
        self.device = None;
        self.target = None;
        self.warp_ignored = false;
        self.state.set_in_use(false);
    }
}

impl MacEmulation {
    pub fn new() -> Result<Self, PlatformError> {
        Self::with_hid(Arc::default())
    }

    /// Injection that tells capture and the session status, through `hid`,
    /// when it drives the virtual device.
    pub(crate) fn with_hid(hid: Arc<HidState>) -> Result<Self, PlatformError> {
        let source = CGEventSource::new(CGEventSourceStateID::HIDSystemState)
            .ok_or_else(|| PlatformError::Os("cannot create a Quartz event source".to_owned()))?;
        CGEventSource::set_user_data(Some(&source), INJECTED);
        // Posting events otherwise suppresses the local mouse and keyboard
        // for a quarter second, delaying the local user taking over.
        CGEventSource::set_local_events_suppression_interval(Some(&source), 0.0);
        let mut preferences = Preferences::read();
        let injector = Injector::new(
            current_position(),
            preferences.clicks(),
            preferences.natural_scrolling(),
        );
        Ok(Self {
            source: EventSource(source),
            injector,
            preferences,
            scroll_remainder: (0.0, 0.0),
            hid: Hid::new(HidUse::WhileLocked, hid),
            relative: RelativeTrack::default(),
        })
    }

    /// Injects through the virtual keyboard and mouse only. The caller is
    /// the login window's agent: at
    /// the login window posted events never arrive. Fails when this build
    /// cannot create the device.
    pub fn virtual_only() -> Result<Self, PlatformError> {
        let mut emulation = Self::new()?;
        emulation.hid.usage = HidUse::Always;
        match VirtualHid::new() {
            Ok(device) => emulation.hid.device = Some(device),
            Err(e) => {
                emulation.hid.state.set_failed();
                return Err(e);
            }
        }
        emulation.hid.state.set_in_use(true);
        std::thread::sleep(ATTACH_PAUSE);
        Ok(emulation)
    }

    /// Whether this call goes through the virtual device: it is created
    /// when the lock screen comes up and removed when it goes. What is held
    /// moves with the input, released on the way it leaves and pressed again
    /// on the way it takes, so a modifier held across the switch stays held.
    fn through_device(&mut self) -> bool {
        self.switch_path(true)
    }

    /// Creates or removes the device as `device_change` says, and returns
    /// whether input goes through it.
    fn switch_path(&mut self, injecting: bool) -> bool {
        let wanted = self.hid.wanted(!injecting);
        match device_change(wanted, self.hid.device.is_some(), injecting) {
            DeviceChange::Keep => {}
            DeviceChange::Create => match VirtualHid::new() {
                Ok(device) => {
                    info!("the screen is locked; injecting through the virtual keyboard and mouse");
                    self.hid.device = Some(device);
                    // Before anything goes through it, so capture never
                    // takes its events for someone at this Mac.
                    self.hid.state.set_in_use(true);
                    std::thread::sleep(ATTACH_PAUSE);
                    self.hand_held_to_device();
                }
                Err(e) => {
                    // Not tried again; other devices are told this lock
                    // screen takes no input, and keep the cursor off it.
                    warn!(error = %e, "the screen is locked, and input cannot reach it");
                    self.hid.state.set_failed();
                }
            },
            DeviceChange::Remove => {
                info!("the screen is unlocked; posting events again");
                self.hand_held_to_quartz();
            }
        }
        self.hid.device.is_some()
    }

    /// Lets go of what posted events hold and presses it on the device.
    fn hand_held_to_device(&mut self) {
        let (keys, buttons) = self.injector.held();
        for synth in self.injector.release_all(Instant::now()) {
            if let Err(e) = self.post(synth) {
                warn!(error = %e, "cannot let go of a key or button before the lock screen");
            }
        }
        self.hid.press(&keys, &buttons);
    }

    /// Lets go of what the device holds, removes it, and presses the same
    /// with posted events.
    fn hand_held_to_quartz(&mut self) {
        let (keys, buttons) = (self.hid.keyboard.held(), self.hid.mouse.held());
        self.hid.close();
        // The device moved the cursor meanwhile.
        self.injector.position = current_position();
        for key in keys {
            if let Some(synth) = self.injector.key(key, true)
                && let Err(e) = self.post(synth)
            {
                warn!(error = %e, "cannot press a key held after the lock screen");
            }
        }
        for button in buttons {
            let synth = self.injector.button(button, true, Instant::now());
            if let Err(e) = self.post(synth) {
                warn!(error = %e, "cannot press a button held after the lock screen");
            }
        }
    }

    /// Puts the cursor at `target` at once, if warping works here: the lock
    /// screen may ignore warps, which is then remembered until it goes.
    fn warp_device_cursor(&mut self, target: Point) -> bool {
        if self.hid.warp_ignored {
            return false;
        }
        CGWarpMouseCursorPosition(CGPoint::new(target.x, target.y));
        // Warping makes macOS ignore the mouse for a quarter second;
        // re-associating cancels that.
        CGAssociateMouseAndMouseCursorPosition(true);
        if distance(current_position(), target) <= ARRIVED {
            return true;
        }
        info!("the lock screen ignores warps; moving the cursor in steps");
        self.hid.warp_ignored = true;
        false
    }

    /// Moves the cursor towards `target` from where it really is: a warp
    /// when it is far and warps work, otherwise one step, whose result the
    /// system's acceleration decides and the next call corrects. Returns
    /// whether it moved.
    fn device_cursor_towards(&mut self, target: Point) -> Result<bool, PlatformError> {
        let current = current_position();
        if distance(current, target) > WARP_DISTANCE && self.warp_device_cursor(target) {
            return Ok(true);
        }
        let Some((dx, dy)) = approach(current, target) else {
            return Ok(false);
        };
        for report in self.hid.mouse.motion(f64::from(dx), f64::from(dy)) {
            self.hid.send(&report)?;
        }
        Ok(true)
    }

    /// Moves the cursor to where it was last asked to go, in a few short
    /// steps, so a click lands there.
    fn settle_cursor(&mut self) -> Result<(), PlatformError> {
        let Some(target) = self.hid.target else {
            return Ok(());
        };
        for _ in 0..CLICK_STEPS {
            if !self.device_cursor_towards(target)? {
                break;
            }
            std::thread::sleep(CLICK_STEP_PAUSE);
        }
        Ok(())
    }

    fn post(&mut self, synth: Synth) -> Result<(), PlatformError> {
        let source = Some(&*self.source.0);
        let event = match synth {
            Synth::Mouse {
                kind,
                location,
                delta,
                button_number,
                click_state,
                flags,
            } => {
                let event = CGEvent::new_mouse_event(
                    source,
                    CGEventType(kind),
                    CGPoint::new(location.x, location.y),
                    CGMouseButton(button_number as u32),
                );
                if let Some(e) = event.as_deref() {
                    let e = Some(e);
                    CGEvent::set_integer_value_field(
                        e,
                        CGEventField::MouseEventDeltaX,
                        delta.x.round() as i64,
                    );
                    CGEvent::set_integer_value_field(
                        e,
                        CGEventField::MouseEventDeltaY,
                        delta.y.round() as i64,
                    );
                    if click_state > 0 {
                        CGEvent::set_integer_value_field(
                            e,
                            CGEventField::MouseEventClickState,
                            click_state,
                        );
                    }
                    CGEvent::set_flags(e, CGEventFlags::from_bits_retain(flags));
                }
                event
            }
            Synth::Key {
                keycode,
                down,
                flags_changed,
                autorepeat,
                flags,
            } => {
                let event = CGEvent::new_keyboard_event(source, keycode, down);
                if let Some(e) = event.as_deref() {
                    let e = Some(e);
                    if flags_changed {
                        CGEvent::set_type(e, CGEventType::FlagsChanged);
                    }
                    CGEvent::set_integer_value_field(
                        e,
                        CGEventField::KeyboardEventAutorepeat,
                        i64::from(autorepeat),
                    );
                    CGEvent::set_flags(e, CGEventFlags::from_bits_retain(flags));
                }
                event
            }
            Synth::Scroll { pixels, flags } => {
                let x = pixels.0 + self.scroll_remainder.0;
                let y = pixels.1 + self.scroll_remainder.1;
                let (whole_x, whole_y) = (x.trunc(), y.trunc());
                self.scroll_remainder = (x - whole_x, y - whole_y);
                if whole_x == 0.0 && whole_y == 0.0 {
                    return Ok(());
                }
                let event = CGEvent::new_scroll_wheel_event2(
                    source,
                    CGScrollEventUnit::Pixel,
                    2,
                    whole_y as i32,
                    whole_x as i32,
                    0,
                );
                if let Some(e) = event.as_deref() {
                    CGEvent::set_flags(Some(e), CGEventFlags::from_bits_retain(flags));
                }
                event
            }
        };
        let event =
            event.ok_or_else(|| PlatformError::Os("cannot create a Quartz event".to_owned()))?;
        // Tag the event itself too, in case the source's user data is not
        // carried over.
        CGEvent::set_integer_value_field(Some(&event), CGEventField::EventSourceUserData, INJECTED);
        CGEvent::post(CGEventTapLocation::HIDEventTap, Some(&event));
        Ok(())
    }

    /// Picks up changes to the user's scrolling and double-click settings.
    fn refresh_preferences(&mut self) {
        if self.preferences.stale() {
            self.preferences = Preferences::read();
            self.injector.clicks = self.preferences.clicks();
            self.injector.natural_scrolling = self.preferences.natural_scrolling();
        }
    }
}

/// What to do with the virtual device.
#[derive(Debug, PartialEq, Eq)]
enum DeviceChange {
    Keep,
    Create,
    Remove,
}

/// What to do with the virtual device, from whether input should go
/// through it now and whether it exists. Only injecting creates it: a
/// refresh, with nothing to inject, only removes one no longer wanted.
fn device_change(wanted: bool, exists: bool, injecting: bool) -> DeviceChange {
    match (wanted, exists) {
        (true, false) if injecting => DeviceChange::Create,
        (false, true) => DeviceChange::Remove,
        _ => DeviceChange::Keep,
    }
}

fn distance(a: Point, b: Point) -> f64 {
    let d = a - b;
    (d.x * d.x + d.y * d.y).sqrt()
}

/// Where the cursor is now.
fn current_position() -> Point {
    let event = CGEvent::new(None);
    let p = CGEvent::location(event.as_deref());
    Point::new(p.x, p.y)
}

impl InputEmulation for MacEmulation {
    fn move_to(&mut self, position: Point) -> Result<(), PlatformError> {
        if self.through_device() {
            self.hid.target = Some(position);
            self.device_cursor_towards(position)?;
            return Ok(());
        }
        let synth = self.injector.move_to(position);
        self.post(synth)
    }

    fn button(&mut self, button: Button, pressed: bool) -> Result<(), PlatformError> {
        if self.through_device() {
            if pressed {
                self.settle_cursor()?;
            }
            return match self.hid.mouse.button(button, pressed) {
                Some(report) => self.hid.send(&report),
                None => Ok(()),
            };
        }
        self.refresh_preferences();
        let synth = self.injector.button(button, pressed, Instant::now());
        self.post(synth)
    }

    fn key(&mut self, usage: HidUsage, pressed: bool) -> Result<(), PlatformError> {
        if self.through_device() {
            return match self.hid.keyboard.key(usage, pressed) {
                Some(report) => self.hid.send(&report),
                None => Ok(()),
            };
        }
        match self.injector.key(usage, pressed) {
            Some(synth) => self.post(synth),
            None => Err(PlatformError::Unsupported("key without a macOS key code")),
        }
    }

    /// Hands input back from the virtual device once the lock screen is
    /// gone, also when it was unlocked at this Mac and nothing is injected;
    /// never creates the device.
    fn refresh(&mut self) {
        if self.hid.device.is_some() {
            self.switch_path(false);
        }
    }

    /// Through the virtual mouse on the lock screen, as its own movement;
    /// otherwise a posted movement whose delta fields carry it.
    fn move_by(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
        if self.through_device() {
            for report in self.hid.mouse.motion(dx, dy) {
                self.hid.send(&report)?;
            }
            return Ok(());
        }
        let now = Instant::now();
        if self
            .relative
            .last
            .is_none_or(|last| now.saturating_duration_since(last) >= RELATIVE_PAUSE)
        {
            self.injector.position = current_position();
        }
        self.relative.last = Some(now);
        let from = self.injector.position;
        let screens = self.relative.screens(now);
        // Off the screens from where it was thought to be: the app may have
        // moved it, so from where it really is.
        let fit = |to: Point| {
            translate::on_screens(screens, from, to).unwrap_or_else(|| {
                let real = current_position();
                translate::on_screens(screens, real, real + (to - from)).unwrap_or(real)
            })
        };
        let synth = self.injector.move_by(Point::new(dx, dy), from, fit);
        self.post(synth)
    }

    fn position(&mut self) -> Option<Point> {
        let position = current_position();
        self.injector.position = position;
        Some(position)
    }

    fn scroll(&mut self, dx: f64, dy: f64) -> Result<(), PlatformError> {
        if self.through_device() {
            for report in self.hid.mouse.scroll(dx, dy) {
                self.hid.send(&report)?;
            }
            return Ok(());
        }
        self.refresh_preferences();
        let synth = self.injector.scroll(dx, dy);
        self.post(synth)
    }
}

impl Drop for MacEmulation {
    fn drop(&mut self) {
        if self.hid.device.is_some() {
            self.hid.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_injecting_creates_the_device() {
        assert_eq!(device_change(true, false, true), DeviceChange::Create);
        // A refresh while the lock screen is up but nothing is injected.
        assert_eq!(device_change(true, false, false), DeviceChange::Keep);
        // Unlocked: removed by injection or by a refresh alike.
        assert_eq!(device_change(false, true, true), DeviceChange::Remove);
        assert_eq!(device_change(false, true, false), DeviceChange::Remove);
        assert_eq!(device_change(true, true, false), DeviceChange::Keep);
        assert_eq!(device_change(false, false, true), DeviceChange::Keep);
    }
}
