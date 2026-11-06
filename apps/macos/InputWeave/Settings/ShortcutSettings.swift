import AppKit
import SwiftUI

/// Settings › General › Switching: waiting
/// at the edge, a modifier to hold for the cursor to cross, the edge
/// lighting up where it crosses, and relative mode turning on by
/// itself for games.
struct SwitchingSection: View {
    @Environment(AgentModel.self) private var model
    /// Why the background service did not take the relative mode switch.
    @State private var relativeAutoError: String?
    /// The app's own setting, not the service's.
    @AppStorage(Preferences.showsEdgeGlow) private var showsEdgeGlow = true

    var body: some View {
        Section("Switching") {
            Toggle("Wait a moment before switching computers", isOn: Binding(
                get: { model.status.edgeDwellMs != 0 },
                set: { on in Task { try? await model.setEdgeDwell(ms: on ? model.switchDelayMs : 0) } }))
            Text("The cursor crosses to the next computer after resting at the edge of the screen for a moment, so brushing the edge does not switch.")
                .foregroundStyle(.secondary)
                .font(.callout)
            Picker("Switch to another computer", selection: Binding(
                get: { model.status.switchModifier },
                set: { modifier in Task { try? await model.setSwitchModifier(modifier) } }
            )) {
                Text("Always").tag(SwitchModifier.none)
                ForEach([SwitchModifier.control, .alt, .shift, .gui], id: \.self) { modifier in
                    Text("While holding \(ShortcutText.symbol(modifier))").tag(modifier)
                }
            }
            Text("Hold the key as the cursor reaches the edge of the screen, going either way. Shortcuts and clicks on the menu bar map move the cursor without it.")
                .foregroundStyle(.secondary)
                .font(.callout)
            Toggle("Light up the screen edge when the cursor crosses to another computer", isOn: $showsEdgeGlow)
            Text("A short glow in the seam color shows where the cursor left this computer's screen, or came in.")
                .foregroundStyle(.secondary)
                .font(.callout)
            // A service from before relative mode has no such switch.
            if model.relativeSupported {
                Toggle("Switch to relative mouse when a game holds the pointer", isOn: Binding(
                    get: { model.status.relativeAuto },
                    set: { on in Task { await setRelativeAuto(on) } }))
                if let relativeAutoError {
                    Text(relativeAutoError).foregroundStyle(.red).font(.callout)
                } else if model.relativeShortcut != nil {
                    Text("When an app on the computer you control, such as a game, hides the pointer and keeps it in its window, mouse movements go to it as they are and the cursor stays there until the app lets go. Only Windows computers tell when this happens; on a Mac, use the shortcut below.")
                        .foregroundStyle(.secondary)
                        .font(.callout)
                } else {
                    Text("When an app on the computer you control, such as a game, hides the pointer and keeps it in its window, mouse movements go to it as they are and the cursor stays there until the app lets go. Only Windows computers tell when this happens; for a Mac, you can set a shortcut for it below.")
                        .foregroundStyle(.secondary)
                        .font(.callout)
                }
            }
        }
        // The setting's state arrived, or the service started over: an
        // earlier refusal no longer holds.
        .onChange(of: model.status.relativeAuto) { _, _ in relativeAutoError = nil }
        .onChange(of: model.link) { _, _ in relativeAutoError = nil }
    }

    private func setRelativeAuto(_ on: Bool) async {
        do {
            try await model.setRelativeAuto(on)
            relativeAutoError = nil
        } catch {
            relativeAutoError = String(localized: "The setting could not be changed. Try again once the background service responds.")
        }
    }
}

/// Settings › General › Shortcuts: each
/// hotkey with a button that records new keys for it, and below taking
/// control back, the switch for pressing Control twice to do it too. The
/// background service checks them and says what is wrong; this only shows
/// its answer.
struct ShortcutsSection: View {
    @Environment(AgentModel.self) private var model
    /// The row being recorded for, if any.
    @State private var recording: Row?
    @State private var problem: String?
    @State private var recorder = KeyRecorder()

    typealias Row = ShortcutText.Row

    var body: some View {
        let hotkeys = model.hotkeys
        Section("Shortcuts") {
            row(.takeBack, keys: ShortcutText.symbols(hotkeys.takeBack))
            // A service from before the double press knows none.
            if model.doublePressSupported {
                Toggle("Double-press ⌃ to take back control", isOn: Binding(
                    get: { model.doublePressOn },
                    set: { on in
                        stopRecording()
                        setDoublePress(on)
                    }))
                    .disabled(model.link != .connected)
                Text("While controlling another computer, press Control twice quickly, with no other key between, to come back here. Control still reaches the other computer.")
                    .foregroundStyle(.secondary)
                    .font(.callout)
            }
            row(.lock, keys: ShortcutText.symbols(hotkeys.lock))
            row(.jump, keys: hotkeys.jumpModifiers.isEmpty
                ? String(localized: "Off") : ShortcutText.jumpRange(modifiers: hotkeys.jumpModifiers))
            // A service from before relative mode knows no such hotkey.
            if model.relativeSupported {
                row(.relative, keys: hotkeys.relative.isEmpty
                    ? String(localized: "Off") : ShortcutText.symbols(hotkeys.relative))
            }
            Text("Computers are numbered from left to right in the arrangement, and each number is shown next to the computer's name. While recording, press Esc to cancel or Delete to go back to the default.")
                .foregroundStyle(.secondary)
                .font(.callout)
            HStack {
                Spacer()
                Button("Reset Shortcuts") {
                    stopRecording()
                    apply(ShortcutText.defaults)
                }
                .disabled(
                    model.link != .connected
                        || ShortcutText.isDefault(
                            hotkeys, relative: model.relativeSupported, doublePress: model.doublePressSupported))
            }
        }
        .alert(problem ?? "", isPresented: Binding(
            get: { problem != nil },
            set: { if !$0 { problem = nil } }
        )) {
            Button("OK") {}
        }
        .onDisappear(perform: stopRecording)
        // A service from before relative mode took over: its row is gone.
        .onChange(of: model.relativeSupported) { _, supported in
            if !supported && recording == .relative {
                stopRecording()
            }
        }
    }

    private func row(_ row: Row, keys: String) -> some View {
        LabeledContent {
            HStack(spacing: 8) {
                Text(recording == row ? row.prompt : keys)
                    .foregroundStyle(recording == row ? .secondary : .primary)
                    .accessibilityLabel(recording == row ? Text(row.prompt) : Text(verbatim: keys))
                Button(recording == row ? "Cancel" : "Record") {
                    if recording == row { stopRecording() } else { startRecording(row) }
                }
                .disabled(model.link != .connected)
                if row.canTurnOff {
                    Toggle(row.title, isOn: Binding(
                        get: { !model.hotkeys[row].isEmpty },
                        set: { on in
                            stopRecording()
                            set(row, to: on ? ShortcutText.defaults[row] : "")
                        }
                    ))
                    .labelsHidden()
                    .toggleStyle(.switch)
                    .controlSize(.small)
                    .disabled(model.link != .connected)
                }
            }
        } label: {
            Text(row.title)
        }
    }

    private func startRecording(_ row: Row) {
        recording = row
        recorder.start { event in record(event, for: row) }
    }

    private func stopRecording() {
        recorder.stop()
        recording = nil
    }

    /// A key pressed while recording `row`.
    private func record(_ event: NSEvent, for row: Row) {
        let flags = event.modifierFlags.intersection(.deviceIndependentFlagsMask)
        let (control, option, shift, command) = (
            flags.contains(.control), flags.contains(.option), flags.contains(.shift), flags.contains(.command))
        let plain = !(control || option || shift || command)
        // Escape, Delete and Forward Delete on their own.
        if plain && event.keyCode == 53 {
            stopRecording()
            return
        }
        if plain && (event.keyCode == 51 || event.keyCode == 117) {
            stopRecording()
            set(row, to: ShortcutText.defaults[row])
            return
        }
        if row == .jump {
            let modifiers = ShortcutText.hotkeyModifiers(control: control, option: option, shift: shift, command: command)
            // The digits need modifiers to go with; keep listening.
            guard !modifiers.isEmpty else { return }
            stopRecording()
            set(.jump, to: modifiers)
            return
        }
        stopRecording()
        guard let hotkey = ShortcutText.hotkey(
            keyCode: event.keyCode, control: control, option: option, shift: shift, command: command)
        else {
            problem = String(localized: "This key can't be used in a shortcut.")
            return
        }
        set(row, to: hotkey)
    }

    private func set(_ row: Row, to value: String) {
        var hotkeys = model.hotkeys
        hotkeys[row] = value
        guard hotkeys != model.hotkeys else { return }
        apply(hotkeys, changing: row)
    }

    private func setDoublePress(_ on: Bool) {
        var hotkeys = model.hotkeys
        hotkeys.takeBackDouble = on ? ShortcutText.doubleControl : ""
        guard hotkeys != model.hotkeys else { return }
        apply(hotkeys)
    }

    private func apply(_ hotkeys: Hotkeys, changing row: Row? = nil) {
        let before = model.hotkeys
        Task {
            do {
                try await model.setHotkeys(hotkeys)
            } catch let refusal as AgentRefusal {
                problem = ShortcutText.refusal(refusal.code, changing: row, to: hotkeys, from: before)
            } catch {
                problem = String(localized: "The shortcut could not be changed.")
            }
        }
    }
}

/// Hears the keys pressed in this app's own windows while a shortcut is
/// recorded: a local event monitor, which sees only what is sent to this
/// app, needs no permission, and keeps the keys from reaching the window.
@MainActor
final class KeyRecorder {
    private var monitor: Any?

    func start(_ handle: @escaping @MainActor (NSEvent) -> Void) {
        stop()
        monitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
            MainActor.assumeIsolated { handle(event) }
            return nil
        }
    }

    func stop() {
        if let monitor { NSEvent.removeMonitor(monitor) }
        monitor = nil
    }
}
