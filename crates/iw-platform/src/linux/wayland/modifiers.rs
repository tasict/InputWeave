//! The modifiers a virtual keyboard tells with its keys
//! (`zwp_virtual_keyboard_v1.modifiers`): wlroots passes a virtual keyboard's keys on as they come, without
//! working out from them which modifiers are down, latched or locked, so
//! the client must, from the keymap it gave the keyboard. Without it Shift,
//! Ctrl, Alt, Super and Caps Lock would do nothing in the apps.
//!
//! The keymap's state is kbvm's, a Rust implementation of XKB: what
//! xkbcommon works out for a keyboard of the compositor's own.

use kbvm::state_machine::{Direction, Event, State, StateMachine};
use kbvm::xkb::diagnostic::{Diagnostic, DiagnosticHandler, DiagnosticKind};
use kbvm::{Components, Keycode};

/// The modifiers and the layout, as `wl_keyboard.modifiers` tells them.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct Mods {
    pub depressed: u32,
    pub latched: u32,
    pub locked: u32,
    /// The layout in effect, as a keyboard of the compositor's own tells it
    /// (`XKB_STATE_LAYOUT_EFFECTIVE`): the request has one group, which
    /// the compositor takes as the one locked, so a layout held or latched
    /// is told as in effect, or it would be lost.
    pub group: u32,
}

/// A keymap's state as its keys go down and up.
pub(crate) struct Modifiers {
    machine: StateMachine,
    state: State,
    components: Components,
    events: Vec<Event>,
}

impl Modifiers {
    /// The state of `keymap`, XKB text as `wl_keyboard.keymap` gives it
    /// (ending in a NUL or not), with no key down. Includes are looked up
    /// in the system's xkb data.
    pub(crate) fn of(keymap: &[u8]) -> Result<Self, String> {
        let end = keymap.iter().rposition(|b| *b != 0).map_or(0, |i| i + 1);
        let keymap = kbvm::xkb::Context::default()
            .keymap_from_bytes(Quiet, None, &keymap[..end])
            .map_err(|e| e.to_string())?;
        let machine = keymap.to_builder().build_state_machine();
        Ok(Self {
            state: machine.create_state(),
            machine,
            components: Components::default(),
            events: Vec::new(),
        })
    }

    /// The evdev key `code` went down or up: the modifiers after, if that
    /// changed them.
    pub(crate) fn key(&mut self, code: u32, down: bool) -> Option<Mods> {
        let direction = if down { Direction::Down } else { Direction::Up };
        self.machine.handle_key(
            &mut self.state,
            &mut self.events,
            Keycode::from_evdev(code),
            direction,
        );
        let mut changed = false;
        for event in self.events.drain(..) {
            changed |= self.components.apply_event(event);
        }
        changed.then(|| self.mods())
    }

    /// The modifiers now.
    pub(crate) fn mods(&self) -> Mods {
        Mods {
            depressed: self.components.mods_pressed.0,
            latched: self.components.mods_latched.0,
            locked: self.components.mods_locked.0,
            group: self.components.group.0,
        }
    }
}

/// What kbvm says of a keymap as it reads it: only a failure matters,
/// which it returns.
struct Quiet;

impl DiagnosticHandler for Quiet {
    fn filter(&self, _: DiagnosticKind, _: bool) -> bool {
        false
    }

    fn handle(&mut self, _: Diagnostic) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Shift, Control, Alt and Caps Lock on the evdev keys, as xkbcommon
    /// writes a keymap out (no includes).
    const KEYMAP: &str = "xkb_keymap {
        xkb_keycodes {
            minimum = 8;
            maximum = 255;
            <AC01> = 38;
            <LFSH> = 50;
            <LCTL> = 37;
            <LALT> = 64;
            <CAPS> = 66;
        };
        xkb_types {
            virtual_modifiers Alt;
            type \"ONE_LEVEL\" {
                modifiers = none;
                level_name[Level1] = \"Any\";
            };
            type \"ALPHABETIC\" {
                modifiers = Shift+Lock;
                map[Shift] = Level2;
                map[Lock] = Level2;
                level_name[Level1] = \"Base\";
                level_name[Level2] = \"Caps\";
            };
        };
        xkb_compat {
            virtual_modifiers Alt;
            interpret Shift_L+AnyOf(all) {
                action = SetMods(modifiers=Shift,clearLocks);
            };
            interpret Control_L+AnyOf(all) {
                action = SetMods(modifiers=Control,clearLocks);
            };
            interpret Alt_L+AnyOf(all) {
                virtualModifier = Alt;
                action = SetMods(modifiers=modMapMods,clearLocks);
            };
            interpret Caps_Lock+AnyOf(all) {
                action = LockMods(modifiers=Lock);
            };
        };
        xkb_symbols {
            key <AC01> { type = \"ALPHABETIC\", symbols[Group1] = [ a, A ] };
            key <LFSH> { type = \"ONE_LEVEL\", symbols[Group1] = [ Shift_L ] };
            key <LCTL> { type = \"ONE_LEVEL\", symbols[Group1] = [ Control_L ] };
            key <LALT> { type = \"ONE_LEVEL\", symbols[Group1] = [ Alt_L ] };
            key <CAPS> { type = \"ONE_LEVEL\", symbols[Group1] = [ Caps_Lock ] };
            modifier_map Shift { <LFSH> };
            modifier_map Lock { <CAPS> };
            modifier_map Control { <LCTL> };
            modifier_map Mod1 { <LALT> };
        };
    };\0";

    const KEY_A: u32 = 30;
    const KEY_LEFTSHIFT: u32 = 42;
    const KEY_LEFTCTRL: u32 = 29;
    const KEY_LEFTALT: u32 = 56;
    const KEY_CAPSLOCK: u32 = 58;

    const SHIFT: u32 = 1 << 0;
    const LOCK: u32 = 1 << 1;
    const CONTROL: u32 = 1 << 2;
    const MOD1: u32 = 1 << 3;

    fn depressed(depressed: u32) -> Option<Mods> {
        Some(Mods {
            depressed,
            ..Mods::default()
        })
    }

    #[test]
    fn modifier_keys_change_the_modifiers_and_others_do_not() {
        let mut m = Modifiers::of(KEYMAP.as_bytes()).unwrap();
        assert_eq!(m.key(KEY_LEFTSHIFT, true), depressed(SHIFT));
        assert_eq!(m.key(KEY_A, true), None);
        assert_eq!(m.key(KEY_A, false), None);
        assert_eq!(m.key(KEY_LEFTCTRL, true), depressed(SHIFT | CONTROL));
        assert_eq!(m.key(KEY_LEFTSHIFT, false), depressed(CONTROL));
        assert_eq!(m.key(KEY_LEFTCTRL, false), depressed(0));
        // Alt is the real modifier it maps to, as xkbcommon takes it.
        assert_eq!(m.key(KEY_LEFTALT, true), depressed(MOD1));
        assert_eq!(m.key(KEY_LEFTALT, false), depressed(0));
    }

    #[test]
    fn caps_lock_locks_until_pressed_again() {
        let mut m = Modifiers::of(KEYMAP.as_bytes()).unwrap();
        let on = Mods {
            locked: LOCK,
            ..Mods::default()
        };
        assert_eq!(
            m.key(KEY_CAPSLOCK, true),
            Some(Mods {
                depressed: LOCK,
                ..on
            })
        );
        assert_eq!(m.key(KEY_CAPSLOCK, false), Some(on));
        assert_eq!(m.key(KEY_A, true), None);
        assert_eq!(m.key(KEY_A, false), None);
        m.key(KEY_CAPSLOCK, true);
        assert_eq!(m.key(KEY_CAPSLOCK, false), Some(Mods::default()));
    }

    /// Two layouts, and keys that switch between them as long as held,
    /// for the next key, and until pressed again.
    const LAYOUTS: &str = "xkb_keymap {
        xkb_keycodes {
            minimum = 8;
            maximum = 255;
            <AC01> = 38;
            <RALT> = 108;
            <LSGT> = 94;
            <MENU> = 135;
        };
        xkb_types {
            type \"ONE_LEVEL\" {
                modifiers = none;
                level_name[Level1] = \"Any\";
            };
        };
        xkb_compat { };
        xkb_symbols {
            key <AC01> {
                type = \"ONE_LEVEL\",
                symbols[Group1] = [ a ],
                symbols[Group2] = [ adiaeresis ]
            };
            key <RALT> {
                type = \"ONE_LEVEL\",
                actions[Group1] = [ SetGroup(group=+1) ]
            };
            key <LSGT> {
                type = \"ONE_LEVEL\",
                actions[Group1] = [ LatchGroup(group=+1) ]
            };
            key <MENU> {
                type = \"ONE_LEVEL\",
                actions[Group1] = [ LockGroup(group=+1) ],
                actions[Group2] = [ LockGroup(group=-1) ]
            };
        };
    };";

    const KEY_RIGHTALT: u32 = 100;
    const KEY_102ND: u32 = 86;
    const KEY_COMPOSE: u32 = 127;

    /// `zwp_virtual_keyboard_v1.modifiers` has one group, which the
    /// compositor takes as the locked one; a keyboard of its own tells the
    /// layout in effect in `wl_keyboard.modifiers`. So the one in effect
    /// goes: the locked one alone would lose a layout held or latched.
    #[test]
    fn the_layout_in_effect_is_told_however_it_was_switched() {
        let mut m = Modifiers::of(LAYOUTS.as_bytes()).unwrap();
        let group = |m: &Modifiers| m.mods().group;
        // Held: the second layout while the key is down.
        m.key(KEY_RIGHTALT, true);
        assert_eq!(group(&m), 1);
        m.key(KEY_RIGHTALT, false);
        assert_eq!(group(&m), 0);
        // Latched: for the next key.
        m.key(KEY_102ND, true);
        m.key(KEY_102ND, false);
        assert_eq!(group(&m), 1);
        m.key(KEY_A, true);
        m.key(KEY_A, false);
        assert_eq!(group(&m), 0);
        // Locked: until pressed again.
        m.key(KEY_COMPOSE, true);
        m.key(KEY_COMPOSE, false);
        assert_eq!(group(&m), 1);
        m.key(KEY_A, true);
        m.key(KEY_A, false);
        assert_eq!(group(&m), 1);
    }

    #[test]
    fn a_keymap_that_does_not_read_is_an_error() {
        assert!(Modifiers::of(b"xkb_keymap { nonsense").is_err());
    }
}
