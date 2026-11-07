import SwiftUI

/// Everything currently keeping sharing from working, each with the one
/// action that fixes it. Empty when all is well.
struct ExceptionList: View {
    @Environment(AgentModel.self) private var model
    @Environment(BackgroundService.self) private var service
    @Environment(\.openWindow) private var openWindow
    /// The panel shows short rows; Troubleshooting shows them in full.
    var compact = false

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            if service.state == .requiresApproval || service.state == .notRegistered {
                row(
                    "The background service is turned off, so sharing is not working.",
                    action: service.state == .requiresApproval ? "Open Login Items" : "Turn On"
                ) {
                    if service.state == .requiresApproval {
                        service.openLoginItemsSettings()
                    } else {
                        service.register()
                    }
                }
            }
            if model.link == .connected {
                ForEach(model.missingPermissions, id: \.rawValue) { permission in
                    switch permission {
                    case .accessibility:
                        row("InputWeave needs the Accessibility permission to control this computer.", action: "Open System Settings") {
                            SystemSettings.open(.accessibility)
                        }
                    case .inputMonitoring:
                        row("InputWeave needs the Input Monitoring permission to detect the cursor at the screen edge.", action: "Open System Settings") {
                            SystemSettings.open(.inputMonitoring)
                        }
                    default:
                        EmptyView()
                    }
                }
                if model.status.secureInput {
                    row("A password field is active, so keystrokes can't reach other computers right now.")
                }
                // Not a computer taken offline for now, which is meant to
                // be, nor any while this one is.
                ForEach(model.unreachableDevices, id: \.id) { device in
                    row("\(device.name) can't be reached.", action: compact ? "Troubleshoot" : nil) {
                        AppActions.showMain(openWindow, page: .troubleshooting)
                    }
                }
                if model.status.devices.isEmpty && compact {
                    row("No other computers yet.", action: "Add Device…") {
                        AppActions.showMain(openWindow, page: .devices, addDevice: true)
                    }
                }
            }
        }
    }

    private func row(_ text: LocalizedStringKey, action: LocalizedStringKey? = nil, perform: @escaping () -> Void = {}) -> some View {
        HStack(alignment: .firstTextBaseline, spacing: 6) {
            Image(systemName: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange)
                .accessibilityHidden(true)
            Text(text)
                .font(.callout)
                .fixedSize(horizontal: false, vertical: true)
            Spacer(minLength: 4)
            if let action {
                Button(action, action: perform)
                    .buttonStyle(.link)
                    .font(.callout)
            }
        }
    }
}

/// Deep links into System Settings. The app never asks for these
/// permissions itself; it only takes the user to where they are granted to
/// the background service.
enum SystemSettings {
    enum Pane: String {
        case accessibility = "Privacy_Accessibility"
        case inputMonitoring = "Privacy_ListenEvent"
        case localNetwork = "Privacy_LocalNetwork"
        case filesAndFolders = "Privacy_FilesAndFolders"
    }

    static func open(_ pane: Pane) {
        if let url = URL(string: "x-apple.systempreferences:com.apple.preference.security?\(pane.rawValue)") {
            NSWorkspace.shared.open(url)
        }
    }
}
