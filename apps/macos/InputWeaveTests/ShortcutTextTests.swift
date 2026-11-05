import XCTest

final class KeyCodesTests: XCTestCase {
    /// The table generated from Chromium's names the keys as the
    /// background service does.
    func testNamesMacKeyCodes() {
        XCTAssertEqual(KeyCodes.domCode(mac: 0x00), "KeyA")
        XCTAssertEqual(KeyCodes.domCode(mac: 0x25), "KeyL")
        XCTAssertEqual(KeyCodes.domCode(mac: 0x12), "Digit1")
        XCTAssertEqual(KeyCodes.domCode(mac: 0x35), "Escape")
        XCTAssertEqual(KeyCodes.domCode(mac: 0x31), "Space")
        XCTAssertEqual(KeyCodes.domCode(mac: 0x60), "F5")
        XCTAssertEqual(KeyCodes.domCode(mac: 0x3B), "ControlLeft")
        // Fn has no name.
        XCTAssertNil(KeyCodes.domCode(mac: 0x3F))
    }
}

@MainActor
final class ShortcutTextTests: XCTestCase {
    func testShowsHotkeysWithSymbols() {
        XCTAssertEqual(ShortcutText.symbols("control+alt+shift+KeyL"), "⌃⌥⇧L")
        XCTAssertEqual(ShortcutText.symbols("control+alt+shift+Escape"), "⌃⌥⇧⎋")
        // In the order macOS shows them, whatever the order written.
        XCTAssertEqual(ShortcutText.symbols("gui+shift+Space"), "⇧⌘␣")
        XCTAssertEqual(ShortcutText.symbols("alt+F5"), "⌥F5")
        XCTAssertEqual(ShortcutText.symbols("control+Digit7"), "⌃7")
        XCTAssertEqual(ShortcutText.symbols("control+ArrowLeft"), "⌃ArrowLeft")
        XCTAssertEqual(ShortcutText.jumpRange(modifiers: "control+alt+shift"), "⌃⌥⇧ + 1…9")
        XCTAssertEqual(ShortcutText.jump(modifiers: "control+alt", slot: 2), "⌃⌥2")
        // Without a number, or with the jumps off.
        XCTAssertNil(ShortcutText.jump(modifiers: "control+alt", slot: 0))
        XCTAssertNil(ShortcutText.jump(modifiers: "control+alt", slot: 10))
        XCTAssertNil(ShortcutText.jump(modifiers: "", slot: 2))
        XCTAssertEqual(ShortcutText.symbol(.gui), "⌘")
        XCTAssertEqual(ShortcutText.symbol(SwitchModifier.none), "")
    }

    func testWritesKeyPressesAsTheServiceReadsThem() {
        XCTAssertEqual(
            ShortcutText.hotkey(keyCode: 0x25, control: true, option: true, shift: true, command: false),
            "control+alt+shift+KeyL")
        XCTAssertEqual(
            ShortcutText.hotkey(keyCode: 0x12, control: false, option: false, shift: true, command: true),
            "shift+gui+Digit1")
        // Without modifiers: the service says what is wrong with it.
        XCTAssertEqual(ShortcutText.hotkey(keyCode: 0x00, control: false, option: false, shift: false, command: false), "KeyA")
        XCTAssertNil(ShortcutText.hotkey(keyCode: 0x3F, control: true, option: false, shift: false, command: false))
        XCTAssertEqual(
            ShortcutText.hotkeyModifiers(control: true, option: false, shift: false, command: true), "control+gui")
        XCTAssertEqual(ShortcutText.hotkeyModifiers(control: false, option: false, shift: false, command: false), "")
    }

    func testSaysWhichShortcutAlreadyUsesTheKeys() {
        let before = ShortcutText.defaults
        var lockOnEscape = before
        lockOnEscape.lock = before.takeBack
        XCTAssertEqual(
            ShortcutText.refusal("duplicate", changing: .lock, to: lockOnEscape, from: before),
            "This combination is already used for “Take back control”.")
        var takeBackOnTwo = before
        takeBackOnTwo.takeBack = "control+alt+shift+Digit2"
        XCTAssertEqual(
            ShortcutText.refusal("duplicate", changing: .takeBack, to: takeBackOnTwo, from: before),
            "This combination is already used for “Jump to computer N”.")
        var lockOnOne = before
        lockOnOne.lock = "control+alt+Digit1"
        lockOnOne.jumpModifiers = ""
        var jumpsOnLock = lockOnOne
        jumpsOnLock.jumpModifiers = "control+alt"
        XCTAssertEqual(
            ShortcutText.refusal("duplicate", changing: .jump, to: jumpsOnLock, from: lockOnOne),
            "This combination is already used for “Lock to the current computer”.")
        XCTAssertEqual(
            ShortcutText.refusal("duplicate", changing: nil, to: before, from: before),
            "This combination is already used for another shortcut.")
        XCTAssertEqual(
            ShortcutText.refusal("needs_modifier", changing: .lock, to: before, from: before),
            "This combination needs at least one modifier key: ⌃, ⌥, ⇧ or ⌘.")
        XCTAssertEqual(
            ShortcutText.refusal("unknown_key", changing: .lock, to: before, from: before),
            "This key can't be used in a shortcut.")
        XCTAssertEqual(
            ShortcutText.refusal("timeout", changing: .lock, to: before, from: before),
            "The shortcut could not be changed.")
    }

    /// The relative mode row: its refusals
    /// name the right rows, and Reset Shortcuts asks about it only when
    /// the service knows it.
    func testTheRelativeModeRow() {
        let before = ShortcutText.defaults
        var relativeOnLock = before
        relativeOnLock.relative = before.lock
        XCTAssertEqual(
            ShortcutText.refusal("duplicate", changing: .relative, to: relativeOnLock, from: before),
            "This combination is already used for “Lock to the current computer”.")
        // The service's code for a clash with the relative mode hotkey
        // names the other row when the relative mode row is the one changed.
        XCTAssertEqual(
            ShortcutText.refusal("duplicate_relative", changing: .relative, to: relativeOnLock, from: before),
            "This combination is already used for “Lock to the current computer”.")
        var lockOnRelative = before
        lockOnRelative.lock = before.relative
        XCTAssertEqual(
            ShortcutText.refusal("duplicate_relative", changing: .lock, to: lockOnRelative, from: before),
            "This combination is already used for “Relative mouse (games)”.")
        var jumpsOnRelative = before
        jumpsOnRelative.relative = "control+alt+Digit3"
        jumpsOnRelative.jumpModifiers = ""
        var jumpsBack = jumpsOnRelative
        jumpsBack.jumpModifiers = "control+alt"
        XCTAssertEqual(
            ShortcutText.refusal("duplicate", changing: .jump, to: jumpsBack, from: jumpsOnRelative),
            "This combination is already used for “Relative mouse (games)”.")
        // The service's rule: the same key, one's modifiers among the
        // other's. ⌃⌥L fires whenever ⌃⌥⇧L would.
        var relativeUnderLock = before
        relativeUnderLock.relative = "control+alt+KeyL"
        XCTAssertEqual(
            ShortcutText.refusal("duplicate_relative", changing: .relative, to: relativeUnderLock, from: before),
            "This combination is already used for “Lock to the current computer”.")
        var lockOverTakeBack = before
        lockOverTakeBack.lock = "control+alt+shift+gui+Escape"
        XCTAssertEqual(
            ShortcutText.refusal("duplicate", changing: .lock, to: lockOverTakeBack, from: before),
            "This combination is already used for “Take back control”.")
        XCTAssertTrue(ShortcutText.clash("alt+control+KeyL", "control+alt+shift+keyl"))
        XCTAssertFalse(ShortcutText.clash("control+alt+KeyL", "control+shift+KeyL"))
        XCTAssertFalse(ShortcutText.clash("control+alt+KeyL", ""))
        // Turned off, it uses nothing.
        var off = before
        off.relative = ""
        var lockOnNothing = off
        lockOnNothing.lock = ""
        XCTAssertEqual(
            ShortcutText.refusal("duplicate", changing: .lock, to: lockOnNothing, from: off),
            "This combination is already used for another shortcut.")
        XCTAssertTrue(ShortcutText.Row.relative.canTurnOff)
        XCTAssertFalse(ShortcutText.Row.lock.canTurnOff)

        XCTAssertTrue(ShortcutText.isDefault(before, relative: true))
        XCTAssertFalse(ShortcutText.isDefault(off, relative: true))
        // A service from before relative mode reports none.
        var old = before
        old.clearRelative()
        XCTAssertTrue(ShortcutText.isDefault(old, relative: false))
        XCTAssertFalse(ShortcutText.isDefault(old, relative: true))
    }

    func testTheAppKnowsWhetherTheServiceHasRelativeMode() {
        var status = AgentStatus()
        status.thisDeviceID = "mac"
        status.hotkeys = ShortcutText.defaults
        // Only as the service says; one from before says nothing.
        XCTAssertFalse(AgentModel.sample(status: status).relativeSupported)
        XCTAssertNil(AgentModel.sample(status: status).relativeShortcut)
        status.relativeSupported = true
        XCTAssertEqual(AgentModel.sample(status: status).relativeShortcut, "⌃⌥⇧G")
        status.hotkeys.relative = ""
        let off = AgentModel.sample(status: status)
        XCTAssertTrue(off.relativeSupported)
        XCTAssertNil(off.relativeShortcut)
    }

    /// The service's hotkeys wherever the app shows one; the defaults
    /// from a service that does not report them.
    func testTheAppShowsTheHotkeysInEffect() {
        var status = AgentStatus()
        status.thisDeviceID = "mac"
        status.sharing = true
        status.locked = true
        let old = AgentModel.sample(status: status)
        XCTAssertEqual(old.lockShortcut, "⌃⌥⇧L")
        XCTAssertEqual(old.takeBackShortcut, "⌃⌥⇧⎋")
        XCTAssertEqual(old.jumpShortcut(slot: 3), "⌃⌥⇧3")

        var hotkeys = Hotkeys()
        hotkeys.takeBack = "control+gui+KeyT"
        hotkeys.lock = "alt+gui+KeyK"
        hotkeys.jumpModifiers = ""
        status.hotkeys = hotkeys
        let model = AgentModel.sample(status: status)
        XCTAssertEqual(model.takeBackShortcut, "⌃⌘T")
        XCTAssertEqual(PanelText.subtitle(model), "Press ⌥⌘K to unlock.")
        XCTAssertNil(model.jumpShortcut(slot: 1))
    }
}
