//! Hotkeys as the background service writes them
//! (`control+alt+shift+KeyL`, app.proto `Hotkeys`) and as Linux shows
//! them: "Ctrl+Alt+Shift+L", the way GTK writes accelerators, with the
//! Super key for "gui"; and what a key pressed while recording one makes,
//! as the Windows app's `ShortcutText`. Only for showing and recording
//! them; the service decides which ones it takes.

use iw_keymap::HidUsage;
use iw_proto::app::v1::Hotkeys;

use crate::text::{UiText, keys};
use crate::ui;

pub const DEFAULT_TAKE_BACK: &str = "control+alt+shift+Escape";
pub const DEFAULT_LOCK: &str = "control+alt+shift+KeyL";
pub const DEFAULT_JUMP_MODIFIERS: &str = "control+alt+shift";
pub const DEFAULT_RELATIVE: &str = "control+alt+shift+KeyG";
/// The double press of Control that takes control back, as the service
/// writes it while it is on (`Hotkeys.take_back_double`); off is empty.
pub const DOUBLE_CONTROL: &str = "double+control";

/// The hotkeys of a service from before it reported them, and what Reset
/// Shortcuts goes back to: the double press off.
pub fn defaults() -> Hotkeys {
    Hotkeys {
        take_back: DEFAULT_TAKE_BACK.into(),
        lock: DEFAULT_LOCK.into(),
        jump_modifiers: DEFAULT_JUMP_MODIFIERS.into(),
        relative: Some(DEFAULT_RELATIVE.into()),
        take_back_double: Some(String::new()),
    }
}

const MODIFIERS: [(&str, &str); 4] = [
    ("control", "Ctrl"),
    ("alt", "Alt"),
    ("shift", "Shift"),
    ("gui", "Super"),
];

fn parts(hotkey: &str) -> Vec<&str> {
    hotkey
        .split('+')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect()
}

/// The modifiers' words in the order the service writes them.
fn modifier_words(names: &[&str]) -> Vec<&'static str> {
    let held: Vec<String> = names.iter().map(|n| n.to_ascii_lowercase()).collect();
    MODIFIERS
        .iter()
        .filter(|(name, _)| held.iter().any(|h| h == name))
        .map(|(_, word)| *word)
        .collect()
}

/// "Ctrl+Alt+Shift+L"; empty for an empty hotkey.
pub fn text(hotkey: &str) -> String {
    let parts = parts(hotkey);
    let Some((key, modifiers)) = parts.split_last() else {
        return String::new();
    };
    let mut words: Vec<String> = modifier_words(modifiers)
        .into_iter()
        .map(str::to_owned)
        .collect();
    words.push(key_word(key));
    words.join("+")
}

/// A key's name as written on it: "L" for KeyL, "2" for Digit2, "Esc".
pub fn key_word(name: &str) -> String {
    if let Some(letter) = name.strip_prefix("Key")
        && letter.len() == 1
    {
        return letter.to_owned();
    }
    if let Some(digit) = name.strip_prefix("Digit")
        && digit.len() == 1
    {
        return digit.to_owned();
    }
    match name {
        "Escape" => "Esc".into(),
        "ArrowLeft" => "Left".into(),
        "ArrowRight" => "Right".into(),
        "ArrowUp" => "Up".into(),
        "ArrowDown" => "Down".into(),
        other => other.to_owned(),
    }
}

/// "Ctrl+Alt+Shift+2", the jump hotkey for a device's number; `None` while
/// the jump hotkeys are off or the device has no number.
pub fn jump(modifiers: &str, slot: u32, _compact: bool) -> Option<String> {
    let words = modifier_words(&parts(modifiers));
    if words.is_empty() || !(1..=9).contains(&slot) {
        return None;
    }
    Some(format!("{}+{slot}", words.join("+")))
}

/// "Ctrl+Alt+Shift+1…9" for the jump hotkeys' row; empty while off.
pub fn jump_range(modifiers: &str) -> String {
    let words = modifier_words(&parts(modifiers));
    if words.is_empty() {
        return String::new();
    }
    format!("{}+1…9", words.join("+"))
}

/// A hotkey, or the jumps together, as General › Shortcuts lists them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Row {
    TakeBack,
    Lock,
    Jump,
    Relative,
}

impl Row {
    pub const ALL: [Row; 4] = [Row::TakeBack, Row::Lock, Row::Jump, Row::Relative];

    pub fn title(self) -> &'static str {
        match self {
            Row::TakeBack => keys::SHORTCUT_TAKE_BACK,
            Row::Lock => keys::SHORTCUT_LOCK,
            Row::Jump => keys::SHORTCUT_JUMP,
            Row::Relative => keys::SHORTCUT_RELATIVE,
        }
    }

    /// Whether the row can be turned off, with a switch beside it.
    pub fn can_turn_off(self) -> bool {
        matches!(self, Row::Jump | Row::Relative)
    }

    /// Whether the row's shortcut may be a single key, such as F13.
    pub fn takes_single_keys(self) -> bool {
        matches!(self, Row::TakeBack | Row::Lock)
    }

    /// What the row says while it records.
    pub fn prompt(self) -> &'static str {
        match self {
            Row::Jump => keys::SHORTCUT_PRESS_JUMP,
            Row::TakeBack | Row::Lock => keys::SHORTCUT_PRESS_SINGLE,
            Row::Relative => keys::SHORTCUT_PRESS_KEYS,
        }
    }
}

/// The modifier keys held as a key goes down, left or right alike.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Held {
    pub control: bool,
    pub alt: bool,
    pub shift: bool,
    pub gui: bool,
}

impl Held {
    pub fn any(self) -> bool {
        self.control || self.alt || self.shift || self.gui
    }

    /// As the background service writes them, in its order:
    /// `control+alt+shift`.
    pub fn names(self) -> String {
        [
            (self.control, "control"),
            (self.alt, "alt"),
            (self.shift, "shift"),
            (self.gui, "gui"),
        ]
        .iter()
        .filter(|(held, _)| *held)
        .map(|(_, name)| *name)
        .collect::<Vec<_>>()
        .join("+")
    }
}

/// What a key pressed while a shortcut is being recorded does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Recorded {
    /// Nothing yet: a modifier on its own, or a key without the modifiers
    /// the jumps need.
    Wait,
    /// Esc on its own: recording stops, the shortcut stays.
    Cancel,
    /// Recording stops and the row's shortcut becomes this.
    Set(String),
    /// A key without a name: recording stops and says it cannot be used.
    Unusable,
}

/// The name of the key GTK reports with `hardware_keycode`: X11 and
/// Wayland both number keys by their evdev code plus 8 (the XKB keycodes
/// libxkbcommon uses). `None` for a key without a name.
pub fn key_name(hardware_keycode: u32) -> Option<&'static str> {
    let evdev = u16::try_from(hardware_keycode.checked_sub(8)?).ok()?;
    HidUsage::from_evdev(evdev)?.name()
}

/// Whether a key, by its name, is a modifier itself.
pub fn is_modifier_key(key: &str) -> bool {
    matches!(
        key,
        "ControlLeft"
            | "ControlRight"
            | "AltLeft"
            | "AltRight"
            | "ShiftLeft"
            | "ShiftRight"
            | "MetaLeft"
            | "MetaRight"
    )
}

/// What a key pressed while `row` is recorded does, by its name (`None`
/// for a key without one) and the modifiers held, as the other two apps'
/// recorders: Esc on its own cancels, Delete or Backspace on its own goes
/// back to the row's default, the jumps take the modifiers held with any
/// key, and the others take the modifiers and the key. GTK reports the
/// modifier keys going down too; those wait for the key.
pub fn record(row: Row, key: Option<&str>, held: Held) -> Recorded {
    if key.is_some_and(is_modifier_key) {
        return Recorded::Wait;
    }
    if !held.any() && key == Some("Escape") {
        return Recorded::Cancel;
    }
    if !held.any() && matches!(key, Some("Delete" | "Backspace")) {
        return Recorded::Set(get(&defaults(), row));
    }
    if row == Row::Jump {
        // The digits need modifiers to go with; keep listening.
        return if held.any() {
            Recorded::Set(held.names())
        } else {
            Recorded::Wait
        };
    }
    let Some(key) = key else {
        return Recorded::Unusable;
    };
    Recorded::Set(if held.any() {
        format!("{}+{key}", held.names())
    } else {
        key.to_owned()
    })
}

/// The keys of a row: the hotkey, or the jumps' modifiers; empty for a row
/// turned off.
pub fn get(hotkeys: &Hotkeys, row: Row) -> String {
    match row {
        Row::TakeBack => hotkeys.take_back.clone(),
        Row::Lock => hotkeys.lock.clone(),
        Row::Relative => hotkeys.relative.clone().unwrap_or_default(),
        Row::Jump => hotkeys.jump_modifiers.clone(),
    }
}

/// A copy of `hotkeys` with `row` set to `value`.
pub fn with(hotkeys: &Hotkeys, row: Row, value: &str) -> Hotkeys {
    let mut changed = hotkeys.clone();
    match row {
        Row::TakeBack => changed.take_back = value.to_owned(),
        Row::Lock => changed.lock = value.to_owned(),
        Row::Relative => changed.relative = Some(value.to_owned()),
        Row::Jump => changed.jump_modifiers = value.to_owned(),
    }
    changed
}

/// Whether `hotkeys` are the defaults, the double press off, so Reset
/// Shortcuts has nothing to do. A service that knows no relative mode
/// hotkey (`relative` false) or no double press (`double_press` false) is
/// not asked about it.
pub fn is_default(hotkeys: &Hotkeys, relative: bool, double_press: bool) -> bool {
    let mut shown = hotkeys.clone();
    let mut defaults = defaults();
    if !relative {
        shown.relative = None;
        defaults.relative = None;
    }
    if !double_press {
        shown.take_back_double = None;
        defaults.take_back_double = None;
    }
    shown == defaults
}

/// A copy of `hotkeys` with the double press of Control on or off.
pub fn with_double_press(hotkeys: &Hotkeys, on: bool) -> Hotkeys {
    Hotkeys {
        take_back_double: Some(if on { DOUBLE_CONTROL } else { "" }.to_owned()),
        ..hotkeys.clone()
    }
}

/// What a row shows: the hotkey, or the jumps together, or Off.
pub fn shown(hotkeys: &Hotkeys, row: Row) -> UiText {
    match row {
        Row::Jump if hotkeys.jump_modifiers.is_empty() => ui!(keys::SHORTCUT_OFF),
        Row::Jump => UiText::verbatim(jump_range(&hotkeys.jump_modifiers)),
        Row::Relative if get(hotkeys, row).is_empty() => ui!(keys::SHORTCUT_OFF),
        _ => UiText::verbatim(text(&get(hotkeys, row))),
    }
}

/// What to tell the user about the background service refusing to change
/// `row`, making `before` into `hotkeys`. For a combination used already,
/// which shortcut uses it.
pub fn refusal(code: &str, row: Option<Row>, hotkeys: &Hotkeys, before: &Hotkeys) -> UiText {
    match code {
        "needs_modifier" if row.is_some_and(Row::takes_single_keys) => {
            ui!(keys::SHORTCUT_SINGLE_KEYS)
        }
        "needs_modifier" => ui!(keys::SHORTCUT_NEEDS_MODIFIER),
        "unknown_key" => ui!(keys::SHORTCUT_UNKNOWN_KEY),
        // In effect all the same, as the status shows.
        "not_saved" => ui!(keys::SHORTCUT_NOT_SAVED),
        "timeout" => ui!(keys::TITLE_NOT_RESPONDING),
        "duplicate" | "duplicate_relative" => {
            if let Some(changed) = row
                && let Some(other) = user(&get(hotkeys, changed), changed, before)
            {
                return ui!(keys::SHORTCUT_USED_FOR, ui!(other.title()));
            }
            // The service says the relative mode hotkey is one of the two.
            if code == "duplicate_relative" && row != Some(Row::Relative) {
                return ui!(keys::SHORTCUT_USED_FOR, ui!(keys::SHORTCUT_RELATIVE));
            }
            ui!(keys::SHORTCUT_USED_ELSEWHERE)
        }
        _ => ui!(keys::SHORTCUT_NOT_CHANGED),
    }
}

/// The other row of `hotkeys` that `keys`, new for `row`, clash with.
fn user(keys: &str, row: Row, hotkeys: &Hotkeys) -> Option<Row> {
    let jumps = |modifiers: &str| -> Vec<String> {
        if modifiers.is_empty() {
            return Vec::new();
        }
        (1..=9).map(|n| format!("{modifiers}+Digit{n}")).collect()
    };
    let singles = [Row::TakeBack, Row::Lock, Row::Relative];
    if row == Row::Jump {
        let jumps = jumps(keys);
        return singles
            .into_iter()
            .find(|single| jumps.iter().any(|j| clash(j, &get(hotkeys, *single))));
    }
    if let Some(other) = singles
        .into_iter()
        .filter(|s| *s != row)
        .find(|other| clash(keys, &get(hotkeys, *other)))
    {
        return Some(other);
    }
    jumps(&hotkeys.jump_modifiers)
        .iter()
        .any(|j| clash(j, keys))
        .then_some(Row::Jump)
}

/// Whether two hotkeys clash as the background service sees it
/// (`Hotkey::shadows`): the same key, and one's modifiers among the
/// other's, since modifiers held beyond a hotkey's own do not stop it.
pub fn clash(a: &str, b: &str) -> bool {
    fn split(keys: &str) -> Option<(Vec<String>, String)> {
        let names: Vec<String> = keys
            .to_ascii_lowercase()
            .split('+')
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty())
            .collect();
        let (key, modifiers) = names.split_last()?;
        Some((modifiers.to_vec(), key.clone()))
    }
    let (Some((first, key_a)), Some((second, key_b))) = (split(a), split(b)) else {
        return false;
    };
    if key_a != key_b {
        return false;
    }
    let subset = |x: &[String], y: &[String]| x.iter().all(|m| y.contains(m));
    subset(&first, &second) || subset(&second, &first)
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Held = Held {
        control: false,
        alt: false,
        shift: false,
        gui: false,
    };
    const CTRL: Held = Held {
        control: true,
        ..NONE
    };
    const CTRL_ALT: Held = Held {
        control: true,
        alt: true,
        ..NONE
    };
    const ALL: Held = Held {
        control: true,
        alt: true,
        shift: true,
        gui: true,
    };

    #[test]
    fn hotkeys_read_like_gtk_accelerators() {
        assert_eq!(text("control+alt+shift+KeyL"), "Ctrl+Alt+Shift+L");
        assert_eq!(text("control+alt+shift+Escape"), "Ctrl+Alt+Shift+Esc");
        assert_eq!(text("shift+gui+F5"), "Shift+Super+F5");
        // The service's order, whatever order they come in.
        assert_eq!(text("gui+SHIFT+control+Digit3"), "Ctrl+Shift+Super+3");
        assert_eq!(text("alt+ArrowLeft"), "Alt+Left");
        assert_eq!(text(""), "");
    }

    #[test]
    fn jump_hotkeys_by_number() {
        assert_eq!(
            jump("control+alt+shift", 2, true).as_deref(),
            Some("Ctrl+Alt+Shift+2")
        );
        assert_eq!(jump("", 2, true), None);
        assert_eq!(jump("control", 0, true), None);
        assert_eq!(jump("control", 10, true), None);
        assert_eq!(jump_range("alt+shift"), "Alt+Shift+1…9");
        assert_eq!(jump_range(""), "");
    }

    #[test]
    fn shows_each_row() {
        let hotkeys = defaults();
        assert_eq!(
            shown(&hotkeys, Row::TakeBack),
            UiText::verbatim("Ctrl+Alt+Shift+Esc")
        );
        assert_eq!(
            shown(&hotkeys, Row::Lock),
            UiText::verbatim("Ctrl+Alt+Shift+L")
        );
        assert_eq!(
            shown(&hotkeys, Row::Jump),
            UiText::verbatim("Ctrl+Alt+Shift+1…9")
        );
        let off = with(&hotkeys, Row::Jump, "");
        assert_eq!(shown(&off, Row::Jump), ui!(keys::SHORTCUT_OFF));
        assert_eq!(Row::Jump.prompt(), keys::SHORTCUT_PRESS_JUMP);
        assert_eq!(Row::Lock.prompt(), keys::SHORTCUT_PRESS_SINGLE);
        assert_eq!(Row::TakeBack.prompt(), keys::SHORTCUT_PRESS_SINGLE);
        assert_eq!(Row::Jump.title(), keys::SHORTCUT_JUMP);
    }

    #[test]
    fn changes_one_row_of_a_copy() {
        let before = defaults();
        let after = with(&before, Row::Lock, "control+alt+KeyK");
        assert_eq!(after.lock, "control+alt+KeyK");
        assert_eq!(before.lock, DEFAULT_LOCK);
        assert_eq!(with(&before, Row::Jump, "").jump_modifiers, "");
        assert_eq!(
            get(&with(&before, Row::TakeBack, "control+F1"), Row::TakeBack),
            "control+F1"
        );
    }

    #[test]
    fn names_keys_by_their_hardware_keycodes() {
        // evdev + 8, as GTK reports them on X11 and Wayland.
        assert_eq!(key_name(38 + 8), Some("KeyL"));
        assert_eq!(key_name(1 + 8), Some("Escape"));
        assert_eq!(key_name(2 + 8), Some("Digit1"));
        assert_eq!(key_name(111 + 8), Some("Delete"));
        assert_eq!(key_name(14 + 8), Some("Backspace"));
        assert_eq!(key_name(97 + 8), Some("ControlRight"));
        assert_eq!(key_name(125 + 8), Some("MetaLeft"));
        assert_eq!(key_name(105 + 8), Some("ArrowLeft"));
        assert_eq!(key_name(0), None);
        assert_eq!(key_name(8), None);
        assert!(is_modifier_key(key_name(125 + 8).unwrap()));
        assert!(is_modifier_key(key_name(100 + 8).unwrap()), "AltGr");
    }

    #[test]
    fn writes_the_modifiers_held_as_the_service_does() {
        assert_eq!(NONE.names(), "");
        assert_eq!(CTRL_ALT.names(), "control+alt");
        assert_eq!(ALL.names(), "control+alt+shift+gui");
        let shift_super = Held {
            shift: true,
            gui: true,
            ..NONE
        };
        assert_eq!(shift_super.names(), "shift+gui");
        assert!(!NONE.any());
        assert!(CTRL.any());
    }

    #[test]
    fn records_a_key_with_its_modifiers() {
        assert_eq!(
            record(Row::Lock, Some("KeyK"), CTRL_ALT),
            Recorded::Set("control+alt+KeyK".into())
        );
        assert_eq!(
            record(Row::TakeBack, Some("F5"), ALL),
            Recorded::Set("control+alt+shift+gui+F5".into())
        );
        // GTK reports the modifiers going down: those wait for the key.
        assert_eq!(record(Row::Lock, Some("ControlLeft"), CTRL), Recorded::Wait);
        assert_eq!(
            record(Row::Jump, Some("AltRight"), CTRL_ALT),
            Recorded::Wait
        );
        assert_eq!(
            record(Row::TakeBack, Some("MetaLeft"), NONE),
            Recorded::Wait
        );
        // Without a modifier it goes to the service, which says one is needed.
        assert_eq!(
            record(Row::Lock, Some("KeyL"), NONE),
            Recorded::Set("KeyL".into())
        );
        // A key without a name cannot be used.
        assert_eq!(record(Row::Lock, None, CTRL_ALT), Recorded::Unusable);
    }

    #[test]
    fn escape_cancels_and_delete_goes_back() {
        assert_eq!(record(Row::Lock, Some("Escape"), NONE), Recorded::Cancel);
        assert_eq!(record(Row::Jump, Some("Escape"), NONE), Recorded::Cancel);
        assert_eq!(
            record(Row::Lock, Some("Delete"), NONE),
            Recorded::Set(DEFAULT_LOCK.into())
        );
        assert_eq!(
            record(Row::TakeBack, Some("Backspace"), NONE),
            Recorded::Set(DEFAULT_TAKE_BACK.into())
        );
        assert_eq!(
            record(Row::Jump, Some("Delete"), NONE),
            Recorded::Set(DEFAULT_JUMP_MODIFIERS.into())
        );
        // With modifiers they are keys like any other.
        let ctrl_alt_shift = Held {
            shift: true,
            ..CTRL_ALT
        };
        assert_eq!(
            record(Row::TakeBack, Some("Escape"), ctrl_alt_shift),
            Recorded::Set("control+alt+shift+Escape".into())
        );
        assert_eq!(
            record(Row::Lock, Some("Delete"), CTRL),
            Recorded::Set("control+Delete".into())
        );
    }

    #[test]
    fn the_jumps_take_the_modifiers_held() {
        assert_eq!(
            record(Row::Jump, Some("Digit5"), CTRL_ALT),
            Recorded::Set("control+alt".into())
        );
        assert_eq!(
            record(Row::Jump, None, CTRL),
            Recorded::Set("control".into())
        );
        // A key alone has no modifiers to take; keep listening.
        assert_eq!(record(Row::Jump, Some("KeyA"), NONE), Recorded::Wait);
    }

    #[test]
    fn says_why_the_service_refused() {
        let before = defaults();
        assert_eq!(
            refusal(
                "needs_modifier",
                Some(Row::Lock),
                &with(&before, Row::Lock, "KeyL"),
                &before
            ),
            ui!(keys::SHORTCUT_SINGLE_KEYS)
        );
        assert_eq!(
            refusal("unknown_key", Some(Row::Lock), &before, &before),
            ui!(keys::SHORTCUT_UNKNOWN_KEY)
        );
        assert_eq!(
            refusal("other", Some(Row::Lock), &before, &before),
            ui!(keys::SHORTCUT_NOT_CHANGED)
        );
        // Changed but not saved, or no answer in time.
        assert_eq!(
            refusal("not_saved", Some(Row::Lock), &before, &before).to_string(),
            "The shortcut changed but couldn't be saved. It goes back to the earlier one when the background service restarts."
        );
        assert_eq!(
            refusal("timeout", None, &before, &before),
            ui!(keys::TITLE_NOT_RESPONDING)
        );
        assert_eq!(
            refusal("timeout", None, &before, &before).to_string(),
            "The background service is not responding."
        );
        // The lock made the same as taking back control.
        assert_eq!(
            refusal(
                "duplicate",
                Some(Row::Lock),
                &with(&before, Row::Lock, DEFAULT_TAKE_BACK),
                &before
            ),
            ui!(keys::SHORTCUT_USED_FOR, ui!(keys::SHORTCUT_TAKE_BACK))
        );
        // Taking back control made one of the jumps.
        assert_eq!(
            refusal(
                "duplicate",
                Some(Row::TakeBack),
                &with(&before, Row::TakeBack, "control+alt+shift+Digit3"),
                &before
            ),
            ui!(keys::SHORTCUT_USED_FOR, ui!(keys::SHORTCUT_JUMP))
        );
        // The jumps' modifiers made with a digit what the lock is.
        let lock_on_digit = with(
            &with(&before, Row::Jump, ""),
            Row::Lock,
            "control+alt+Digit1",
        );
        assert_eq!(
            refusal(
                "duplicate",
                Some(Row::Jump),
                &with(&lock_on_digit, Row::Jump, "control+alt"),
                &lock_on_digit
            ),
            ui!(keys::SHORTCUT_USED_FOR, ui!(keys::SHORTCUT_LOCK))
        );
        // Reset, or no telling which.
        assert_eq!(
            refusal("duplicate", None, &before, &before),
            ui!(keys::SHORTCUT_USED_ELSEWHERE)
        );
        assert_eq!(
            refusal(
                "duplicate",
                Some(Row::Lock),
                &with(&before, Row::Lock, "control+KeyQ"),
                &before
            ),
            ui!(keys::SHORTCUT_USED_ELSEWHERE)
        );
    }

    #[test]
    fn the_relative_mode_row() {
        let before = defaults();
        assert_eq!(
            shown(&before, Row::Relative),
            UiText::verbatim("Ctrl+Alt+Shift+G")
        );
        let off = with(&before, Row::Relative, "");
        assert_eq!(off.relative.as_deref(), Some(""));
        assert_eq!(shown(&off, Row::Relative), ui!(keys::SHORTCUT_OFF));
        assert_eq!(Row::Relative.title(), keys::SHORTCUT_RELATIVE);
        assert_eq!(Row::Relative.prompt(), keys::SHORTCUT_PRESS_KEYS);
        assert!(Row::Relative.can_turn_off());
        assert!(Row::Jump.can_turn_off());
        assert!(!Row::Lock.can_turn_off());
        assert!(!Row::TakeBack.can_turn_off());
        assert_eq!(
            record(Row::Relative, Some("Delete"), NONE),
            Recorded::Set(DEFAULT_RELATIVE.into())
        );
        assert_eq!(
            record(Row::Relative, Some("KeyR"), CTRL_ALT),
            Recorded::Set("control+alt+KeyR".into())
        );
        // Reset Shortcuts asks about it only when the service knows it.
        assert!(is_default(&before, true, true));
        assert!(!is_default(&off, true, true));
        assert!(is_default(&off, false, true));
        let old = Hotkeys {
            relative: None,
            ..before.clone()
        };
        assert!(is_default(&old, false, true));
        assert!(!is_default(&old, true, true));
        assert!(!is_default(
            &with(&before, Row::Lock, "control+KeyK"),
            false,
            true
        ));
    }

    #[test]
    fn names_the_rows_the_relative_mode_hotkey_clashes_with() {
        let before = defaults();
        // The relative mode hotkey made the lock: the lock is named, whichever code.
        let relative_on_lock = with(&before, Row::Relative, DEFAULT_LOCK);
        for code in ["duplicate_relative", "duplicate"] {
            assert_eq!(
                refusal(code, Some(Row::Relative), &relative_on_lock, &before),
                ui!(keys::SHORTCUT_USED_FOR, ui!(keys::SHORTCUT_LOCK))
            );
        }
        // The lock made the relative mode hotkey.
        let lock_on_relative = with(&before, Row::Lock, DEFAULT_RELATIVE);
        assert_eq!(
            refusal(
                "duplicate_relative",
                Some(Row::Lock),
                &lock_on_relative,
                &before
            ),
            ui!(keys::SHORTCUT_USED_FOR, ui!(keys::SHORTCUT_RELATIVE))
        );
        // The jumps' modifiers made with a digit what the relative mode hotkey is.
        let relative_on_digit = with(
            &with(&before, Row::Jump, ""),
            Row::Relative,
            "control+alt+Digit3",
        );
        assert_eq!(
            refusal(
                "duplicate",
                Some(Row::Jump),
                &with(&relative_on_digit, Row::Jump, "control+alt"),
                &relative_on_digit
            ),
            ui!(keys::SHORTCUT_USED_FOR, ui!(keys::SHORTCUT_RELATIVE))
        );
        // The service's rule: the same key, one's modifiers among the
        // other's. Ctrl+Alt+L fires whenever Ctrl+Alt+Shift+L would.
        assert_eq!(
            refusal(
                "duplicate_relative",
                Some(Row::Relative),
                &with(&before, Row::Relative, "control+alt+KeyL"),
                &before
            ),
            ui!(keys::SHORTCUT_USED_FOR, ui!(keys::SHORTCUT_LOCK))
        );
        assert_eq!(
            refusal(
                "duplicate",
                Some(Row::Lock),
                &with(&before, Row::Lock, "control+alt+shift+gui+Escape"),
                &before
            ),
            ui!(keys::SHORTCUT_USED_FOR, ui!(keys::SHORTCUT_TAKE_BACK))
        );
        assert!(clash("alt+control+KeyL", "control+alt+shift+keyl"));
        assert!(!clash("control+alt+KeyL", "control+shift+KeyL"));
        assert!(!clash("control+alt+KeyL", ""));
        // Turned off, it uses nothing.
        let off = with(&before, Row::Relative, "");
        assert_eq!(
            refusal(
                "duplicate",
                Some(Row::Lock),
                &with(&off, Row::Lock, ""),
                &off
            ),
            ui!(keys::SHORTCUT_USED_ELSEWHERE)
        );
        // A code from the service, with no telling which row.
        assert_eq!(
            refusal("duplicate_relative", None, &before, &before),
            ui!(keys::SHORTCUT_USED_FOR, ui!(keys::SHORTCUT_RELATIVE))
        );
    }

    /// Taking control back and the lock may be a single key such as F13;
    /// the service says which keys, and the app tells why by row.
    #[test]
    fn takes_single_keys_for_taking_back_and_the_lock() {
        assert_eq!(text("F13"), "F13");
        assert_eq!(text("ScrollLock"), "ScrollLock");
        assert_eq!(text("control+F13"), "Ctrl+F13");
        // evdev + 8: F13 is 183, F19 189, Scroll Lock 70, Pause 119.
        assert_eq!(key_name(183 + 8), Some("F13"));
        assert_eq!(key_name(189 + 8), Some("F19"));
        assert_eq!(key_name(70 + 8), Some("ScrollLock"));
        assert_eq!(key_name(119 + 8), Some("Pause"));
        // A key alone goes to the service as it is, for every row but the
        // jumps.
        assert_eq!(
            record(Row::TakeBack, Some("F13"), NONE),
            Recorded::Set("F13".into())
        );
        assert_eq!(
            record(Row::Lock, Some("Pause"), NONE),
            Recorded::Set("Pause".into())
        );
        assert_eq!(
            record(Row::Relative, Some("F13"), NONE),
            Recorded::Set("F13".into())
        );
        assert_eq!(record(Row::Jump, Some("F13"), NONE), Recorded::Wait);
        assert!(Row::TakeBack.takes_single_keys() && Row::Lock.takes_single_keys());
        assert!(!Row::Relative.takes_single_keys() && !Row::Jump.takes_single_keys());

        let before = defaults();
        for row in [Row::TakeBack, Row::Lock] {
            assert_eq!(
                refusal(
                    "needs_modifier",
                    Some(row),
                    &with(&before, row, "KeyK"),
                    &before
                ),
                ui!(keys::SHORTCUT_SINGLE_KEYS)
            );
        }
        assert_eq!(
            refusal(
                "needs_modifier",
                Some(Row::Relative),
                &with(&before, Row::Relative, "F13"),
                &before
            ),
            ui!(keys::SHORTCUT_NEEDS_MODIFIER)
        );
        assert_eq!(
            refusal("needs_modifier", None, &before, &before),
            ui!(keys::SHORTCUT_NEEDS_MODIFIER)
        );
        // F13 alone fires whatever modifiers are held: it clashes with any
        // hotkey on F13.
        let lock_alone = with(&before, Row::TakeBack, "control+F13");
        assert_eq!(
            refusal(
                "duplicate",
                Some(Row::Lock),
                &with(&lock_alone, Row::Lock, "F13"),
                &lock_alone
            ),
            ui!(keys::SHORTCUT_USED_FOR, ui!(keys::SHORTCUT_TAKE_BACK))
        );
        assert!(clash("F13", "control+alt+F13"));
        assert!(!clash("F13", "F14"));
    }

    /// The double press: off in the defaults, which Reset Shortcuts goes
    /// back to; asked about only when the service knows it.
    #[test]
    fn the_double_press_is_part_of_the_defaults() {
        let before = defaults();
        assert_eq!(before.take_back_double.as_deref(), Some(""));
        let on = with_double_press(&before, true);
        assert_eq!(on.take_back_double.as_deref(), Some(DOUBLE_CONTROL));
        assert_eq!(DOUBLE_CONTROL, "double+control");
        assert_eq!(with_double_press(&on, false), before);
        assert!(is_default(&before, true, true));
        assert!(!is_default(&on, true, true));
        // A service from before it reports none.
        let old = Hotkeys {
            take_back_double: None,
            ..before.clone()
        };
        assert!(is_default(&old, true, false));
        assert!(!is_default(&old, true, true));
    }
}
