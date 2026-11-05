import SwiftUI

/// The inspector's name row. This computer's
/// name is only shown here; General changes it. Another computer's is a
/// field: the name this Mac shows for it (`RenameDevice`), which only this
/// Mac uses, with the name it gives itself beneath once they differ.
/// "Rename…" in the arrangement's context menu selects a computer and puts
/// the cursor in this field.
struct DeviceNameRow: View {
    @Environment(AgentModel.self) private var model
    @Bindable private var navigation = AppActions.navigation
    let deviceID: String
    /// What to show for a computer whose name cannot be changed here.
    let fallback: String
    @State private var name = ""
    @State private var error: String?
    @FocusState private var focused: Bool

    var body: some View {
        if let device = model.device(deviceID) {
            VStack(alignment: .leading, spacing: 4) {
                TextField("Name", text: $name, prompt: Text(device.reportedName))
                    .focused($focused)
                    .onSubmit(commit)
                if let error {
                    Text(error).foregroundStyle(.red).font(.callout)
                } else if let reported = model.reportedName(of: deviceID) {
                    Text("It calls itself “\(reported)”.")
                        .foregroundStyle(.secondary)
                        .font(.callout)
                }
            }
            .onAppear { name = device.name }
            .onChange(of: deviceID) { _, _ in
                name = model.device(deviceID)?.name ?? ""
                error = nil
            }
            .onChange(of: device.name) { _, new in
                if !focused { name = new }
            }
            .onChange(of: focused) { _, now in
                if !now { commit() }
            }
            .onChange(of: navigation.renamingDevice, initial: true) { _, id in
                guard id == deviceID else { return }
                navigation.renamingDevice = nil
                Task { focused = true }
            }
        } else {
            LabeledContent("Name", value: fallback)
        }
    }

    /// Sends the name typed, unless it is the one shown already. Emptied,
    /// the computer's own name shows again.
    private func commit() {
        guard let device = model.device(deviceID) else { return }
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        guard trimmed != device.name else {
            name = device.name
            error = nil
            return
        }
        let id = deviceID
        Task {
            do {
                try await model.renameDevice(id, to: trimmed)
                error = nil
                name = model.device(id)?.name ?? trimmed
            } catch {
                self.error = String(localized: "A name is 1 to 64 characters.")
            }
        }
    }
}

/// Whether this Mac shares its clipboard and files with another computer
/// (`SetDeviceSharing`), in the inspector. Only this Mac stops; the other
/// computer is not told.
struct DeviceSharingSection: View {
    @Environment(AgentModel.self) private var model
    let deviceID: String

    var body: some View {
        if let device = model.device(deviceID) {
            Section {
                Toggle("Sync the clipboard with this computer", isOn: Binding(
                    get: { !device.clipboardOff },
                    set: { on in set(clipboardOff: !on, filesOff: device.filesOff) }))
                Toggle("Send and receive files with this computer", isOn: Binding(
                    get: { !device.filesOff },
                    set: { on in set(clipboardOff: device.clipboardOff, filesOff: !on) }))
            } footer: {
                Text("Only this Mac stops; the other computer is not told.")
                    .foregroundStyle(.secondary)
                    .font(.callout)
            }
        }
    }

    private func set(clipboardOff: Bool, filesOff: Bool) {
        let id = deviceID
        Task { try? await model.setDeviceSharing(id, clipboardOff: clipboardOff, filesOff: filesOff) }
    }
}

/// What the app says about files and a computer they are turned off with.
enum DeviceSharingText {
    /// Why files cannot go to or come from `device`.
    static func filesOff(_ device: String) -> String {
        String(localized: "Files with “\(device)” are turned off.")
    }

    /// The same, short, for a computer on the mini map.
    static var filesOffShort: String {
        String(localized: "Files turned off")
    }
}
