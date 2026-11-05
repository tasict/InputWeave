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

use std::fmt;
use std::str::FromStr;

use iw_keymap::usage;

use crate::HidUsage;

/// How many devices can be jumped to by number: the digits 1 to 9.
pub const JUMP_SLOTS: u8 = 9;

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

    /// Modifiers and a key, such as `control+alt+shift+KeyL`. The key
    /// cannot be a modifier itself, and at least one modifier is needed.
    fn from_str(s: &str) -> Result<Self, HotkeyError> {
        let s = s.trim();
        let (modifiers, key) = match s.rsplit_once('+') {
            Some((modifiers, key)) => (modifiers.parse::<Modifiers>()?, key.trim()),
            None => (Modifiers::NONE, s),
        };
        let key = HidUsage::from_name(key)
            .filter(|k| !k.is_modifier())
            .ok_or_else(|| HotkeyError::UnknownKey(key.to_owned()))?;
        if modifiers.is_empty() {
            return Err(HotkeyError::NeedsModifier(s.to_owned()));
        }
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
            Self::UnknownKey(_) => "unknown_key",
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
}

impl Default for Hotkeys {
    fn default() -> Self {
        Self {
            take_back: Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::ESCAPE),
            lock: Hotkey::new(Modifiers::CONTROL_ALT_SHIFT, usage::L),
            jump: Some(Modifiers::CONTROL_ALT_SHIFT),
            relative: Some(Self::DEFAULT_RELATIVE),
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
    /// it is off and the others stay. Two hotkeys that are the same keys,
    /// counting each jump modifier with each digit, are refused.
    pub fn parse(
        take_back: &str,
        lock: &str,
        jump_modifiers: &str,
        relative: Option<&str>,
    ) -> Result<Self, HotkeyError> {
        let jump = jump_modifiers.parse::<Modifiers>()?;
        let mut hotkeys = Self {
            take_back: take_back.parse()?,
            lock: lock.parse()?,
            jump: (!jump.is_empty()).then_some(jump),
            relative: match relative.map(str::trim) {
                None => Some(Self::DEFAULT_RELATIVE),
                Some("") => None,
                Some(text) => Some(text.parse()?),
            },
        };
        if relative.is_none() && hotkeys.check().is_err() {
            hotkeys.relative = None;
        }
        hotkeys.check()?;
        Ok(hotkeys)
    }

    /// Refuses hotkeys without modifiers, and a hotkey that could never
    /// fire because one looked at before it (the lock, then the jumps, then
    /// taking back, then relative mode) matches whenever it does: the same
    /// key, with the earlier one's modifiers among its own, since modifiers
    /// held beyond a hotkey's own do not stop it (`Hotkey::matches`).
    pub fn check(&self) -> Result<(), HotkeyError> {
        let hotkeys = [Some(self.take_back), Some(self.lock), self.relative];
        for hotkey in hotkeys.into_iter().flatten() {
            if hotkey.modifiers.is_empty() {
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

    /// The device number `hotkey` jumps to, if it is one of the jumps.
    /// The jump hotkey on `key`, if it is one of the digits and jumps are on.
    fn jump_hotkey_for(&self, key: HidUsage) -> Option<Hotkey> {
        self.jump_hotkey(digit_slot(key)?)
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
    fn hotkeys_need_a_modifier_and_a_key_that_is_not_one() {
        let error = |s: &str| s.parse::<Hotkey>().unwrap_err();
        assert_eq!(error("KeyL"), HotkeyError::NeedsModifier("KeyL".to_owned()));
        assert_eq!(error("KeyL").code(), "needs_modifier");
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
        )
        .unwrap();
        assert_eq!(off.jump, None);
        assert_eq!(off.reserved(), vec![off.lock]);
    }

    #[test]
    fn the_same_keys_cannot_do_two_things() {
        let parse = |t, l, j| {
            Hotkeys::parse(t, l, j, None)
                .map(|_| ())
                .map_err(|e| e.code())
        };
        assert_eq!(parse("control+KeyT", "control+KeyL", "control+alt"), Ok(()));
        assert_eq!(
            Hotkeys::parse("control+KeyL", "control+KeyL", "", None),
            Err(HotkeyError::Duplicate("take-back", "lock"))
        );
        // A jump modifier with a digit is one of the jumps.
        assert_eq!(
            Hotkeys::parse("control+alt+Digit3", "control+KeyL", "alt+control", None),
            Err(HotkeyError::Duplicate("jump", "take-back"))
        );
        assert_eq!(
            parse("control+KeyT", "control+alt+Digit9", "control+alt"),
            Err("duplicate")
        );
        // Other modifiers held do not stop a hotkey, so one whose modifiers
        // are among another's on the same key would fire first every time.
        assert_eq!(
            Hotkeys::parse("control+alt+KeyL", "control+KeyL", "", None),
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
                None
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
        )
        .unwrap();
        assert_eq!(saved.lock.to_string(), "control+alt+shift+KeyG");
        assert_eq!(saved.relative, None);
        // Without a clash it is the default, as for anyone else.
        let saved = Hotkeys::parse("control+alt+shift+Escape", "control+KeyK", "", None).unwrap();
        assert_eq!(saved.relative, Some(Hotkeys::DEFAULT_RELATIVE));
        // Hotkeys that cannot be used are still refused.
        assert_eq!(
            Hotkeys::parse("control+KeyL", "control+KeyL", "", None),
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
}
