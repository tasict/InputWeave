//! Physical key codes as they travel between peers.
//!
//! Keys are identified by their USB HID usage ID on the Keyboard/Keypad page
//! (0x07), so the receiving machine interprets them with its own keyboard
//! layout and input method. The translations to and from native key codes
//! (macOS virtual key codes, Windows scan codes, Linux evdev codes) are
//! generated from Chromium's `dom_code_data.inc` (BSD-3-Clause) by
//! `cargo xtask keymap`.

use std::fmt;

mod table;

/// A usage ID on the USB HID Keyboard/Keypad page (0x07).
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct HidUsage(u16);

impl HidUsage {
    pub const fn new(id: u16) -> Self {
        Self(id)
    }

    pub const fn id(self) -> u16 {
        self.0
    }

    /// Whether this is one of the eight modifier keys (0xE0..=0xE7).
    pub const fn is_modifier(self) -> bool {
        self.0 >= 0xE0 && self.0 <= 0xE7
    }

    /// The macOS virtual key code (`kVK_*`).
    pub fn to_mac(self) -> Option<u16> {
        table::to_mac(self.0)
    }

    pub fn from_mac(code: u16) -> Option<Self> {
        table::from_mac(code).map(Self)
    }

    /// The Windows set-1 scan code.
    pub fn to_windows(self) -> Option<ScanCode> {
        table::to_windows(self.0).map(|(code, extended)| ScanCode { code, extended })
    }

    pub fn from_windows(scan: ScanCode) -> Option<Self> {
        table::from_windows(scan.code, scan.extended).map(Self)
    }

    /// The Linux evdev code (`KEY_*`).
    pub fn to_evdev(self) -> Option<u16> {
        table::to_evdev(self.0)
    }

    pub fn from_evdev(code: u16) -> Option<Self> {
        table::from_evdev(code).map(Self)
    }

    /// The UI Events `code` name, such as `"KeyA"` or `"ShiftLeft"`.
    pub fn name(self) -> Option<&'static str> {
        table::name(self.0)
    }

    /// The key whose UI Events `code` name is `name`, ignoring case.
    pub fn from_name(name: &str) -> Option<Self> {
        (0..=0xffu16)
            .map(Self)
            .find(|key| key.name().is_some_and(|n| n.eq_ignore_ascii_case(name)))
    }
}

/// A Windows set-1 scan code. Extended keys are sent with an `0xE0` prefix
/// (`KEYEVENTF_EXTENDEDKEY`, `LLKHF_EXTENDED`).
///
/// Pause is the exception the table cannot express: it arrives as scan code
/// `0x45` without the extended flag, the same as NumLock's code with it, and
/// cannot be injected by scan code at all; the Windows backend sends it by
/// virtual key instead. With Control held it arrives as Break, `0x46` with
/// the flag, which `from_windows` reads as Pause too (Control with Scroll
/// Lock sends the same on many keyboards).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ScanCode {
    pub code: u8,
    pub extended: bool,
}

impl fmt::Debug for HidUsage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "HidUsage({:#04x})", self.0)
    }
}

/// Usage IDs referenced by name in the codebase.
pub mod usage {
    use super::HidUsage;

    pub const A: HidUsage = HidUsage::new(0x04);
    pub const C: HidUsage = HidUsage::new(0x06);
    pub const G: HidUsage = HidUsage::new(0x0A);
    pub const L: HidUsage = HidUsage::new(0x0F);
    pub const V: HidUsage = HidUsage::new(0x19);
    /// The digit row: 1 is 0x1E through 9 at 0x26; 0 is 0x27.
    pub const DIGIT1: HidUsage = HidUsage::new(0x1E);
    pub const ESCAPE: HidUsage = HidUsage::new(0x29);
    pub const CAPS_LOCK: HidUsage = HidUsage::new(0x39);
    pub const SCROLL_LOCK: HidUsage = HidUsage::new(0x47);
    pub const PAUSE: HidUsage = HidUsage::new(0x48);
    /// Forward Delete (Del), not Backspace.
    pub const DELETE: HidUsage = HidUsage::new(0x4C);
    pub const NUM_LOCK: HidUsage = HidUsage::new(0x53);
    /// F13 to F24 follow one another from here. F14 is where Apple's
    /// extended keyboard has Scroll Lock.
    pub const F13: HidUsage = HidUsage::new(0x68);
    pub const F19: HidUsage = HidUsage::new(0x6E);

    pub const LEFT_CONTROL: HidUsage = HidUsage::new(0xE0);
    pub const LEFT_SHIFT: HidUsage = HidUsage::new(0xE1);
    pub const LEFT_ALT: HidUsage = HidUsage::new(0xE2);
    pub const LEFT_GUI: HidUsage = HidUsage::new(0xE3);
    pub const RIGHT_CONTROL: HidUsage = HidUsage::new(0xE4);
    pub const RIGHT_SHIFT: HidUsage = HidUsage::new(0xE5);
    pub const RIGHT_ALT: HidUsage = HidUsage::new(0xE6);
    pub const RIGHT_GUI: HidUsage = HidUsage::new(0xE7);
}

/// Per-peer modifier remapping applied before a key is sent to that peer.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ModifierRemap {
    /// Swap GUI (⌘ / Windows key) with Control, so ⌘C on a Mac arrives as
    /// Ctrl+C on Windows.
    pub swap_gui_and_control: bool,
}

impl ModifierRemap {
    pub fn apply(self, key: HidUsage) -> HidUsage {
        if !self.swap_gui_and_control {
            return key;
        }
        match key {
            usage::LEFT_GUI => usage::LEFT_CONTROL,
            usage::LEFT_CONTROL => usage::LEFT_GUI,
            usage::RIGHT_GUI => usage::RIGHT_CONTROL,
            usage::RIGHT_CONTROL => usage::RIGHT_GUI,
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn modifier_range() {
        assert!(usage::LEFT_CONTROL.is_modifier());
        assert!(usage::RIGHT_GUI.is_modifier());
        assert!(!usage::ESCAPE.is_modifier());
        assert!(!HidUsage::new(0xE8).is_modifier());
    }

    #[test]
    fn translates_well_known_keys() {
        assert_eq!(usage::A.to_mac(), Some(0x00));
        assert_eq!(
            usage::A.to_windows(),
            Some(ScanCode {
                code: 0x1e,
                extended: false
            })
        );
        assert_eq!(usage::A.to_evdev(), Some(30));
        assert_eq!(usage::A.name(), Some("KeyA"));

        // ⌘ / Windows key.
        assert_eq!(usage::LEFT_GUI.to_mac(), Some(0x37));
        assert_eq!(
            usage::LEFT_GUI.to_windows(),
            Some(ScanCode {
                code: 0x5b,
                extended: true
            })
        );
        assert_eq!(usage::LEFT_GUI.to_evdev(), Some(125));

        // NumLock is the extended form of Pause's scan code.
        assert_eq!(
            usage::NUM_LOCK.to_windows(),
            Some(ScanCode {
                code: 0x45,
                extended: true
            })
        );
        assert_eq!(
            usage::PAUSE.to_windows(),
            Some(ScanCode {
                code: 0x45,
                extended: false
            })
        );
        assert_eq!(usage::ESCAPE.to_mac(), Some(0x35));
        assert_eq!(
            HidUsage::from_mac(0x24).and_then(HidUsage::name),
            Some("Enter")
        );
    }

    #[test]
    fn keys_are_found_by_name_whatever_the_case() {
        assert_eq!(HidUsage::from_name("KeyL"), Some(usage::L));
        assert_eq!(HidUsage::from_name("escape"), Some(usage::ESCAPE));
        assert_eq!(HidUsage::from_name("Digit1"), Some(usage::DIGIT1));
        assert_eq!(HidUsage::from_name("ShiftLeft"), Some(usage::LEFT_SHIFT));
        assert_eq!(HidUsage::from_name("L"), None);
        assert_eq!(HidUsage::from_name(""), None);
        for id in 0..=0xffu16 {
            let key = HidUsage::new(id);
            if let Some(name) = key.name() {
                assert_eq!(HidUsage::from_name(name), Some(key), "{name}");
            }
        }
    }

    #[test]
    fn every_translation_round_trips() {
        let mut keys = 0;
        for id in 0..=0xffu16 {
            let key = HidUsage::new(id);
            if let Some(code) = key.to_mac() {
                assert_eq!(HidUsage::from_mac(code), Some(key), "mac {code:#x}");
                keys += 1;
            }
            if let Some(scan) = key.to_windows() {
                assert_eq!(HidUsage::from_windows(scan), Some(key), "win {scan:?}");
            }
            if let Some(code) = key.to_evdev() {
                assert_eq!(HidUsage::from_evdev(code), Some(key), "evdev {code:#x}");
            }
        }
        // Letters, digits, punctuation, function keys, keypad, modifiers.
        assert!(keys > 100, "only {keys} keys map to macOS");
        for modifier in 0xE0..=0xE7 {
            let key = HidUsage::new(modifier);
            assert!(key.to_mac().is_some() && key.to_windows().is_some());
        }
    }

    /// Break, what Pause sends with Control held, reads as Pause; Pause is
    /// sent as itself.
    #[test]
    fn break_reads_as_pause_on_windows() {
        let scan = |code, extended| ScanCode { code, extended };
        assert_eq!(HidUsage::from_windows(scan(0x46, true)), Some(usage::PAUSE));
        assert_eq!(
            HidUsage::from_windows(scan(0x45, false)),
            Some(usage::PAUSE)
        );
        assert_eq!(usage::PAUSE.to_windows(), Some(scan(0x45, false)));
        assert_eq!(
            HidUsage::from_windows(scan(0x46, false)),
            Some(usage::SCROLL_LOCK)
        );
    }

    #[test]
    fn remap_disabled_is_identity() {
        let remap = ModifierRemap::default();
        assert_eq!(remap.apply(usage::LEFT_GUI), usage::LEFT_GUI);
        assert_eq!(remap.apply(usage::C), usage::C);
    }

    #[test]
    fn remap_swaps_gui_and_control_on_both_sides() {
        let remap = ModifierRemap {
            swap_gui_and_control: true,
        };
        assert_eq!(remap.apply(usage::LEFT_GUI), usage::LEFT_CONTROL);
        assert_eq!(remap.apply(usage::LEFT_CONTROL), usage::LEFT_GUI);
        assert_eq!(remap.apply(usage::RIGHT_GUI), usage::RIGHT_CONTROL);
        assert_eq!(remap.apply(usage::RIGHT_CONTROL), usage::RIGHT_GUI);
        assert_eq!(remap.apply(usage::LEFT_SHIFT), usage::LEFT_SHIFT);
        assert_eq!(remap.apply(usage::C), usage::C);
    }
}
