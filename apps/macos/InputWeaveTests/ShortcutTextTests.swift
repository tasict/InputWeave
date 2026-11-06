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
            ShortcutText.refusal("needs_modifier", changing: .relative, to: before, from: before),
            "This combination needs at least one modifier key: ⌃, ⌥, ⇧ or ⌘.")
        XCTAssertEqual(
            ShortcutText.refusal("unknown_key", changing: .lock, to: before, from: before),
            "This key can't be used in a shortcut.")
        XCTAssertEqual(
            ShortcutText.refusal("other", changing: .lock, to: before, from: before),
            "The shortcut could not be changed.")
        // Changed but not saved, or no answer in time.
        XCTAssertEqual(
            ShortcutText.refusal("not_saved", changing: .lock, to: before, from: before),
            "The shortcut changed but couldn't be saved. It goes back to the earlier one when the background service restarts.")
        XCTAssertEqual(
            ShortcutText.refusal("timeout", changing: nil, to: before, from: before),
            "The background service is not responding.")
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

        XCTAssertTrue(ShortcutText.isDefault(before, relative: true, doublePress: true))
        XCTAssertFalse(ShortcutText.isDefault(off, relative: true, doublePress: true))
        // A service from before relative mode reports none.
        var old = before
        old.clearRelative()
        XCTAssertTrue(ShortcutText.isDefault(old, relative: false, doublePress: true))
        XCTAssertFalse(ShortcutText.isDefault(old, relative: true, doublePress: true))
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

    /// Taking control back and the lock may be a single key such as F13;
    /// the service says which keys, and the app tells why by row.
    func testSingleKeys() {
        XCTAssertEqual(ShortcutText.symbols("F13"), "F13")
        XCTAssertEqual(ShortcutText.symbols("Pause"), "Pause")
        XCTAssertEqual(ShortcutText.symbols("control+F13"), "⌃F13")
        // A key pressed alone goes to the service as it is.
        XCTAssertEqual(
            ShortcutText.hotkey(keyCode: 0x69, control: false, option: false, shift: false, command: false), "F13")
        // Apple's extended keyboard has F14 where Scroll Lock is.
        XCTAssertEqual(
            ShortcutText.hotkey(keyCode: 0x6B, control: false, option: false, shift: false, command: false), "F14")
        XCTAssertEqual(ShortcutText.Row.takeBack.prompt, "Press a shortcut, or a single key such as F13…")
        XCTAssertEqual(ShortcutText.Row.lock.prompt, "Press a shortcut, or a single key such as F13…")
        XCTAssertEqual(ShortcutText.Row.relative.prompt, "Press the keys…")
        XCTAssertEqual(ShortcutText.Row.jump.prompt, "Press the modifiers and a number…")

        let before = ShortcutText.defaults
        for row in [ShortcutText.Row.takeBack, .lock] {
            var alone = before
            alone[row] = "KeyK"
            XCTAssertEqual(
                ShortcutText.refusal("needs_modifier", changing: row, to: alone, from: before),
                "Only F13–F19 work alone. Other keys need a modifier: ⌃, ⌥, ⇧ or ⌘.")
        }
        var relativeAlone = before
        relativeAlone.relative = "F13"
        XCTAssertEqual(
            ShortcutText.refusal("needs_modifier", changing: .relative, to: relativeAlone, from: before),
            "This combination needs at least one modifier key: ⌃, ⌥, ⇧ or ⌘.")
        XCTAssertEqual(
            ShortcutText.refusal("needs_modifier", changing: nil, to: before, from: before),
            "This combination needs at least one modifier key: ⌃, ⌥, ⇧ or ⌘.")
        // F13 alone clashes with any hotkey on F13, as the service sees it.
        var lockAlone = before
        lockAlone.takeBack = "control+F13"
        var takeBackOnLock = lockAlone
        takeBackOnLock.lock = "F13"
        XCTAssertEqual(
            ShortcutText.refusal("duplicate", changing: .lock, to: takeBackOnLock, from: lockAlone),
            "This combination is already used for “Take back control”.")
        XCTAssertTrue(ShortcutText.clash("F13", "control+alt+F13"))
        XCTAssertFalse(ShortcutText.clash("F13", "F14"))
    }

    /// The switch for pressing Control twice: shown only when the service
    /// reports it, part of Reset Shortcuts, and named in the panel.
    func testTheDoublePress() {
        var status = AgentStatus()
        status.thisDeviceID = "mac"
        status.sharing = true
        status.activeDeviceID = "pc"
        var pc = DeviceStatus()
        pc.id = "pc"
        pc.name = "Windows Desktop"
        pc.state = .connected
        status.devices = [pc]
        var hotkeys = ShortcutText.defaults
        hotkeys.clearTakeBackDouble()
        status.hotkeys = hotkeys
        // A service from before it says nothing of it.
        let old = AgentModel.sample(status: status)
        XCTAssertFalse(old.doublePressSupported)
        XCTAssertFalse(old.doublePressOn)
        XCTAssertEqual(PanelText.subtitle(old), "Press ⌃⌥⇧⎋ to take back control.")
        XCTAssertTrue(ShortcutText.isDefault(old.hotkeys, relative: false, doublePress: false))

        status.hotkeys.takeBackDouble = ""
        let off = AgentModel.sample(status: status)
        XCTAssertTrue(off.doublePressSupported)
        XCTAssertFalse(off.doublePressOn)
        XCTAssertEqual(PanelText.subtitle(off), "Press ⌃⌥⇧⎋ to take back control.")
        XCTAssertTrue(ShortcutText.isDefault(off.hotkeys, relative: false, doublePress: true))

        // Off, the notices and setup say it as before.
        XCTAssertEqual(off.takeBackReminder, "Press ⌃⌥⇧⎋ to take back control at any time.")
        XCTAssertEqual(off.takeBackWorked, "It works. Press ⌃⌥⇧⎋ to take back control at any time.")

        status.hotkeys.takeBackDouble = ShortcutText.doubleControl
        let on = AgentModel.sample(status: status)
        XCTAssertTrue(on.doublePressOn)
        XCTAssertEqual(PanelText.subtitle(on), "Press ⌃⌥⇧⎋ or double-press ⌃ to take back control.")
        // On, everything that tells how to take control back says it as
        // the panel does.
        XCTAssertEqual(on.takeBackReminder, "Press ⌃⌥⇧⎋ or double-press ⌃ to take back control.")
        XCTAssertEqual(on.takeBackWorked, "It works. Press ⌃⌥⇧⎋ or double-press ⌃ to take back control.")
        // On, Reset Shortcuts has something to do; it turns it off.
        XCTAssertFalse(ShortcutText.isDefault(on.hotkeys, relative: false, doublePress: true))
        XCTAssertEqual(ShortcutText.defaults.takeBackDouble, "")
        XCTAssertTrue(ShortcutText.defaults.hasTakeBackDouble)
        // A single key reads as itself.
        status.hotkeys.takeBack = "F13"
        XCTAssertEqual(
            PanelText.subtitle(AgentModel.sample(status: status)), "Press F13 or double-press ⌃ to take back control.")
    }

    /// In relative mouse the service ignores the double press (games often
    /// use Control), so nothing that tells how to take control back names
    /// it then, even with it on.
    func testRelativeMouseHintsLeaveOutTheDoublePress() {
        var status = AgentStatus()
        status.thisDeviceID = "mac"
        status.sharing = true
        status.activeDeviceID = "pc"
        var pc = DeviceStatus()
        pc.id = "pc"
        pc.name = "Windows Desktop"
        pc.state = .connected
        status.devices = [pc]
        status.hotkeys = ShortcutText.defaults
        status.hotkeys.takeBackDouble = ShortcutText.doubleControl
        status.hotkeys.relative = ""
        status.relativeSupported = true
        status.relativeMouse = "pc"
        let relative = AgentModel.sample(status: status)
        XCTAssertTrue(relative.doublePressOn)
        XCTAssertEqual(
            RelativeText.message(.entered(device: "pc", automatic: false), model: relative),
            "Relative mouse: the cursor stays on “Windows Desktop”. Press ⌃⌥⇧⎋ to take back control.")
        XCTAssertEqual(PanelText.subtitle(relative), "Press ⌃⌥⇧⎋ to take back control.")
        XCTAssertEqual(relative.takeBackReminder, "Press ⌃⌥⇧⎋ to take back control at any time.")
        XCTAssertEqual(relative.takeBackWorked, "It works. Press ⌃⌥⇧⎋ to take back control at any time.")
        // Once it ends, the panel names it again.
        status.relativeMouse = ""
        XCTAssertEqual(
            PanelText.subtitle(AgentModel.sample(status: status)), "Press ⌃⌥⇧⎋ or double-press ⌃ to take back control.")
    }
}
