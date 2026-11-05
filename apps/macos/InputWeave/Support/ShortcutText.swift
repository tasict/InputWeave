import Foundation

typealias Hotkeys = Inputweave_App_V1_Hotkeys
typealias SwitchModifier = Inputweave_App_V1_SwitchModifier

/// How hotkeys look in the app: with symbols on macOS, from the text the background service writes them as, such as
/// `control+alt+shift+KeyL` (`Hotkeys` in app.proto). Only for showing and
/// recording them; the service decides which ones it takes.
enum ShortcutText {
    /// A hotkey, or the jumps together, as Settings lists them.
    enum Row: Hashable {
        case takeBack, lock, jump, relative

        var title: String {
            switch self {
            case .takeBack: String(localized: "Take back control")
            case .lock: String(localized: "Lock to the current computer")
            case .jump: String(localized: "Jump to computer N")
            case .relative: String(localized: "Relative mouse (games)")
            }
        }

        /// Whether the row can be turned off, with a switch beside it.
        var canTurnOff: Bool { self == .jump || self == .relative }
    }

    /// The hotkeys of a service that does not report them, and what
    /// Delete in the recorder goes back to.
    static let defaults: Hotkeys = {
        var hotkeys = Hotkeys()
        hotkeys.takeBack = "control+alt+shift+Escape"
        hotkeys.lock = "control+alt+shift+KeyL"
        hotkeys.jumpModifiers = "control+alt+shift"
        hotkeys.relative = "control+alt+shift+KeyG"
        return hotkeys
    }()

    /// The modifiers as the service names them, in the order macOS shows
    /// them, with their symbols.
    private static let modifiers: [(name: String, symbol: String)] = [
        ("control", "⌃"), ("alt", "⌥"), ("shift", "⇧"), ("gui", "⌘"),
    ]

    /// `control+alt+shift+KeyL` as ⌃⌥⇧L.
    static func symbols(_ hotkey: String) -> String {
        let parts = hotkey.split(separator: "+").map(String.init)
        guard let key = parts.last else { return "" }
        return modifierSymbols(parts.dropLast()) + keySymbol(key)
    }

    /// `control+alt+shift` as ⌃⌥⇧.
    static func modifierSymbols(_ hotkeyModifiers: String) -> String {
        modifierSymbols(hotkeyModifiers.split(separator: "+").map(String.init))
    }

    private static func modifierSymbols(_ names: some Sequence<String>) -> String {
        let held = Set(names.map { $0.lowercased() })
        return modifiers.filter { held.contains($0.name) }.map(\.symbol).joined()
    }

    /// A key's UI Events `code` name as it is shown: the letter or digit,
    /// ⎋ for Escape and ␣ for Space; other names as they are, such as F5.
    static func keySymbol(_ name: String) -> String {
        if name.hasPrefix("Key"), name.count == 4 { return String(name.dropFirst(3)) }
        if name.hasPrefix("Digit"), name.count == 6 { return String(name.dropFirst(5)) }
        switch name {
        case "Escape": return "⎋"
        case "Space": return "␣"
        default: return name
        }
    }

    /// The jump to the computer numbered `slot`, such as ⌃⌥⇧2; nil without
    /// a number, or with the jumps off.
    static func jump(modifiers: String, slot: UInt32) -> String? {
        guard !modifiers.isEmpty, (1...9).contains(slot) else { return nil }
        return modifierSymbols(modifiers) + String(slot)
    }

    /// All the jumps together, for settings: ⌃⌥⇧ + 1…9.
    static func jumpRange(modifiers: String) -> String {
        "\(modifierSymbols(modifiers)) + 1…9"
    }

    /// The hotkey a key press makes, as the service writes it: the
    /// modifiers held and the key's name. Nil for a key without a name.
    static func hotkey(keyCode: UInt16, control: Bool, option: Bool, shift: Bool, command: Bool) -> String? {
        guard let key = KeyCodes.domCode(mac: keyCode) else { return nil }
        let held = hotkeyModifiers(control: control, option: option, shift: shift, command: command)
        return held.isEmpty ? key : "\(held)+\(key)"
    }

    /// The modifiers held, as the service writes them: control+alt+shift.
    static func hotkeyModifiers(control: Bool, option: Bool, shift: Bool, command: Bool) -> String {
        zip(modifiers.map(\.name), [control, option, shift, command])
            .filter(\.1)
            .map(\.0)
            .joined(separator: "+")
    }

    /// Whether `hotkeys` are the defaults, so Reset Shortcuts has nothing
    /// to do. A service that knows no relative mode hotkey (`relative`
    /// false) is not asked about it.
    static func isDefault(_ hotkeys: Hotkeys, relative: Bool) -> Bool {
        var hotkeys = hotkeys
        var defaults = defaults
        if !relative {
            hotkeys.clearRelative()
            defaults.clearRelative()
        }
        return hotkeys == defaults
    }

    /// What to tell the user about the service refusing to change `row`,
    /// making `before` into `hotkeys`. For a combination used already,
    /// which shortcut uses it.
    static func refusal(_ code: String, changing row: Row?, to hotkeys: Hotkeys, from before: Hotkeys) -> String {
        switch code {
        case "needs_modifier":
            return String(localized: "This combination needs at least one modifier key: ⌃, ⌥, ⇧ or ⌘.")
        case "unknown_key":
            return String(localized: "This key can't be used in a shortcut.")
        case "duplicate", "duplicate_relative":
            if let row, let other = user(of: hotkeys[row], for: row, among: before) {
                return String(localized: "This combination is already used for “\(other.title)”.")
            }
            // The service says the relative mode hotkey is one of the two.
            if code == "duplicate_relative" && row != .relative {
                return String(localized: "This combination is already used for “\(Row.relative.title)”.")
            }
            return String(localized: "This combination is already used for another shortcut.")
        default:
            return String(localized: "The shortcut could not be changed.")
        }
    }

    /// The other row of `hotkeys` that `keys`, new for `row`, clash with:
    /// another hotkey, or one of the jumps.
    private static func user(of keys: String, for row: Row, among hotkeys: Hotkeys) -> Row? {
        let jumps = { (modifiers: String) in
            modifiers.isEmpty ? [] : (1...9).map { "\(modifiers)+Digit\($0)" }
        }
        let singles: [Row] = [.takeBack, .lock, .relative]
        switch row {
        case .takeBack, .lock, .relative:
            if let other = singles.first(where: { $0 != row && clash(keys, hotkeys[$0]) }) {
                return other
            }
            if jumps(hotkeys.jumpModifiers).contains(where: { clash($0, keys) }) { return .jump }
        case .jump:
            let new = jumps(keys)
            return singles.first { single in new.contains { clash($0, hotkeys[single]) } }
        }
        return nil
    }

    /// Whether two hotkeys clash as the background service sees it
    /// (`Hotkey::shadows`): the same key, and one's modifiers among the
    /// other's, since modifiers held beyond a hotkey's own do not stop it.
    static func clash(_ a: String, _ b: String) -> Bool {
        let parts = { (keys: String) -> (modifiers: Set<String>, key: String)? in
            let names = keys.lowercased().split(separator: "+").map(String.init)
            guard let key = names.last else { return nil }
            return (Set(names.dropLast()), key)
        }
        guard let a = parts(a), let b = parts(b), a.key == b.key else { return false }
        return a.modifiers.isSubset(of: b.modifiers) || b.modifiers.isSubset(of: a.modifiers)
    }

    /// The symbol of a modifier to hold for switching.
    static func symbol(_ modifier: SwitchModifier) -> String {
        switch modifier {
        case .control: "⌃"
        case .alt: "⌥"
        case .shift: "⇧"
        case .gui: "⌘"
        default: ""
        }
    }
}

extension Hotkeys {
    /// The keys of a row: the hotkey, or the jumps' modifiers.
    subscript(row: ShortcutText.Row) -> String {
        get {
            switch row {
            case .takeBack: takeBack
            case .lock: lock
            case .jump: jumpModifiers
            case .relative: relative
            }
        }
        set {
            switch row {
            case .takeBack: takeBack = newValue
            case .lock: lock = newValue
            case .jump: jumpModifiers = newValue
            case .relative: relative = newValue
            }
        }
    }
}

extension AgentModel {
    /// The hotkeys in effect: as the service reports them, or the defaults
    /// while it has not.
    var hotkeys: Hotkeys {
        status.hasHotkeys ? status.hotkeys : ShortcutText.defaults
    }

    /// Takes the keyboard and mouse back to this computer, such as ⌃⌥⇧⎋.
    var takeBackShortcut: String { ShortcutText.symbols(hotkeys.takeBack) }

    /// Locks the cursor to the computer it is on, or unlocks it.
    var lockShortcut: String { ShortcutText.symbols(hotkeys.lock) }

    /// The jump to the computer numbered `slot`, such as ⌃⌥⇧2; nil
    /// without one, or with the jumps off.
    func jumpShortcut(slot: UInt32) -> String? {
        ShortcutText.jump(modifiers: hotkeys.jumpModifiers, slot: slot)
    }

    /// Whether the service knows relative mode: it reports a relative mode hotkey, empty when it is off.
    var relativeSupported: Bool {
        status.relativeSupported
    }

    /// Turns relative mode on or off, such as ⌃⌥⇧G; nil when it is off or
    /// the service has no relative mode.
    var relativeShortcut: String? {
        guard relativeSupported, !hotkeys.relative.isEmpty else { return nil }
        return ShortcutText.symbols(hotkeys.relative)
    }
}
