import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// The InputWeave window: a sidebar of pages.
struct MainWindow: View {
    @Environment(AgentModel.self) private var model
    @Bindable private var navigation = AppActions.navigation

    var body: some View {
        NavigationSplitView {
            List(selection: Binding($navigation.page)) {
                Section {
                    ForEach(Page.listed) { page in
                        Label(page.title, systemImage: page.symbol).tag(page)
                    }
                }
                Section {
                    LicenseSidebarRow().tag(Page.license)
                }
            }
            .navigationSplitViewColumnWidth(min: 190, ideal: 210)
        } detail: {
            Group {
                switch navigation.page {
                case .general: GeneralPage()
                case .devices: DevicesPage()
                case .arrangement: ArrangementPage()
                case .files: FilesPage()
                case .troubleshooting: TroubleshootingPage()
                case .license: LicensePage()
                }
            }
            .navigationTitle(navigation.page.title)
        }
        .frame(minWidth: 760, minHeight: 520)
        .sheet(isPresented: $navigation.addingDevice) {
            AddDeviceSheet()
        }
        .overlay {
            if model.link != .connected {
                AgentUnavailableView()
            }
        }
    }
}

/// Covers the window while the background service is out of reach.
private struct AgentUnavailableView: View {
    @Environment(AgentModel.self) private var model
    @Environment(BackgroundService.self) private var service

    var body: some View {
        ContentUnavailableView {
            Label(PanelText.title(model), systemImage: "bolt.horizontal.circle")
        } description: {
            Text("Settings appear here once the background service is running.")
        } actions: {
            if service.state == .requiresApproval {
                Button("Open Login Items") { service.openLoginItemsSettings() }
            } else if service.state == .notRegistered {
                Button("Turn On the Background Service") { service.register() }
            } else {
                Button("Restart the Background Service") { service.restart() }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .background(.background)
    }
}

/// The arrangement page: the canvas and, for the selected computer, an
/// inspector.
struct ArrangementPage: View {
    @Environment(AgentModel.self) private var model
    @State private var selection: String?
    @State private var syncedNotice = false

    init(selection: String? = nil) {
        _selection = State(initialValue: selection)
    }

    var body: some View {
        HStack(spacing: 0) {
            ZStack(alignment: .bottom) {
                ArrangementCanvas(selection: $selection)
                HStack {
                    IdentifyScreensButton()
                    if syncedNotice {
                        Text("Synced to \(model.status.devices.filter { $0.state == .connected }.count) devices")
                            .font(.callout)
                            .padding(.horizontal, 10)
                            .padding(.vertical, 4)
                            .background(.regularMaterial, in: Capsule())
                            .transition(.opacity)
                    }
                    Spacer()
                    Button("Add Device…") { AppActions.navigation.addingDevice = true }
                }
                .padding(12)
            }
            Divider()
            DeviceInspector(deviceID: selection ?? model.thisDeviceID)
                .frame(width: 250)
        }
        .onChange(of: model.arrangement.version) { old, new in
            guard old != 0, new != old, model.status.devices.contains(where: { $0.state == .connected }) else { return }
            withAnimation { syncedNotice = true }
            Task {
                try? await Task.sleep(for: .seconds(2))
                withAnimation { syncedNotice = false }
            }
        }
    }
}

/// What the arrangement page shows about one computer, and its options.
struct DeviceInspector: View {
    @Environment(AgentModel.self) private var model
    let deviceID: String
    @State private var confirmingRemoval = false

    var body: some View {
        let arranged = model.arrangement.devices.first { $0.id == deviceID }
        let status = model.device(deviceID)
        Form {
            Section {
                DeviceNameRow(deviceID: deviceID, fallback: arranged?.name ?? model.name(of: deviceID))
                LabeledContent("System") {
                    Label(OSLabel(os: arranged?.os ?? status?.os ?? "").name, systemImage: OSLabel(os: arranged?.os ?? "").symbol)
                }
                DeviceStatusRow(deviceID: deviceID, thisDevice: arranged?.thisDevice == true)
            }
            Section("Screens") {
                if let screens = arranged?.screens, !screens.isEmpty {
                    ForEach(Array(screens.enumerated()), id: \.offset) { _, screen in
                        HStack {
                            Text(verbatim: "\(Int((screen.width * screen.scale).rounded())) × \(Int((screen.height * screen.scale).rounded()))")
                                .monospacedDigit()
                            Spacer()
                            if screen.primary { Text("Main").foregroundStyle(.secondary) }
                        }
                    }
                } else {
                    Text("Shown once it connects.").foregroundStyle(.secondary)
                }
            }
            CursorSpeedSection(deviceID: deviceID)
            DeviceSharingSection(deviceID: deviceID)
            if let status, status.paired {
                Section {
                    Toggle("Swap ⌘ and Ctrl", isOn: Binding(
                        get: { status.swapGuiAndControl },
                        set: { on in Task { try? await model.setSwapGUIAndControl(deviceID, on) } }))
                    Button("Remove Device…", role: .destructive) { confirmingRemoval = true }
                }
            }
        }
        .formStyle(.grouped)
        .confirmationDialog("Remove \(model.name(of: deviceID))?", isPresented: $confirmingRemoval) {
            Button("Remove", role: .destructive) { Task { try? await model.unpair(deviceID) } }
        } message: {
            Text("This computer stops trusting it. To use it again, pair it again.")
        }
    }
}

/// Every trusted computer, and the way to add more.
struct DevicesPage: View {
    @Environment(AgentModel.self) private var model
    @State private var selection: String?

    var body: some View {
        VStack(spacing: 0) {
            if model.status.devices.isEmpty {
                ContentUnavailableView {
                    Label("No Other Computers", systemImage: "desktopcomputer")
                } description: {
                    Text("Install InputWeave on another computer on this network, then add it here.")
                } actions: {
                    Button("Add Device…") { AppActions.navigation.addingDevice = true }
                }
            } else {
                HStack(spacing: 0) {
                    List(model.status.devices, id: \.id, selection: $selection) { device in
                        DeviceRow(
                            device: device, jump: model.jumpShortcut(slot: device.jumpSlot),
                            wakeSupported: model.wakeSupported)
                    }
                    Divider()
                    DeviceInspector(deviceID: selection ?? model.status.devices.first?.id ?? "")
                        .frame(width: 250)
                }
                Divider()
                HStack {
                    Spacer()
                    Button("Add Device…") { AppActions.navigation.addingDevice = true }
                }
                .padding(10)
            }
        }
    }
}

private struct DeviceRow: View {
    let device: DeviceStatus
    /// The shortcut that moves the cursor to it, if any.
    var jump: String?
    /// The service can wake other computers.
    var wakeSupported = false

    var body: some View {
        HStack {
            Image(systemName: OSLabel(os: device.os).symbol)
                .foregroundStyle(.secondary)
                .frame(width: 22)
            VStack(alignment: .leading) {
                HStack(spacing: 4) {
                    Text(device.name)
                    // The status says it; the moon only marks it.
                    if WakeText.showsMoon(device, supported: wakeSupported) {
                        Image(systemName: WakeText.moon)
                            .imageScale(.small)
                            .accessibilityHidden(true)
                    }
                }
                Text(OSLabel(os: device.os).name).font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            if let jump {
                Text(verbatim: jump)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .accessibilityLabel(Text("Shortcut \(jump)"))
            }
            if device.state == .connected {
                Label(
                    ConnectionText.connected(
                        rttMs: device.rttMs, screenLocked: device.screenLocked,
                        lockScreenInput: device.lockScreenInput, inputUnavailable: device.inputUnavailable,
                        sharingPaused: device.sharingPaused),
                    systemImage: ConnectionText.symbol(screenLocked: device.screenLocked, sharingPaused: device.sharingPaused))
                    .labelStyle(.titleAndIcon)
                    .font(.caption)
                    .monospacedDigit()
                    .foregroundStyle(.green)
            } else {
                // Refreshed twice a minute, for how long it has been offline.
                TimelineView(.periodic(from: .now, by: 30)) { context in
                    Label(
                        WakeText.offlineStatus(device, supported: wakeSupported, now: context.date),
                        systemImage: "circle.dashed")
                        .font(.caption)
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                }
            }
        }
        .padding(.vertical, 2)
    }
}

/// Where received files go, whether to ask first, and the files sent and
/// received lately.
struct FilesPage: View {
    @Environment(AgentModel.self) private var model
    @State private var confirmingClear = false

    var body: some View {
        let transfers = model.transfers
        Form {
            Section("Receiving") {
                LabeledContent("Save received files in") {
                    if transfers.receiveFolder.isEmpty {
                        Text("This computer does not receive files.").foregroundStyle(.secondary)
                    } else {
                        HStack {
                            Label {
                                Text(verbatim: (transfers.receiveFolder as NSString).abbreviatingWithTildeInPath)
                                    .lineLimit(1)
                                    .truncationMode(.middle)
                            } icon: {
                                Image(systemName: "folder")
                            }
                            Button("Choose…", action: chooseFolder)
                        }
                    }
                }
                Picker("When a paired computer sends files", selection: Binding(
                    get: { transfers.askBeforeReceiving },
                    set: { ask in
                        Task { try? await model.setFileSettings(folderPath: transfers.receiveFolder, ask: ask) }
                    }
                )) {
                    Text("Receive them automatically").tag(false)
                    Text("Ask each time").tag(true)
                }
                .pickerStyle(.radioGroup)
            }
            Section("Sending") {
                LabeledContent {
                    Button("Send Files…") { SendFilesPanel.present(model: model) }
                        .disabled(model.fileReceivers.isEmpty)
                } label: {
                    Text("Drop files on a computer in the arrangement or the menu bar map, or choose them here.")
                        .foregroundStyle(.secondary)
                }
            }
            Section {
                if transfers.transfers.isEmpty {
                    Text("No files sent or received yet.").foregroundStyle(.secondary)
                }
                ForEach(transfers.transfers, id: \.transferID) { transfer in
                    TransferRow(transfer: transfer, removable: true)
                        .contextMenu {
                            if transfer.state.isFinished {
                                Button("Remove from History") {
                                    Task { try? await model.removeTransfer(transfer.transferID) }
                                }
                            }
                        }
                }
            } header: {
                HStack {
                    Text("Recent")
                    Spacer()
                    Button("Clear History…") { confirmingClear = true }
                        .controlSize(.small)
                        .disabled(!transfers.transfers.contains { $0.state.isFinished })
                }
            }
        }
        .formStyle(.grouped)
        .confirmationDialog("Clear the transfer history?", isPresented: $confirmingClear) {
            Button("Clear History", role: .destructive) { Task { try? await model.clearTransfers() } }
        } message: {
            Text("Finished transfers are taken off the list. The files stay where they are, and transfers still going stay listed.")
        }
    }

    private func chooseFolder() {
        let panel = NSOpenPanel()
        panel.canChooseFiles = false
        panel.canChooseDirectories = true
        panel.canCreateDirectories = true
        panel.prompt = String(localized: "Choose")
        panel.directoryURL = URL(fileURLWithPath: model.transfers.receiveFolder)
        guard panel.runModal() == .OK, let folder = panel.url else { return }
        Task { try? await model.setFileSettings(folderPath: folder.path, ask: model.transfers.askBeforeReceiving) }
    }
}

/// Name, startup, clipboard, desktop picture, switching, shortcut and
/// update settings.
struct GeneralPage: View {
    @Environment(AgentModel.self) private var model
    @Environment(BackgroundService.self) private var service
    /// Updates are a section only when this build has an update feed.
    var updater: Updater? = AppActions.updater
    var version = Bundle.main.shortVersion
    @State private var name = ""
    @State private var nameError: String?
    @State private var loginWindowError: String?

    var body: some View {
        Form {
            Section {
                TextField("This computer's name", text: $name)
                    .onSubmit(rename)
                if let nameError {
                    Text(nameError).foregroundStyle(.red).font(.callout)
                } else {
                    Text("Other computers see this name.").foregroundStyle(.secondary).font(.callout)
                }
            }
            Section {
                Toggle("Open InputWeave at login", isOn: Binding(
                    get: { service.launchesAtLogin },
                    set: { service.setLaunchesAtLogin($0) }))
                Toggle("Show in the menu bar", isOn: Binding(
                    get: { UserDefaults.standard.object(forKey: Preferences.showsMenuBarIcon) as? Bool ?? true },
                    set: { UserDefaults.standard.set($0, forKey: Preferences.showsMenuBarIcon) }))
                Text("When the icon is hidden, open InputWeave from the Applications folder to show it again.")
                    .foregroundStyle(.secondary)
                    .font(.callout)
            }
            Section("Clipboard") {
                Toggle("Sync the clipboard", isOn: Binding(
                    get: { model.status.clipboardSync },
                    set: { on in Task { try? await model.setClipboardSync(on) } }))
                Text("What you copy on one computer can be pasted on the next one the cursor moves to. Content from password managers is never synced, and nothing over 8 MB.")
                    .foregroundStyle(.secondary)
                    .font(.callout)
            }
            Section("Desktop pictures") {
                Toggle("Share this computer's desktop picture with paired computers", isOn: Binding(
                    get: { model.status.previewSharing },
                    set: { on in Task { try? await model.setPreviewSharing(on) } }))
                Text("Paired computers show it behind your screens in their arrangement, so each computer is easy to tell apart. Windows computers don't send theirs yet.")
                    .foregroundStyle(.secondary)
                    .font(.callout)
            }
            Section("Lock screen") {
                if model.status.lockScreenInput {
                    Text("Other computers can type and click on this Mac's lock screen.")
                        .foregroundStyle(.secondary)
                        .font(.callout)
                } else {
                    Text("Other computers can't type on this Mac's lock screen: InputWeave can't use the virtual keyboard it needs there, which takes a permission Apple grants. While the screen is locked, the cursor stays off this Mac.")
                        .foregroundStyle(.secondary)
                        .font(.callout)
                }
                // Off can always be chosen, even once the lock screen stops
                // taking input.
                Toggle("Let other computers control this Mac at the login window", isOn: Binding(
                    get: { model.status.loginWindowAccess },
                    set: { on in Task { await setLoginWindowAccess(on) } }))
                    .disabled(!model.status.lockScreenInput && !model.status.loginWindowAccess)
                if let loginWindowError {
                    Text(loginWindowError).foregroundStyle(.red).font(.callout)
                } else {
                    Text("Applies from the next time the login window shows, such as after you log out, as long as you were the last to log in. With FileVault on, a Mac that restarts asks for its password before macOS starts, so it doesn't apply then.")
                        .foregroundStyle(.secondary)
                        .font(.callout)
                }
            }
            SwitchingSection()
            ShortcutsSection()
            if let updater {
                Section("Updates") {
                    Toggle("Check for updates automatically", isOn: Binding(
                        get: { updater.checksAutomatically },
                        set: { updater.setChecksAutomatically($0) }))
                    LabeledContent("Version") {
                        HStack {
                            Text(version)
                            Button("Check Now…") { updater.checkForUpdates() }
                        }
                    }
                }
            }
            Section {
                LabeledContent {
                    Button("Uninstall InputWeave…") { Uninstaller.confirmAndRun() }
                } label: {
                    Text("Removes InputWeave and its background service from this Mac. Received files are kept.")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .formStyle(.grouped)
        .onAppear {
            name = model.status.thisDeviceName
            updater?.reload()
        }
        .onChange(of: model.status.thisDeviceName) { _, new in name = new }
    }

    private func rename() {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        guard trimmed != model.status.thisDeviceName else { return }
        Task {
            do {
                try await model.rename(trimmed)
                nameError = nil
            } catch {
                nameError = String(localized: "A name is 1 to 64 characters.")
            }
        }
    }

    private func setLoginWindowAccess(_ on: Bool) async {
        do {
            try await model.setLoginWindowAccess(on)
            loginWindowError = nil
        } catch let refusal as AgentRefusal {
            loginWindowError = LoginWindowText.refusal(refusal.code)
        } catch {
            loginWindowError = LoginWindowText.refusal("")
        }
    }
}

/// The state of every moving part, for when something does not work.
struct TroubleshootingPage: View {
    @Environment(AgentModel.self) private var model
    @Environment(BackgroundService.self) private var service

    var body: some View {
        Form {
            Section("Problems") {
                ExceptionList()
                if service.state == .enabled && model.link == .connected && model.missingPermissions.isEmpty
                    && model.offlineDevices.isEmpty && !model.status.secureInput {
                    Label("Everything is working.", systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                }
            }
            Section("Background service") {
                LabeledContent("Status") {
                    switch service.state {
                    case .enabled: Text(model.link == .connected ? "Running" : "Starting")
                    case .requiresApproval: Text("Waiting for approval in Login Items")
                    case .notRegistered: Text("Off")
                    case .notFound: Text("Missing from the app; reinstall InputWeave")
                    }
                }
                Button("Restart the Background Service") { service.restart() }
            }
            Section("Permissions") {
                ForEach(model.status.permissions, id: \.permission.rawValue) { permission in
                    LabeledContent(permission.permission == .accessibility ? "Accessibility" : "Input Monitoring") {
                        if permission.granted {
                            Label("Granted", systemImage: "checkmark.circle.fill").foregroundStyle(.green)
                        } else {
                            Button("Open System Settings") {
                                SystemSettings.open(permission.permission == .accessibility ? .accessibility : .inputMonitoring)
                            }
                        }
                    }
                }
                LabeledContent("This computer's keyboard and mouse") {
                    if model.status.capturing {
                        Text("Can control other computers")
                    } else {
                        Text("Cannot control other computers yet")
                    }
                }
                if !model.status.capturing {
                    Group {
                        if model.missingPermissions.isEmpty {
                            Text("If this does not change within a few seconds, restart the background service.")
                        } else {
                            Text("Turn on the permissions above. The background service then restarts on its own.")
                        }
                    }
                    .foregroundStyle(.secondary)
                    .font(.callout)
                }
            }
            if model.asksForFolders {
                FolderAccessSection()
            }
            Section("This computer") {
                LabeledContent("Name", value: model.status.thisDeviceName)
                LabeledContent("Device ID") { Text(model.status.thisDeviceID).textSelection(.enabled).monospaced() }
                LabeledContent("Version", value: Bundle.main.shortVersion)
            }
            Section {
                LabeledContent {
                    Button("Export Diagnostics…", action: exportDiagnostics)
                        .disabled(model.link != .connected)
                } label: {
                    Text("A text file to attach when reporting a problem: versions, settings, connection state and recent logs. It contains no keys, file names or clipboard contents.")
                        .foregroundStyle(.secondary)
                }
                LabeledContent {
                    Button("Open Logs Folder", action: openLogsFolder)
                } label: {
                    Text("The background service's log files.")
                        .foregroundStyle(.secondary)
                }
            }
        }
        .formStyle(.grouped)
        .onAppear { service.refresh() }
    }

    /// Shows the background service's log in Finder.
    private func openLogsFolder() {
        let folder = AgentModel.logFolder
        let log = folder.appending(path: "agent.log")
        if FileManager.default.fileExists(atPath: log.path) {
            NSWorkspace.shared.activateFileViewerSelecting([log])
        } else if FileManager.default.fileExists(atPath: folder.path) {
            NSWorkspace.shared.open(folder)
        } else {
            let alert = NSAlert()
            alert.messageText = String(localized: "The background service has not written a log yet.")
            alert.informativeText = String(localized: "Its log appears once it has started. Restart the background service, then try again.")
            alert.runModal()
        }
    }

    private func exportDiagnostics() {
        let panel = NSSavePanel()
        panel.nameFieldStringValue = "InputWeave Diagnostics.txt"
        panel.allowedContentTypes = [.plainText]
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task {
            do {
                try await model.diagnostics().write(to: url, atomically: true, encoding: .utf8)
                NSWorkspace.shared.activateFileViewerSelecting([url])
            } catch {
                let alert = NSAlert()
                alert.messageText = String(localized: "The diagnostics could not be exported.")
                alert.informativeText = error.localizedDescription
                alert.runModal()
            }
        }
    }
}

/// The folders macOS protects that the background service sends files
/// from and saves them in, on the Troubleshooting page.
private struct FolderAccessSection: View {
    @Environment(AgentModel.self) private var model
    @State private var error: String?

    var body: some View {
        Section("Files and Folders") {
            ForEach(model.status.folderAccess, id: \.folder) { access in
                let state = FolderAccessText.state(access.state)
                LabeledContent(FolderAccessText.name(access.folder)) {
                    Label(state.text, systemImage: state.symbol)
                        .foregroundStyle(Color(nsColor: FolderAccessText.color(access.state)))
                }
            }
            if model.status.folderAccess.contains(where: { $0.state == .denied }) {
                LabeledContent {
                    Button("Open System Settings") { SystemSettings.open(.filesAndFolders) }
                } label: {
                    Text("macOS asks about each folder only once. To allow one now, turn it on for “InputWeave” in System Settings › Privacy & Security › Files & Folders.")
                        .foregroundStyle(.secondary)
                }
            }
            LabeledContent {
                Button(model.status.folderAccessAsked ? "Check Again" : "Allow Access") {
                    Task {
                        do {
                            try await model.requestFolderAccess()
                            error = nil
                        } catch {
                            self.error = FolderAccessText.requestFailed
                        }
                    }
                }
                .disabled(model.link != .connected || model.askingForFolders)
            } label: {
                if model.status.folderAccessAsked {
                    Text("Looks at the folders again, after you changed them in System Settings.")
                        .foregroundStyle(.secondary)
                } else {
                    Text("macOS asks once for each folder InputWeave sends files from or saves them in. Ask now, so a transfer is not interrupted.")
                        .foregroundStyle(.secondary)
                }
            }
            if let error {
                Text(error).foregroundStyle(.red).font(.callout)
            }
        }
    }
}
