//! Hotkeys: the key combinations InputWeave acts on itself, whichever
//! device the cursor is on, and how they are written down.
//!
//! A hotkey is written as its modifiers and then its key, joined by `+`:
//! `control+alt+shift+Escape`. Modifiers are `control`, `alt`, `shift` and
//! `gui` (⌘ on a Mac, the Windows key), in that order; the key is its UI
//! Events `code` name (`KeyL`, `Digit1`, `Escape`, `F5`, as
//! [`HidUsage::name`] gives it). Both are read without regard to case.
//! The hotkeys that jump to a device by its number share their modifiers,
//! written alone (`control+alt+shift`), with the digits 1 to 9.
//!
//! Taking control back and the lock may also be a key typing never uses
//! alone, without modifiers (`Hotkey::works_alone`), written as that key:
//! `F13`. Control pressed twice quickly takes control back too
//! (`Hotkeys::take_back_double`), written `double+control`.

use std::fmt;
use std::str::FromStr;
use std::time::{Duration, Instant};

use iw_keymap::usage;

use crate::HidUsage;

/// How many devices can be jumped to by number: the digits 1 to 9.
pub const JUMP_SLOTS: u8 = 9;

/// How many hotkeys capture is given to hold back at most
/// (`Hotkeys::reserved`): the lock and the jumps.
pub const RESERVED_MAX: usize = 1 + JUMP_SLOTS as usize;

/// Which of the reserved hotkeys (`Hotkeys::reserved`) capture holds back,
/// so local apps never see their keys go down; the others reach them. A
/// set small enough to copy, as the input events that tell it are
/// (`InputEvent::HotkeysHeldBack`): [`RESERVED_MAX`] at most. Numbered
/// among the sets a capture tells, so one told earlier, read after a later
/// one, never replaces it (`take`). Equal when they hold the same hotkeys,
/// whatever their numbers.
#[derive(Clone, Copy, Debug, Default)]
pub struct HeldHotkeys {
    hotkeys: [Option<Hotkey>; RESERVED_MAX],
    change: u64,
}

impl PartialEq for HeldHotkeys {
    fn eq(&self, other: &Self) -> bool {
        self.hotkeys == other.hotkeys
    }
}

impl Eq for HeldHotkeys {}

impl HeldHotkeys {
    /// The same hotkeys as the `change`th set a capture told; 0, the
    /// default, for one that numbers none.
    pub fn as_change(self, change: u64) -> Self {
        Self { change, ..self }
    }

    pub fn change(&self) -> u64 {
        self.change
    }

    /// Takes `told` in place of these, unless capture told it before them.
    pub fn take(&mut self, told: HeldHotkeys) {
        if told.change >= self.change {
            *self = told;
        }
    }

    pub fn contains(&self, hotkey: &Hotkey) -> bool {
        self.hotkeys.contains(&Some(*hotkey))
    }

    pub fn iter(&self) -> impl Iterator<Item = Hotkey> + '_ {
        self.hotkeys.iter().flatten().copied()
    }

    pub fn len(&self) -> usize {
        self.iter().count()
    }

    pub fn is_empty(&self) -> bool {
        self.hotkeys[0].is_none()
    }
}

impl FromIterator<Hotkey> for HeldHotkeys {
    /// The first [`RESERVED_MAX`] of them; any after those count as not
    /// held back.
    fn from_iter<I: IntoIterator<Item = Hotkey>>(hotkeys: I) -> Self {
        let mut held = Self::default();
        for (slot, hotkey) in held.hotkeys.iter_mut().zip(hotkeys) {
            *slot = Some(hotkey);
        }
        held
    }
}

/// How `Hotkeys::take_back_double` is written when it is on; off is the
/// empty string.
pub const DOUBLE_CONTROL: &str = "double+control";

/// The longest pressing and letting go of Control twice may take, from the
/// first press to the second release, to take control back
/// (`Hotkeys::take_back_double`).
pub const DOUBLE_PRESS_WITHIN: Duration = Duration::from_millis(400);

/// A modifier key, either the left or the right one.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Modifier {
    Control,
    Alt,
    Shift,
    /// ⌘ on a Mac, the Windows key.
    Gui,
}

impl Modifier {
    /// In the order they are written.
    pub const ALL: [Modifier; 4] = [Self::Control, Self::Alt, Self::Shift, Self::Gui];

    pub fn name(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Alt => "alt",
            Self::Shift => "shift",
            Self::Gui => "gui",
        }
    }

    /// The modifier called `name`, ignoring case.
    pub fn from_name(name: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|m| m.name().eq_ignore_ascii_case(name))
    }

    /// Its left and right keys.
    pub fn keys(self) -> [HidUsage; 2] {
        match self {
            Self::Control => [usage::LEFT_CONTROL, usage::RIGHT_CONTROL],
            Self::Alt => [usage::LEFT_ALT, usage::RIGHT_ALT],
            Self::Shift => [usage::LEFT_SHIFT, usage::RIGHT_SHIFT],
            Self::Gui => [usage::LEFT_GUI, usage::RIGHT_GUI],
        }
    }

    /// The modifier `key` is one of the keys of, if any.
    pub fn of(key: HidUsage) -> Option<Self> {
        Self::ALL.into_iter().find(|m| m.keys().contains(&key))
    }

    /// Whether either of its keys is down, as `held` reports.
    pub fn is_held(self, held: impl Fn(HidUsage) -> bool) -> bool {
        self.keys().into_iter().any(held)
    }
}

impl fmt::Display for Modifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for Modifier {
    type Err = HotkeyError;

    fn from_str(s: &str) -> Result<Self, HotkeyError> {
        Self::from_name(s.trim()).ok_or_else(|| HotkeyError::UnknownKey(s.trim().to_owned()))
    }
}

/// A set of modifiers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct Modifiers {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub gui: bool,
}

impl Modifiers {
    pub const NONE: Self = Self {
        control: false,
        alt: false,
        shift: false,
        gui: false,
    };

    /// ⌃⌥⇧, which the default hotkeys use.
    pub const CONTROL_ALT_SHIFT: Self = Self {
        control: true,
        alt: true,
        shift: true,
        gui: false,
    };

    pub fn contains(self, modifier: Modifier) -> bool {
        match modifier {
            Modifier::Control => self.control,
            Modifier::Alt => self.alt,
            Modifier::Shift => self.shift,
            Modifier::Gui => self.gui,
        }
    }

    pub fn with(mut self, modifier: Modifier) -> Self {
        match modifier {
            Modifier::Control => self.control = true,
            Modifier::Alt => self.alt = true,
            Modifier::Shift => self.shift = true,
            Modifier::Gui => self.gui = true,
        }
        self
    }

    pub fn is_empty(self) -> bool {
        self == Self::NONE
    }

    /// Whether every modifier in `other` is in this set too.
    pub fn includes(self, other: Modifiers) -> bool {
        (self.control || !other.control)
            && (self.alt || !other.alt)
            && (self.shift || !other.shift)
            && (self.gui || !other.gui)
    }

    /// The modifiers in the set, in the order they are written.
    pub fn iter(self) -> impl Iterator<Item = Modifier> {
        Modifier::ALL.into_iter().filter(move |m| self.contains(*m))
    }

    /// Whether every modifier in the set is down, as `held` reports.
    /// Others may be down too.
    pub fn are_held(self, held: impl Fn(HidUsage) -> bool) -> bool {
        self.iter().all(|m| m.is_held(&held))
    }
}

impl fmt::Display for Modifiers {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (i, m) in self.iter().enumerate() {
            if i > 0 {
                f.write_str("+")?;
            }
            f.write_str(m.name())?;
        }
        Ok(())
    }
}

impl FromStr for Modifiers {
    type Err = HotkeyError;

    /// Modifiers alone, such as `control+alt+shift`; the empty string is
    /// none.
    fn from_str(s: &str) -> Result<Self, HotkeyError> {
        let s = s.trim();
        if s.is_empty() {
            return Ok(Self::NONE);
        }
        s.split('+')
            .try_fold(Self::NONE, |set, part| Ok(set.with(part.parse()?)))
    }
}

/// A key combination recognised on key-down of `key` while its modifiers
/// are held (either the left or right key counts). Other modifiers may be
/// held too.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Hotkey {
    pub modifiers: Modifiers,
    pub key: HidUsage,
}

impl Hotkey {
    /// Whether this hotkey fires on every press that fires `other`: the
    /// same key, and no modifier of its own that `other` lacks.
    pub fn shadows(self, other: Hotkey) -> bool {
        self.key == other.key && other.modifiers.includes(self.modifiers)
    }

    /// Whether `key` is one typing never uses, which taking control back
    /// and the lock may be on their own, without modifiers: Scroll Lock,
    /// Pause and F13 to F19 (F14 on a Mac, where Apple's extended keyboard
    /// has Scroll Lock).
    pub fn works_alone(key: HidUsage) -> bool {
        key == usage::SCROLL_LOCK || key == usage::PAUSE || (usage::F13..=usage::F19).contains(&key)
    }

    pub const fn new(modifiers: Modifiers, key: HidUsage) -> Self {
        Self { modifiers, key }
    }

    /// Whether pressing `key` triggers this hotkey, given which keys `held`
    /// reports as down.
    pub fn matches(&self, key: HidUsage, held: impl Fn(HidUsage) -> bool) -> bool {
        key == self.key && self.modifiers.are_held(held)
    }
}

impl fmt::Display for Hotkey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if !self.modifiers.is_empty() {
            write!(f, "{}+", self.modifiers)?;
        }
        match self.key.name() {
            Some(name) => f.write_str(name),
            None => write!(f, "{:#04x}", self.key.id()),
        }
    }
}

impl FromStr for Hotkey {
    type Err = HotkeyError;

    /// Modifiers and a key, such as `control+alt+shift+KeyL`, or a key
    /// alone. The key cannot be a modifier itself. Which hotkeys may go
    /// without modifiers is for `Hotkeys::check` to say.
    fn from_str(s: &str) -> Result<Self, HotkeyError> {
        let s = s.trim();
        let (modifiers, key) = match s.rsplit_once('+') {
            Some((modifiers, key)) => (modifiers.parse::<Modifiers>()?, key.trim()),
            None => (Modifiers::NONE, s),
        };
        let key = HidUsage::from_name(key)
            .filter(|k| !k.is_modifier())
            .ok_or_else(|| HotkeyError::UnknownKey(key.to_owned()))?;
        Ok(Self { modifiers, key })
    }
}

/// Why hotkeys cannot be used.
#[derive(Clone, Debug, thiserror::Error, PartialEq, Eq)]
pub enum HotkeyError {
    #[error("{0:?} needs at least one modifier")]
    NeedsModifier(String),
    /// A key or modifier name that is not one, or a modifier where the
    /// key goes.
    #[error("{0:?} is not a key or modifier a hotkey can use")]
    UnknownKey(String),
    /// A hotkey on a key this computer's keyboard never reports, so it
    /// could never be pressed (`Hotkeys::check_keyboard`).
    #[error("{0:?} is on a key this computer never reports")]
    NotReported(String),
    /// Text where the double press goes that is neither `double+control`
    /// nor empty.
    #[error("{0:?} is no double press InputWeave knows; only double+control is")]
    UnknownDouble(String),
    /// Two hotkeys are the same keys: the two named.
    #[error("the {0} and {1} hotkeys are the same keys")]
    Duplicate(&'static str, &'static str),
}

/// What `HotkeyError::Duplicate` calls the relative mode hotkey.
const RELATIVE: &str = "relative";

impl HotkeyError {
    /// The error code clients are told.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NeedsModifier(_) => "needs_modifier",
            Self::UnknownKey(_) | Self::NotReported(_) | Self::UnknownDouble(_) => "unknown_key",
            // Its own code, so an app can name that hotkey; one from before
            // it shows its usual message.
            Self::Duplicate(a, b) if *a == RELATIVE || *b == RELATIVE => "duplicate_relative",
            Self::Duplicate(..) => "duplicate",
        }
    }
}

/// Every hotkey, as the user set them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Hotkeys {
    /// Returns control to this machine. Never forwarded to a remote device.
    pub take_back: Hotkey,
    /// Locks the cursor to the device it is on, or unlocks it. Never
    /// forwarded to a remote device.
    pub lock: Hotkey,
    /// With the digits 1 to 9, moves the cursor to the device with that
    /// number. None turns them off. Never forwarded to a remote device.
    pub jump: Option<Modifiers>,
    /// Turns relative mode on or off for the remote device the cursor is
    /// on: its movements go there as such, for games that hold the cursor.
    /// Not forwarded to a device it puts in or out of relative mode. None
    /// turns it off.
    pub relative: Option<Hotkey>,
    /// Control pressed and let go of twice while controlling a remote
    /// device, with nothing else between and within
    /// `DOUBLE_PRESS_WITHIN`, returns control to this machine too. Both
    /// presses must be of the same Control key, left or right. Unlike the
    /// hotkeys, they still go to the device. Off by default.
    pub take_back_double: bool,
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            take_back: Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::ESCAPE),
            lock: Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::L),
            jump: Some(Modifiers::CONTROL_ALT_SHIFT),
            relative: Some(Self::DEFAULT_RELATIVE),
            take_back_double: false,
        }
    }
}

impl Hotkeys {
    /// The relative mode hotkey unless the user chose another.
    pub const DEFAULT_RELATIVE: Hotkey = Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::G);

    /// Reads hotkeys as they are written; `jump_modifiers` empty turns
    /// jumping by number off, and `relative` empty turns that hotkey off.
    /// `relative` not given at all, as in a devices file from before it, is
    /// its default, unless that is the same keys as one of the others: then
    /// it is off and the others stay. `take_back_double` is
    /// `double+control` (`DOUBLE_CONTROL`) or empty for off. Two hotkeys
    /// that are the same keys, counting each jump modifier with each digit,
    /// are refused, and so are those `check` refuses.
    pub fn parse(
        take_back: &str,
        lock: &str,
        jump_modifiers: &str,
        relative: Option<&str>,
        take_back_double: &str,
    ) -> Result<Self, HotkeyError> {
        let given = relative.map(read_relative).transpose()?;
        let mut hotkeys = Self::read(
            take_back,
            lock,
            jump_modifiers,
            given.unwrap_or(Some(Self::DEFAULT_RELATIVE)),
            double_press(take_back_double)?,
        )?;
        if given.is_none() && hotkeys.check().is_err() {
            hotkeys.relative = None;
        }
        hotkeys.check()?;
        Ok(hotkeys)
    }

    /// Reads hotkeys as a client sends them, to replace `current`. The
    /// relative mode hotkey and the double press left out (`None`), as by
    /// clients from before them, stay as in `current`; given, they are
    /// read as in `parse`. Refused as `check` refuses them.
    pub fn parse_keeping(
        current: &Hotkeys,
        take_back: &str,
        lock: &str,
        jump_modifiers: &str,
        relative: Option<&str>,
        take_back_double: Option<&str>,
    ) -> Result<Self, HotkeyError> {
        let relative = match relative {
            Some(text) => read_relative(text)?,
            None => current.relative,
        };
        let take_back_double = match take_back_double {
            Some(text) => double_press(text)?,
            None => current.take_back_double,
        };
        let hotkeys = Self::read(take_back, lock, jump_modifiers, relative, take_back_double)?;
        hotkeys.check()?;
        Ok(hotkeys)
    }

    /// The hotkeys written as text, with relative mode's and the double
    /// press already read; not checked.
    fn read(
        take_back: &str,
        lock: &str,
        jump_modifiers: &str,
        relative: Option<Hotkey>,
        take_back_double: bool,
    ) -> Result<Self, HotkeyError> {
        let jump = jump_modifiers.parse::<Modifiers>()?;
        Ok(Self {
            take_back: take_back.parse()?,
            lock: lock.parse()?,
            jump: (!jump.is_empty()).then_some(jump),
            relative,
            take_back_double,
        })
    }

    /// Refuses hotkeys without modifiers, but for taking control back and
    /// the lock on a key that works alone (`Hotkey::works_alone`); and a
    /// hotkey that could never fire because one looked at before it (the
    /// lock, then the jumps, then taking back, then relative mode) matches
    /// whenever it does: the same key, with the earlier one's modifiers
    /// among its own, since modifiers held beyond a hotkey's own do not
    /// stop it (`Hotkey::matches`). A key alone has no modifiers, so it
    /// shadows every hotkey on its key that is looked at after it.
    pub fn check(&self) -> Result<(), HotkeyError> {
        let bare = [
            (Some(self.take_back), true),
            (Some(self.lock), true),
            (self.relative, false),
        ];
        for (hotkey, may_be_alone) in bare {
            if let Some(hotkey) = hotkey
                && hotkey.modifiers.is_empty()
                && !(may_be_alone && Hotkey::works_alone(hotkey.key))
            {
                return Err(HotkeyError::NeedsModifier(hotkey.to_string()));
            }
        }
        if let Some(relative) = self.relative {
            if self.lock.shadows(relative) {
                return Err(HotkeyError::Duplicate(RELATIVE, "lock"));
            }
            if self
                .jump_hotkey_for(relative.key)
                .is_some_and(|jump| jump.shadows(relative))
            {
                return Err(HotkeyError::Duplicate("jump", RELATIVE));
            }
            if self.take_back.shadows(relative) {
                return Err(HotkeyError::Duplicate(RELATIVE, "take-back"));
            }
        }
        if self.lock.shadows(self.take_back) {
            return Err(HotkeyError::Duplicate("take-back", "lock"));
        }
        if self
            .jump_hotkey_for(self.lock.key)
            .is_some_and(|jump| self.lock.shadows(jump))
        {
            return Err(HotkeyError::Duplicate("jump", "lock"));
        }
        if self
            .jump_hotkey_for(self.take_back.key)
            .is_some_and(|jump| jump.shadows(self.take_back))
        {
            return Err(HotkeyError::Duplicate("jump", "take-back"));
        }
        Ok(())
    }

    /// Refuses a hotkey on a key this computer's keyboard never reports, as
    /// `reports` says, which could never fire: a Mac has no key code for
    /// Scroll Lock or Pause (Apple's extended keyboard has F14 where Scroll
    /// Lock is), so a take-back hotkey on one would be lost. The jumps are
    /// on the digits, which every keyboard has.
    pub fn check_keyboard(&self, reports: impl Fn(HidUsage) -> bool) -> Result<(), HotkeyError> {
        let hotkeys = [Some(self.take_back), Some(self.lock), self.relative];
        match hotkeys.into_iter().flatten().find(|h| !reports(h.key)) {
            Some(hotkey) => Err(HotkeyError::NotReported(hotkey.to_string())),
            None => Ok(()),
        }
    }

    /// These hotkeys with each one `check_keyboard` refuses replaced, every
    /// other as it is, by the first choice that clashes with none
    /// (`check`): its own default, then the other defaults (the lock's or
    /// the take-back's, then relative mode's), and for relative mode off.
    /// Also which were replaced, named as in `HotkeyError::Duplicate`. For
    /// hotkeys stored before on such a key (Scroll Lock or Pause, set on a
    /// PC and read on a Mac). A hotkey kept is on one key, so it clashes
    /// with one of the three defaults at most, and a choice that clashes
    /// with none is always found; were there none, the first one tried is
    /// returned, which `check` refuses.
    pub fn reported_only(self, reports: impl Fn(HidUsage) -> bool) -> (Hotkeys, Vec<&'static str>) {
        let defaults = Hotkeys::default();
        let mut replaced = Vec::new();
        let mut choices = |name: &'static str, hotkey: Hotkey, own: Hotkey, others: [Hotkey; 2]| {
            if reports(hotkey.key) {
                vec![hotkey]
            } else {
                replaced.push(name);
                vec![own, others[0], others[1]]
            }
        };
        let take_backs = choices(
            "take-back",
            self.take_back,
            defaults.take_back,
            [defaults.lock, Self::DEFAULT_RELATIVE],
        );
        let locks = choices(
            "lock",
            self.lock,
            defaults.lock,
            [defaults.take_back, Self::DEFAULT_RELATIVE],
        );
        let relatives = match self.relative {
            Some(relative) if !reports(relative.key) => {
                replaced.push(RELATIVE);
                vec![Some(Self::DEFAULT_RELATIVE), None]
            }
            relative => vec![relative],
        };
        let mut tries = Vec::new();
        for &take_back in &take_backs {
            for &lock in &locks {
                for &relative in &relatives {
                    tries.push(Hotkeys {
                        take_back,
                        lock,
                        relative,
                        ..self
                    });
                }
            }
        }
        let chosen = tries
            .iter()
            .find(|h| h.check().is_ok() && h.check_keyboard(&reports).is_ok())
            .unwrap_or(&tries[0]);
        (*chosen, replaced)
    }

    /// The number of the device to jump to that pressing `key` asks for,
    /// given which keys `held` reports as down.
    pub fn jump_slot(&self, key: HidUsage, held: impl Fn(HidUsage) -> bool) -> Option<u8> {
        let modifiers = self.jump?;
        let slot = digit_slot(key)?;
        modifiers.are_held(held).then_some(slot)
    }

    /// The hotkey that jumps to device number `slot`, from 1.
    pub fn jump_hotkey(&self, slot: u8) -> Option<Hotkey> {
        let modifiers = self.jump?;
        if !(1..=JUMP_SLOTS).contains(&slot) {
            return None;
        }
        let key = HidUsage::new(usage::DIGIT1.id() + u16::from(slot) - 1);
        Some(Hotkey::new(modifiers, key))
    }

    /// The hotkeys capture keeps from local apps even while input is not
    /// grabbed: the lock and the jumps. The take-back and relative mode
    /// hotkeys matter only while input is grabbed, when nothing reaches
    /// local apps anyway.
    pub fn reserved(&self) -> Vec<Hotkey> {
        let mut reserved = vec![self.lock];
        reserved.extend((1..=JUMP_SLOTS).filter_map(|slot| self.jump_hotkey(slot)));
        reserved
    }

    /// The jump hotkey on `key`, if it is one of the digits and jumps are on.
    fn jump_hotkey_for(&self, key: HidUsage) -> Option<Hotkey> {
        self.jump_hotkey(digit_slot(key)?)
    }

    /// `take_back_double` as it is written: `DOUBLE_CONTROL`, or empty
    /// while off.
    pub fn take_back_double_text(&self) -> &'static str {
        if self.take_back_double {
            DOUBLE_CONTROL
        } else {
            ""
        }
    }
}

/// The relative mode hotkey written as `text`; none when it is empty.
fn read_relative(text: &str) -> Result<Option<Hotkey>, HotkeyError> {
    let text = text.trim();
    if text.is_empty() {
        Ok(None)
    } else {
        text.parse().map(Some)
    }
}

/// Whether `text` turns the double press on: `double+control` in any case,
/// or empty for off.
fn double_press(text: &str) -> Result<bool, HotkeyError> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(false);
    }
    let parts: Vec<&str> = text.split('+').map(str::trim).collect();
    match parts[..] {
        [double, modifier]
            if double.eq_ignore_ascii_case("double")
                && Modifier::from_name(modifier) == Some(Modifier::Control) =>
        {
            Ok(true)
        }
        _ => Err(HotkeyError::UnknownDouble(text.to_owned())),
    }
}

/// Watches for the double press that takes control back
/// (`Hotkeys::take_back_double`): a Control key pressed and let go of twice,
/// the same key both times, each press with nothing else held, nothing else
/// pressed or let go of between, the second release within
/// `DOUBLE_PRESS_WITHIN` of the first press. A press too slow for the one
/// before may be the first of the next two. The caller passes every key
/// event with its time, and calls `interrupt` for anything else that starts
/// it over, such as a mouse button or scrolling.
#[derive(Clone, Copy, Debug, Default)]
pub struct DoublePressWatch {
    /// The Control key down now, and when it went down.
    down: Option<(HidUsage, Instant)>,
    /// The Control key pressed and let go of before, and when it went
    /// down: the first of the two.
    first: Option<(HidUsage, Instant)>,
}

impl DoublePressWatch {
    /// Whether `key` going down or up at `now` completes the double press,
    /// given the keys `held` down before it (`key` among them if this is an
    /// auto-repeat) and whether a mouse button is held: a press counts only
    /// with nothing else down.
    pub fn key(
        &mut self,
        key: HidUsage,
        pressed: bool,
        held: impl IntoIterator<Item = HidUsage>,
        buttons_held: bool,
        now: Instant,
    ) -> bool {
        let (mut down, mut others) = (false, false);
        for held in held {
            if held == key {
                down = true;
            } else {
                others = true;
            }
        }
        let repeat = pressed && down;
        let alone = !others && !buttons_held;
        if Modifier::of(key) != Some(Modifier::Control) {
            self.interrupt();
            return false;
        }
        if pressed {
            if repeat {
                return false;
            }
            if !alone {
                self.interrupt();
                return false;
            }
            self.down = Some((key, now));
            // The other Control key does not make the second press.
            if self.first.is_some_and(|(first, _)| first != key) {
                self.first = None;
            }
            return false;
        }
        let Some((key, at)) = self.down.take().filter(|(down, _)| *down == key) else {
            // Its press was not one of these.
            self.interrupt();
            return false;
        };
        if let Some((_, first)) = self.first.take()
            && now.saturating_duration_since(first) <= DOUBLE_PRESS_WITHIN
        {
            return true;
        }
        self.first = Some((key, at));
        false
    }

    /// Starts over.
    pub fn interrupt(&mut self) {
        *self = Self::default();
    }
}

/// 1 to 9 for the digit keys 1 to 9 above the letters.
fn digit_slot(key: HidUsage) -> Option<u8> {
    let slot = key.id().checked_sub(usage::DIGIT1.id())? + 1;
    u8::try_from(slot).ok().filter(|s| *s <= JUMP_SLOTS)
}

#[cfg(test)]
mod tests {
    use super::*;

    const CONTROL_ALT: Modifiers = Modifiers {
        control: true,
        alt: true,
        shift: false,
        gui: false,
    };

    #[test]
    fn hotkeys_read_and_write_as_modifiers_and_a_key() {
        let lock: Hotkey = "control+alt+shift+KeyL".parse().unwrap();
        assert_eq!(lock, Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::L));
        assert_eq!(lock.to_string(), "control+alt+shift+KeyL");
        // Any order and case, written back in the usual one.
        let read: Hotkey = " Shift+GUI+control+escape ".parse().unwrap();
        assert_eq!(read.to_string(), "control+shift+gui+Escape");
        assert_eq!(
            "alt+F5".parse::<Hotkey>().unwrap(),
            Hotkey::new(
                Modifiers::NONE.with(Modifier::Alt),
                HidUsage::from_name("F5").unwrap()
            )
        );
        assert_eq!(
            Hotkeys::default().take_back.to_string(),
            "control+alt+shift+Escape"
        );
    }

    #[test]
    fn hotkeys_need_a_key_that_is_not_a_modifier() {
        let error = |s: &str| s.parse::<Hotkey>().unwrap_err();
        // A key alone reads; whether it may be used is for `check`.
        assert_eq!(
            "KeyL".parse::<Hotkey>().unwrap(),
            Hotkey::new(Modifiers::NONE, usage::L)
        );
        assert_eq!(
            Hotkeys {
                lock: "KeyL".parse().unwrap(),
                ..Hotkeys::default()
            }
            .check(),
            Err(HotkeyError::NeedsModifier("KeyL".to_owned()))
        );
        assert_eq!(
            error("control+Kay"),
            HotkeyError::UnknownKey("Kay".to_owned())
        );
        assert_eq!(
            error("ctrl+KeyL"),
            HotkeyError::UnknownKey("ctrl".to_owned())
        );
        assert_eq!(error("control+ShiftLeft").code(), "unknown_key");
        assert_eq!(error("control+alt").code(), "unknown_key");
        assert_eq!(error("control+").code(), "unknown_key");
        assert_eq!(error("").code(), "unknown_key");
    }

    #[test]
    fn jump_modifiers_are_modifiers_alone_and_empty_turns_them_off() {
        assert_eq!("control+alt".parse::<Modifiers>().unwrap(), CONTROL_ALT);
        assert_eq!(CONTROL_ALT.to_string(), "control+alt");
        assert_eq!("".parse::<Modifiers>().unwrap(), Modifiers::NONE);
        assert_eq!(Modifiers::NONE.to_string(), "");
        assert_eq!(
            "control+KeyL".parse::<Modifiers>().unwrap_err(),
            HotkeyError::UnknownKey("KeyL".to_owned())
        );
        let off = Hotkeys::parse(
            "control+alt+shift+Escape",
            "control+alt+shift+KeyL",
            "",
            None,
            "",
        )
        .unwrap();
        assert_eq!(off.jump, None);
        assert_eq!(off.reserved(), vec![off.lock]);
    }

    #[test]
    fn the_same_keys_cannot_do_two_things() {
        let parse = |t, l, j| {
            Hotkeys::parse(t, l, j, None, "")
                .map(|_| ())
                .map_err(|e| e.code())
        };
        assert_eq!(parse("control+KeyT", "control+KeyL", "control+alt"), Ok(()));
        assert_eq!(
            Hotkeys::parse("control+KeyL", "control+KeyL", "", None, ""),
            Err(HotkeyError::Duplicate("take-back", "lock"))
        );
        // A jump modifier with a digit is one of the jumps.
        assert_eq!(
            Hotkeys::parse(
                "control+alt+Digit3",
                "control+KeyL",
                "alt+control",
                None,
                ""
            ),
            Err(HotkeyError::Duplicate("jump", "take-back"))
        );
        assert_eq!(
            parse("control+KeyT", "control+alt+Digit9", "control+alt"),
            Err("duplicate")
        );
        // Other modifiers held do not stop a hotkey, so one whose modifiers
        // are among another's on the same key would fire first every time.
        assert_eq!(
            Hotkeys::parse("control+alt+KeyL", "control+KeyL", "", None, ""),
            Err(HotkeyError::Duplicate("take-back", "lock"))
        );
        assert_eq!(
            parse("control+KeyT", "control+Digit1", "control+alt+shift"),
            Err("duplicate")
        );
        assert_eq!(
            Hotkeys::parse(
                "control+alt+shift+Digit1",
                "control+KeyL",
                "control+alt",
                None,
                ""
            ),
            Err(HotkeyError::Duplicate("jump", "take-back"))
        );
        // The other way round both still fire: ⌃L takes back, ⌃⌥L locks.
        assert_eq!(parse("control+KeyL", "control+alt+KeyL", ""), Ok(()));
        assert_eq!(
            parse("control+KeyT", "control+alt+shift+Digit1", "control+alt"),
            Ok(())
        );
        // Digit 0 is no jump, nor are other modifiers.
        assert_eq!(
            parse("control+alt+Digit0", "control+KeyL", "control+alt"),
            Ok(())
        );
        assert_eq!(
            parse("control+Digit1", "control+KeyL", "control+alt"),
            Ok(())
        );
        // Each mistake is told as such.
        assert_eq!(parse("KeyT", "control+KeyL", ""), Err("needs_modifier"));
        assert_eq!(
            parse("control+KeyT", "control+KeyL", "hyper"),
            Err("unknown_key")
        );
    }

    #[test]
    fn relative_mode_has_its_own_hotkey_that_cannot_be_hidden() {
        let defaults = Hotkeys::default();
        assert_eq!(defaults.relative, Some(Hotkeys::DEFAULT_RELATIVE));
        assert_eq!(
            Hotkeys::DEFAULT_RELATIVE.to_string(),
            "control+alt+shift+KeyG"
        );
        let parse = |relative: Option<&str>| {
            Hotkeys::parse(
                "control+alt+shift+Escape",
                "control+alt+shift+KeyL",
                "control+alt+shift",
                relative,
                "",
            )
        };
        // Not given is the default; empty turns it off.
        assert_eq!(parse(None).unwrap(), defaults);
        assert_eq!(parse(Some(" ")).unwrap().relative, None);
        assert_eq!(
            parse(Some("control+alt+KeyR"))
                .unwrap()
                .relative
                .map(|h| h.to_string()),
            Some("control+alt+KeyR".to_owned())
        );
        let refused = |relative: &str| parse(Some(relative)).map(|_| ());
        assert_eq!(
            refused("control+alt+shift+KeyL"),
            Err(HotkeyError::Duplicate("relative", "lock"))
        );
        assert_eq!(
            refused("control+alt+shift+Escape"),
            Err(HotkeyError::Duplicate("relative", "take-back"))
        );
        assert_eq!(
            refused("control+alt+shift+Digit2"),
            Err(HotkeyError::Duplicate("jump", "relative"))
        );
        // Clashes with it have a code of their own; others keep theirs.
        assert_eq!(
            refused("control+alt+shift+KeyL").map_err(|e| e.code()),
            Err("duplicate_relative")
        );
        assert_eq!(
            HotkeyError::Duplicate("take-back", "lock").code(),
            "duplicate"
        );
        assert_eq!(refused("KeyG").map_err(|e| e.code()), Err("needs_modifier"));
        assert_eq!(
            refused("control+alt+Bogus").map_err(|e| e.code()),
            Err("unknown_key")
        );
        // On another key, or off, its default is free for the others.
        for relative in [Some("control+alt+shift+KeyR"), Some("")] {
            let lock_on_g = Hotkeys::parse(
                "control+alt+shift+Escape",
                "control+alt+shift+KeyG",
                "",
                relative,
                "",
            )
            .unwrap();
            assert_eq!(lock_on_g.lock.to_string(), "control+alt+shift+KeyG");
        }
        // Not kept from local apps: it only matters while input is grabbed.
        assert!(!defaults.reserved().contains(&Hotkeys::DEFAULT_RELATIVE));
    }

    /// Hotkeys saved before relative mode keep working: where its default
    /// would clash with them, it is off rather than all of them reset.
    #[test]
    fn hotkeys_from_before_relative_mode_stay_as_they_were() {
        let saved = Hotkeys::parse(
            "control+alt+shift+Escape",
            "control+alt+shift+KeyG",
            "control+alt+shift",
            None,
            "",
        )
        .unwrap();
        assert_eq!(saved.lock.to_string(), "control+alt+shift+KeyG");
        assert_eq!(saved.relative, None);
        // Without a clash it is the default, as for anyone else.
        let saved =
            Hotkeys::parse("control+alt+shift+Escape", "control+KeyK", "", None, "").unwrap();
        assert_eq!(saved.relative, Some(Hotkeys::DEFAULT_RELATIVE));
        // Hotkeys that cannot be used are still refused.
        assert_eq!(
            Hotkeys::parse("control+KeyL", "control+KeyL", "", None, ""),
            Err(HotkeyError::Duplicate("take-back", "lock"))
        );
    }

    #[test]
    fn jumps_go_by_the_digit_with_their_modifiers_held() {
        let hotkeys = Hotkeys::default();
        let digit = |n: u16| HidUsage::new(usage::DIGIT1.id() + n - 1);
        let held = [usage::LEFT_CONTROL, usage::RIGHT_ALT, usage::LEFT_SHIFT];
        let with = |keys: &'static [HidUsage]| move |k: HidUsage| keys.contains(&k);
        assert_eq!(hotkeys.jump_slot(digit(1), |k| held.contains(&k)), Some(1));
        assert_eq!(hotkeys.jump_slot(digit(9), |k| held.contains(&k)), Some(9));
        // Digit 0, other keys, and a modifier short.
        assert_eq!(
            hotkeys.jump_slot(HidUsage::new(0x27), |k| held.contains(&k)),
            None
        );
        assert_eq!(hotkeys.jump_slot(usage::L, |k| held.contains(&k)), None);
        assert_eq!(
            hotkeys.jump_slot(digit(2), with(&[usage::LEFT_CONTROL, usage::LEFT_ALT])),
            None
        );
        assert_eq!(
            hotkeys.jump_hotkey(2),
            Some(Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, digit(2)))
        );
        assert_eq!(hotkeys.jump_hotkey(0), None);
        assert_eq!(hotkeys.jump_hotkey(10), None);
        let reserved = hotkeys.reserved();
        assert_eq!(reserved.len(), 10);
        assert_eq!(reserved[0], hotkeys.lock);
        assert!(!reserved.contains(&hotkeys.take_back));

        let off = Hotkeys {
            jump: None,
            ..hotkeys
        };
        assert_eq!(off.jump_slot(digit(1), |k| held.contains(&k)), None);
    }

    #[test]
    fn modifiers_count_either_side() {
        assert!(Modifier::Shift.is_held(|k| k == usage::RIGHT_SHIFT));
        assert!(!Modifier::Shift.is_held(|k| k == usage::RIGHT_ALT));
        assert_eq!(Modifier::of(usage::RIGHT_GUI), Some(Modifier::Gui));
        assert_eq!(Modifier::of(usage::L), None);
        assert_eq!("Shift".parse::<Modifier>().unwrap(), Modifier::Shift);
        let hotkey = Hotkey::new(CONTROL_ALT, usage::L);
        assert!(hotkey.matches(usage::L, |k| {
            [usage::RIGHT_CONTROL, usage::LEFT_ALT].contains(&k)
        }));
        assert!(!hotkey.matches(usage::L, |k| k == usage::LEFT_CONTROL));
        assert!(!hotkey.matches(usage::A, |k| {
            [usage::LEFT_CONTROL, usage::LEFT_ALT].contains(&k)
        }));
    }

    #[test]
    fn keys_typing_never_uses_work_alone() {
        let alone: Vec<&str> = (0..=0xffu16)
            .map(HidUsage::new)
            .filter(|k| Hotkey::works_alone(*k))
            .filter_map(HidUsage::name)
            .collect();
        assert_eq!(
            alone,
            [
                "ScrollLock",
                "Pause",
                "F13",
                "F14",
                "F15",
                "F16",
                "F17",
                "F18",
                "F19"
            ]
        );
        // Written and read back as the key alone.
        let f13: Hotkey = " f13 ".parse().unwrap();
        assert_eq!(f13, Hotkey::new(Modifiers::NONE, usage::F13));
        assert_eq!(f13.to_string(), "F13");
        assert_eq!(
            "ScrollLock".parse::<Hotkey>().unwrap().to_string(),
            "ScrollLock"
        );
    }

    #[test]
    fn taking_back_and_the_lock_may_be_a_key_alone_the_others_may_not() {
        let parse = |t, l, j, r| {
            Hotkeys::parse(t, l, j, Some(r), "")
                .map(|_| ())
                .map_err(|e| e.code())
        };
        let jump = "control+alt+shift";
        let relative = "control+alt+shift+KeyG";
        assert_eq!(
            parse("F13", "control+alt+shift+KeyL", jump, relative),
            Ok(())
        );
        assert_eq!(
            parse("control+alt+shift+Escape", "Pause", jump, relative),
            Ok(())
        );
        assert_eq!(parse("ScrollLock", "F19", jump, relative), Ok(()));
        let lock_alone = Hotkeys::parse("F14", "F15", jump, Some(relative), "").unwrap();
        assert_eq!(lock_alone.lock.to_string(), "F15");
        assert_eq!(lock_alone.reserved()[0], lock_alone.lock);
        // Other keys alone need a modifier, as F12 and F20 do.
        for key in ["KeyK", "F12", "F20", "Escape", "Space", "Digit1"] {
            assert_eq!(
                parse(key, "control+alt+shift+KeyL", jump, relative),
                Err("needs_modifier"),
                "{key}"
            );
            assert_eq!(
                parse("control+alt+shift+Escape", key, jump, relative),
                Err("needs_modifier"),
                "{key}"
            );
        }
        // Relative mode needs one whatever the key.
        assert_eq!(
            parse(
                "control+alt+shift+Escape",
                "control+alt+shift+KeyL",
                jump,
                "F13"
            ),
            Err("needs_modifier")
        );
        assert_eq!(
            parse(
                "control+alt+shift+Escape",
                "control+alt+shift+KeyL",
                jump,
                "alt+F13"
            ),
            Ok(())
        );
        // The jumps take modifiers alone, as before.
        assert_eq!(
            parse(
                "control+alt+shift+Escape",
                "control+alt+shift+KeyL",
                "F13",
                relative
            ),
            Err("unknown_key")
        );
    }

    /// A key alone fires whatever modifiers are held, so it shadows every
    /// hotkey on its key; one with modifiers on it does not shadow it.
    #[test]
    fn a_key_alone_cannot_hide_another_hotkey_on_it() {
        let parse =
            |t: &str, l: &str, r: &str| Hotkeys::parse(t, l, "control+alt+shift", Some(r), "");
        let relative = "control+alt+shift+KeyG";
        assert_eq!(
            parse("F13", "F13", relative),
            Err(HotkeyError::Duplicate("take-back", "lock"))
        );
        // The lock is looked at first: F13 alone locks whenever ⌃F13 is
        // pressed, so taking back on ⌃F13 never fires.
        assert_eq!(
            parse("control+F13", "F13", relative),
            Err(HotkeyError::Duplicate("take-back", "lock"))
        );
        // The other way round both fire: ⌃F13 locks, F13 takes back.
        let both = parse("F13", "control+F13", relative).unwrap();
        assert_eq!(
            (both.take_back.to_string(), both.lock.to_string()),
            ("F13".to_owned(), "control+F13".to_owned())
        );
        assert_eq!(
            parse("F13", "control+alt+shift+KeyL", "control+F13"),
            Err(HotkeyError::Duplicate("relative", "take-back"))
        );
        assert_eq!(
            parse("control+alt+shift+Escape", "Pause", "alt+Pause"),
            Err(HotkeyError::Duplicate("relative", "lock"))
        );
        assert_eq!(
            parse("control+alt+shift+Escape", "Pause", "alt+Pause").map_err(|e| e.code()),
            Err("duplicate_relative")
        );
    }

    #[test]
    fn the_double_press_reads_and_writes_as_double_control() {
        let parse = |double: &str| {
            Hotkeys::parse(
                "control+alt+shift+Escape",
                "control+alt+shift+KeyL",
                "control+alt+shift",
                None,
                double,
            )
        };
        assert!(!Hotkeys::default().take_back_double);
        assert_eq!(Hotkeys::default().take_back_double_text(), "");
        assert_eq!(parse("").unwrap(), Hotkeys::default());
        assert_eq!(parse(" ").unwrap(), Hotkeys::default());
        for on in ["double+control", " Double + CONTROL "] {
            let hotkeys = parse(on).unwrap();
            assert!(hotkeys.take_back_double, "{on}");
            assert_eq!(hotkeys.take_back_double_text(), DOUBLE_CONTROL);
        }
        assert_eq!(DOUBLE_CONTROL, "double+control");
        // Only Control, and only twice.
        for wrong in [
            "double+shift",
            "double+ctrl",
            "double",
            "control",
            "double+control+alt",
            "triple+control",
            "control+double",
            "on",
        ] {
            let error = parse(wrong).unwrap_err();
            assert_eq!(error, HotkeyError::UnknownDouble(wrong.trim().to_owned()));
            assert_eq!(error.code(), "unknown_key");
        }
        // It shares no keys with the hotkeys, so it clashes with none.
        let hotkeys =
            Hotkeys::parse("F13", "control+KeyL", "", Some(""), "double+control").unwrap();
        assert!(hotkeys.take_back_double);
        assert_eq!(hotkeys.reserved(), vec![hotkeys.lock]);
    }

    /// What a client leaves out stays; what it sends is read and checked
    /// as the rest.
    #[test]
    fn a_client_leaving_out_relative_mode_or_the_double_press_keeps_them() {
        let current = Hotkeys {
            relative: Some("control+alt+KeyR".parse().unwrap()),
            take_back_double: true,
            ..Hotkeys::default()
        };
        let keep = |relative: Option<&str>, double: Option<&str>| {
            Hotkeys::parse_keeping(
                &current,
                "F13",
                "control+alt+shift+KeyL",
                "control+alt+shift",
                relative,
                double,
            )
        };
        let kept = keep(None, None).unwrap();
        assert_eq!(kept.take_back.to_string(), "F13");
        assert_eq!(kept.relative, current.relative);
        assert!(kept.take_back_double);
        let changed = keep(Some(""), Some("")).unwrap();
        assert_eq!((changed.relative, changed.take_back_double), (None, false));
        assert_eq!(
            keep(Some("control+alt+KeyG"), None).unwrap().relative,
            Some("control+alt+KeyG".parse().unwrap())
        );
        // Refused as anything else is.
        assert_eq!(
            keep(Some("F14"), None).unwrap_err().code(),
            "needs_modifier"
        );
        assert_eq!(
            keep(None, Some("double+alt")).unwrap_err().code(),
            "unknown_key"
        );
        assert_eq!(
            Hotkeys::parse_keeping(&current, "KeyK", "control+KeyL", "", None, None)
                .unwrap_err()
                .code(),
            "needs_modifier"
        );
        // Relative mode kept as it was may clash with what is sent now.
        assert_eq!(
            Hotkeys::parse_keeping(&current, "control+alt+KeyR", "control+KeyL", "", None, None),
            Err(HotkeyError::Duplicate("relative", "take-back"))
        );
    }

    #[test]
    fn the_double_press_watch_starts_over_on_anything_else() {
        let start = Instant::now();
        let at = |ms: u64| start + Duration::from_millis(ms);
        let control = usage::LEFT_CONTROL;
        let none: [HidUsage; 0] = [];
        let tap = |watch: &mut DoublePressWatch, down: u64, up: u64| {
            watch.key(control, true, none, false, at(down));
            watch.key(control, false, [control], false, at(up))
        };
        let mut watch = DoublePressWatch::default();
        assert!(!tap(&mut watch, 0, 50));
        assert!(tap(&mut watch, 100, 150));
        // Started over after it: one more is a first press again.
        assert!(!tap(&mut watch, 200, 250));
        watch.interrupt();
        assert!(!tap(&mut watch, 300, 350));
        // Not alone (another key, or a button), or another key: no.
        for (held, buttons) in [(vec![usage::LEFT_SHIFT], false), (vec![], true)] {
            let mut watch = DoublePressWatch::default();
            tap(&mut watch, 0, 50);
            watch.key(control, true, held, buttons, at(80));
            assert!(!watch.key(control, false, [control], false, at(100)));
        }
        let mut watch = DoublePressWatch::default();
        tap(&mut watch, 0, 50);
        watch.key(usage::A, true, none, false, at(60));
        assert!(!tap(&mut watch, 80, 100));
        // A repeat is the same press, even with other keys down by then.
        let mut watch = DoublePressWatch::default();
        tap(&mut watch, 0, 50);
        watch.key(control, true, none, false, at(80));
        watch.key(control, true, [control], false, at(90));
        assert!(watch.key(control, false, [control], false, at(100)));
    }

    /// A computer whose keyboard never reports a key takes no hotkey on it.
    #[test]
    fn a_hotkey_on_a_key_the_keyboard_never_reports_is_refused() {
        // As on a Mac.
        let mac = |key: HidUsage| key.to_mac().is_some();
        let alone = |take_back: &str, lock: &str| Hotkeys {
            take_back: take_back.parse().unwrap(),
            lock: lock.parse().unwrap(),
            ..Hotkeys::default()
        };
        for key in ["ScrollLock", "Pause"] {
            assert_eq!(
                alone(key, "control+KeyL").check_keyboard(mac),
                Err(HotkeyError::NotReported(key.to_owned()))
            );
            assert_eq!(
                alone("F13", key).check_keyboard(mac).unwrap_err().code(),
                "unknown_key"
            );
            // With modifiers too.
            assert!(
                alone("F13", &format!("control+{key}"))
                    .check_keyboard(mac)
                    .is_err()
            );
        }
        let relative = Hotkeys {
            relative: Some("control+Pause".parse().unwrap()),
            ..Hotkeys::default()
        };
        assert!(relative.check_keyboard(mac).is_err());
        assert_eq!(alone("F13", "F14").check_keyboard(mac), Ok(()));
        assert_eq!(Hotkeys::default().check_keyboard(mac), Ok(()));
        // Where every key is reported, as on Windows and Linux.
        assert_eq!(
            alone("ScrollLock", "Pause").check_keyboard(|_| true),
            Ok(())
        );
    }

    #[test]
    fn a_held_set_told_before_another_does_not_replace_it() {
        let lock = Hotkeys::default().lock;
        let held: HeldHotkeys = [lock].into_iter().collect();
        let mut current = held.as_change(5);
        current.take(HeldHotkeys::default().as_change(4));
        assert_eq!(current, held);
        assert_eq!(current.change(), 5);
        current.take(HeldHotkeys::default().as_change(6));
        assert!(current.is_empty());
        // Unnumbered, as from a capture that never changes them: taken.
        let mut unnumbered = HeldHotkeys::default();
        unnumbered.take(held);
        assert_eq!(unnumbered, held);
        // Equal whatever the numbers.
        assert_eq!(held.as_change(1), held.as_change(2));
    }

    #[test]
    fn hotkeys_on_keys_the_keyboard_never_reports_alone_are_put_back() {
        let mac = |key: HidUsage| key.to_mac().is_some();
        let hotkeys = |take_back: &str, lock: &str, relative: Option<&str>| Hotkeys {
            take_back: take_back.parse().unwrap(),
            lock: lock.parse().unwrap(),
            jump: None,
            relative: relative.map(|r| r.parse().unwrap()),
            take_back_double: true,
        };
        // Relative mode's on Pause, as set on a PC: only it is put back.
        let (kept, put_back) =
            hotkeys("F13", "control+KeyK", Some("control+alt+Pause")).reported_only(mac);
        assert_eq!(
            kept,
            Hotkeys {
                relative: Some(Hotkeys::DEFAULT_RELATIVE),
                ..hotkeys("F13", "control+KeyK", None)
            }
        );
        assert_eq!(put_back, vec![RELATIVE]);
        assert_eq!(kept.check_keyboard(mac), Ok(()));
        // Its default the same keys as the lock: off.
        let (kept, put_back) =
            hotkeys("F13", "control+alt+shift+KeyG", Some("control+Pause")).reported_only(mac);
        assert_eq!(kept.relative, None);
        assert_eq!(kept.lock.to_string(), "control+alt+shift+KeyG");
        assert_eq!(put_back, vec![RELATIVE]);
        // A default the same keys as a hotkey kept is passed over, for the
        // next default: taking back on Scroll Lock, locking on the
        // take-back's default.
        let (kept, put_back) =
            hotkeys("ScrollLock", "control+alt+shift+Escape", Some("alt+KeyR")).reported_only(mac);
        assert_eq!(kept.take_back, Hotkeys::default().lock);
        assert_eq!(kept.lock.to_string(), "control+alt+shift+Escape");
        assert_eq!(kept.relative.unwrap().to_string(), "alt+KeyR");
        assert_eq!(put_back, vec!["take-back"]);
        assert_eq!(kept.check(), Ok(()));
        // Both on such keys, relative mode on the take-back's default:
        // the two left.
        let (kept, put_back) =
            hotkeys("Pause", "ScrollLock", Some("control+alt+shift+Escape")).reported_only(mac);
        assert_eq!(kept.take_back, Hotkeys::default().lock);
        assert_eq!(kept.lock, Hotkeys::DEFAULT_RELATIVE);
        assert_eq!(
            kept.relative.unwrap().to_string(),
            "control+alt+shift+Escape"
        );
        assert_eq!(put_back, vec!["take-back", "lock"]);
        assert_eq!(kept.check(), Ok(()));
        // Taking back and locking.
        let (kept, put_back) =
            hotkeys("ScrollLock", "control+Pause", Some("alt+KeyR")).reported_only(mac);
        assert_eq!(kept.take_back, Hotkeys::default().take_back);
        assert_eq!(kept.lock, Hotkeys::default().lock);
        assert_eq!(kept.relative.unwrap().to_string(), "alt+KeyR");
        assert!(kept.take_back_double);
        assert_eq!(put_back, vec!["take-back", "lock"]);
        // Nothing to put back, or where every key is reported.
        let all = hotkeys("ScrollLock", "Pause", Some("alt+KeyR"));
        assert_eq!(all.reported_only(|_| true), (all, Vec::new()));
        let (kept, put_back) = Hotkeys::default().reported_only(mac);
        assert_eq!((kept, put_back), (Hotkeys::default(), Vec::new()));
    }
}
