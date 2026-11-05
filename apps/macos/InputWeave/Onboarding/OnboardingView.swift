import SwiftUI

/// First-run setup on macOS.
struct OnboardingView: View {
    @Environment(AgentModel.self) private var model
    @Environment(BackgroundService.self) private var service
    @Environment(NotificationPermission.self) private var notifications
    @Environment(\.dismissWindow) private var dismissWindow
    @State private var step: Step
    @State private var name = ""
    /// "Allow Access" is waiting for macOS's prompts to be answered.
    @State private var allowing = false
    @State private var folderError: String?
    /// The running macOS's major version, which decides what the
    /// permission lists are called in System Settings.
    private let macOSMajor: Int

    enum Step: Int, CaseIterable {
        case welcome, background, permissions, folders, name, connect, arrange, tryIt
    }

    init(step: Step = .welcome, macOSMajor: Int = ProcessInfo.processInfo.operatingSystemVersion.majorVersion) {
        _step = State(initialValue: step)
        self.macOSMajor = macOSMajor
    }

    var body: some View {
        VStack(spacing: 0) {
            content
                .padding(32)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            Divider()
            HStack(spacing: 12) {
                ProgressView(value: Double(step.rawValue + 1), total: Double(Step.allCases.count))
                    .frame(width: 140)
                    .accessibilityLabel(Text("Setup progress"))
                Text("Step \(step.rawValue + 1) of \(Step.allCases.count)")
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .monospacedDigit()
                Spacer()
                if step != .welcome {
                    Button("Back", action: goBack)
                }
                if step == .connect {
                    Button("Later") {
                        cancelPairingInProgress()
                        finish()
                    }
                }
                if step == .folders && (leftToAsk || allowing) {
                    Button("Later", action: advance)
                    Button("Allow Access", action: allowAccess)
                        .buttonStyle(.borderedProminent)
                        .keyboardShortcut(.defaultAction)
                        .disabled(allowing || model.link != .connected)
                } else if step != .welcome && step != .connect {
                    Button(step == .tryIt ? "Done" : "Continue", action: advance)
                        .buttonStyle(.borderedProminent)
                        .keyboardShortcut(.defaultAction)
                        .disabled(!canAdvance)
                }
            }
            .padding(16)
        }
        .frame(width: 620, height: 520)
        .onChange(of: model.status.thisDeviceName) { _, new in if name.isEmpty { name = new } }
        // Read before the folders step, so its buttons do not change as it
        // opens.
        .task { await notifications.refresh() }
    }

    @ViewBuilder
    private var content: some View {
        switch step {
        case .welcome:
            VStack(spacing: 20) {
                Image(nsImage: NSApp.applicationIconImage).resizable().frame(width: 96, height: 96)
                Text("One keyboard and mouse for every computer on your desk.")
                    .font(.title2.weight(.semibold))
                    .multilineTextAlignment(.center)
                VStack(alignment: .leading, spacing: 10) {
                    Benefit(symbol: "display.2", text: "Move the cursor across the edge of the screen to the next computer.")
                    Benefit(symbol: "doc.on.clipboard", text: "Copy on one computer, paste on the next.")
                    Benefit(symbol: "arrow.up.doc", text: "Drop files on a computer to send them.")
                }
                Text("You'll need another computer on the same network. Setup takes about five minutes.")
                    .foregroundStyle(.secondary)
                    .multilineTextAlignment(.center)
                Button("Get Started") { advance() }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                    .keyboardShortcut(.defaultAction)
            }
        case .background:
            StepLayout(title: "Run in the background", symbol: "rectangle.on.rectangle") {
                Text("InputWeave keeps running in the background. Closing its window or menu bar icon does not stop sharing.")
                switch service.state {
                case .enabled:
                    Label("Allowed", systemImage: "checkmark.circle.fill").foregroundStyle(.green)
                case .requiresApproval:
                    Text("Turn on InputWeave in Login Items, then come back here.")
                    Button("Open Login Items") { service.openLoginItemsSettings() }
                case .notRegistered, .notFound:
                    Button("Allow in the Background") { service.register() }
                }
                if let error = service.lastError {
                    Text(error).foregroundStyle(.red).font(.callout)
                }
            }
            .task {
                // The user approves in System Settings; check back.
                while !Task.isCancelled && service.state != .enabled {
                    try? await Task.sleep(for: .seconds(1))
                    service.refresh()
                }
            }
        case .permissions:
            StepLayout(title: "Permissions", symbol: "lock.shield") {
                // macOS 27 calls the Accessibility list "Device Control and
                // Data Access", and turning it on grants Input Monitoring
                // too; the background service is not listed under Input
                // Monitoring there.
                if accessibilityIncludesInputMonitoring {
                    Text("In System Settings › Privacy & Security, turn on “InputWeave Agent” in the Device Control and Data Access list. That also grants Input Monitoring. This page checks every second.")
                } else {
                    Text("In System Settings › Privacy & Security, turn on “InputWeave Agent” in both the Accessibility list and the Input Monitoring list. This page checks every second.")
                }
                PermissionRow(
                    title: "Accessibility",
                    detail: "Lets this computer's keyboard and mouse control other computers, and lets them control this one.",
                    permission: .accessibility, pane: .accessibility)
                PermissionRow(
                    title: "Input Monitoring",
                    detail: accessibilityIncludesInputMonitoring
                        ? "Detects when the cursor reaches the edge of the screen. Granted with Device Control and Data Access."
                        : "Detects when the cursor reaches the edge of the screen.",
                    permission: .inputMonitoring,
                    pane: accessibilityIncludesInputMonitoring ? nil : .inputMonitoring)
                if model.link != .connected {
                    Text("Restarting the background service…").foregroundStyle(.secondary)
                }
            }
        case .folders:
            StepLayout(title: "Files and Folders", symbol: "folder") {
                Text("macOS asks once for each folder InputWeave uses, and for notifications. Allow them now, so nothing interrupts your first transfer.")
                if model.asksForFolders {
                    FolderAccessCard()
                }
                NotificationCard(permission: notifications)
                if model.status.folderAccess.contains(where: { $0.state == .denied }) {
                    Text("To allow a folder later, turn it on for “InputWeave” in System Settings › Privacy & Security › Files & Folders.")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
                if let folderError {
                    Text(folderError).foregroundStyle(.red).font(.callout)
                }
                if model.link != .connected {
                    Text("Restarting the background service…").foregroundStyle(.secondary)
                }
            }
            .task { await notifications.refresh() }
        case .name:
            StepLayout(title: "This computer's name", symbol: "character.cursor.ibeam") {
                TextField("Name", text: $name)
                    .textFieldStyle(.roundedBorder)
                    .frame(maxWidth: 320)
                Text("Other computers see this name.").foregroundStyle(.secondary)
            }
        case .connect:
            StepLayout(title: "Connect another computer", symbol: "plus.rectangle.on.rectangle") {
                PairingFlow { step = .arrange }
                Label("macOS may ask whether InputWeave can find devices on your local network. Choose Allow.", systemImage: "network")
                    .font(.callout)
                    .foregroundStyle(.secondary)
            }
        case .arrange:
            StepLayout(title: "Arrangement", symbol: "rectangle.3.group") {
                Text(arrangeHint)
                ArrangementCanvas(selection: .constant(nil), padding: 24)
                    .frame(height: 220)
                    .clipShape(RoundedRectangle(cornerRadius: 10))
            }
        case .tryIt:
            StepLayout(title: "Try it", symbol: "cursorarrow.motionlines") {
                if model.status.controlReturns > 0 || model.controllingOther {
                    Label("It works. Press \(model.takeBackShortcut) to take back control at any time.", systemImage: "checkmark.circle.fill")
                        .foregroundStyle(.green)
                } else {
                    Text("Move the cursor towards “\(otherName)”.")
                }
            }
        }
    }

    private var accessibilityIncludesInputMonitoring: Bool {
        macOSMajor >= 27
    }

    /// Something on the folders step is still for macOS to ask the user:
    /// a folder (while the background service is not connected, it may be),
    /// or notifications (until read, they may be).
    private var leftToAsk: Bool {
        model.link != .connected || model.foldersToAsk
            || notifications.state == .notAsked || notifications.state == .unknown
    }

    /// Has the background service list the folders, which has macOS ask
    /// about each in turn, then asks for notifications, so that two prompts
    /// never show at once.
    private func allowAccess() {
        allowing = true
        folderError = nil
        Task {
            if model.foldersToAsk {
                do {
                    let round = try await model.requestFolderAccess()
                    await model.wait { !model.listingFolders(in: round) || model.link != .connected }
                } catch {
                    folderError = FolderAccessText.requestFailed
                }
            }
            _ = await notifications.allowed()
            allowing = false
        }
    }

    private var otherName: String {
        model.status.devices.first?.name ?? String(localized: "the other computer")
    }

    private var arrangeHint: String {
        String(localized: "Drag “\(otherName)” to where it sits on your desk.")
    }

    private var canAdvance: Bool {
        switch step {
        case .background: service.state == .enabled
        case .name: !name.trimmingCharacters(in: .whitespaces).isEmpty
        default: true
        }
    }

    private func advance() {
        switch step {
        case .name:
            let trimmed = name.trimmingCharacters(in: .whitespaces)
            if trimmed != model.status.thisDeviceName {
                Task { try? await model.rename(trimmed) }
            }
        case .tryIt:
            finish()
            return
        default:
            break
        }
        if let next = Step(rawValue: step.rawValue + 1) {
            step = next
        }
    }

    /// Only moves back: what a step did when the user continued from it
    /// (registering, renaming) is not undone or done again.
    private func goBack() {
        if step == .connect {
            cancelPairingInProgress()
        }
        if let previous = Step(rawValue: step.rawValue - 1) {
            step = previous
        }
    }

    /// A pairing this step started would otherwise wait for a code nobody
    /// enters, like closing the Add Device sheet.
    private func cancelPairingInProgress() {
        switch model.pairing.phase {
        case .connecting, .enterCode, .verifying:
            Task { try? await model.cancelPairing() }
        default:
            break
        }
    }

    private func finish() {
        UserDefaults.standard.set(true, forKey: Preferences.finishedSetup)
        dismissWindow(id: "onboarding")
    }
}

private struct StepLayout<Content: View>: View {
    let title: LocalizedStringKey
    let symbol: String
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            Label(title, systemImage: symbol)
                .font(.title2.weight(.semibold))
            content
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }
}

/// One line of what InputWeave does, on the welcome step.
private struct Benefit: View {
    let symbol: String
    let text: LocalizedStringKey

    var body: some View {
        Label {
            Text(text)
        } icon: {
            Image(systemName: symbol)
                .foregroundStyle(.tint)
                .frame(width: 24)
        }
    }
}

/// The folders the background service asked macOS for, and what came of
/// each.
private struct FolderAccessCard: View {
    @Environment(AgentModel.self) private var model

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "folder")
                .foregroundStyle(.tint)
                .font(.title3)
            VStack(alignment: .leading, spacing: 6) {
                Text("Downloads, Desktop and Documents").font(.headline)
                Text("Lets InputWeave send the files you drag or choose there, and save the files it receives in Downloads › InputWeave or where you drop them.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                ForEach(model.status.folderAccess, id: \.folder) { access in
                    let state = FolderAccessText.state(access.state)
                    HStack(spacing: 6) {
                        Image(systemName: state.symbol)
                            .foregroundStyle(Color(nsColor: FolderAccessText.color(access.state)))
                        Text(FolderAccessText.name(access.folder))
                        Spacer()
                        Text(state.text).foregroundStyle(.secondary)
                    }
                    .accessibilityElement(children: .combine)
                }
            }
        }
        .padding(12)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 10))
    }
}

private struct NotificationCard: View {
    let permission: NotificationPermission

    var body: some View {
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: "bell")
                .foregroundStyle(.tint)
                .font(.title3)
            VStack(alignment: .leading, spacing: 4) {
                Text("Notifications").font(.headline)
                Text("Tells you when files arrive, and when InputWeave needs your attention.")
                    .foregroundStyle(.secondary)
            }
            Spacer()
            switch permission.state {
            case .allowed:
                Label("Allowed", systemImage: "checkmark.circle.fill")
                    .foregroundStyle(.green)
            case .denied:
                Button("Open System Settings") { NotificationPermission.openSettings() }
            case .notAsked, .unknown:
                Text("Not asked yet").foregroundStyle(.secondary)
            }
        }
        .padding(12)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 10))
    }
}

private struct PermissionRow: View {
    @Environment(AgentModel.self) private var model
    let title: LocalizedStringKey
    let detail: LocalizedStringKey
    let permission: Inputweave_App_V1_Permission
    /// Where the permission is turned on; nil when it comes with another.
    let pane: SystemSettings.Pane?

    var body: some View {
        let granted = model.status.permissions.first { $0.permission == permission }?.granted ?? false
        HStack(alignment: .top, spacing: 12) {
            Image(systemName: granted ? "checkmark.circle.fill" : "circle")
                .foregroundStyle(granted ? .green : .secondary)
                .font(.title3)
                .accessibilityLabel(granted ? Text("Granted") : Text("Not granted"))
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.headline)
                Text(detail).foregroundStyle(.secondary)
            }
            Spacer()
            if !granted, let pane {
                Button("Open System Settings") { SystemSettings.open(pane) }
            }
        }
        .padding(12)
        .background(.quaternary.opacity(0.5), in: RoundedRectangle(cornerRadius: 10))
    }
}
