import SwiftUI

/// The menu bar panel: state and the sharing
/// switch, the mini map (click a computer to move the cursor there, drop
/// files on one to send them), exceptions, transfers, then settings.
struct MenuPanel: View {
    @Environment(AgentModel.self) private var model
    @Environment(BackgroundService.self) private var service
    @Environment(\.openWindow) private var openWindow
    @State private var confirmingQuit = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            if model.link == .connected {
                ArrangementCanvas(interactive: false, selection: .constant(nil), padding: 12)
                    .frame(height: 118)
                    .clipShape(RoundedRectangle(cornerRadius: 8))
            }
            ExceptionList(compact: true)
            TransferList()
            ClipboardLine()
            Divider()
            footer
        }
        .padding(14)
        .frame(width: 340)
        // The menu bar window's own glass shows so much of what is behind
        // it that text over a bright, busy screen is hard to read in light
        // mode (macOS 26 and later); a thick material keeps it frosted but
        // readable. The window clips it to its rounded corners, and
        // `containerBackground(_:for: .window)` does not reach this window.
        .background(.thickMaterial)
        .confirmationDialog("Quit InputWeave?", isPresented: $confirmingQuit) {
            Button("Quit InputWeave", role: .destructive) { AppActions.quit(service: service) }
            Button("Hide Menu Bar Icon Only") { AppActions.hideMenuBarIcon() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("After quitting, this computer stops sharing its keyboard, mouse, clipboard and files with your other computers until you log in again or reopen InputWeave. If you only hide the menu bar icon, open InputWeave from the Applications folder to show it again.")
        }
    }

    private var header: some View {
        HStack(alignment: .firstTextBaseline) {
            VStack(alignment: .leading, spacing: 2) {
                Text(PanelText.title(model)).font(.headline)
                if let subtitle = PanelText.subtitle(model) {
                    Text(subtitle).font(.callout).foregroundStyle(.secondary)
                }
            }
            Spacer()
            Toggle(
                "Sharing",
                isOn: Binding(
                    get: { model.status.sharing },
                    set: { on in Task { try? await model.setSharing(on) } }
                )
            )
            .toggleStyle(.switch)
            .labelsHidden()
            .disabled(model.link != .connected)
            .accessibilityLabel(Text("Sharing"))
        }
    }

    private var footer: some View {
        HStack {
            Button("Send Files…") { SendFilesPanel.present(model: model) }
                .disabled(model.link != .connected || model.fileReceivers.isEmpty)
            Spacer()
            Button("Settings…") { AppActions.showMain(openWindow) }
                .keyboardShortcut(",")
            Spacer()
            Menu {
                Button("About InputWeave") { AppActions.showAbout(openWindow) }
                Divider()
                Button(model.status.sharing ? "Pause Sharing" : "Resume Sharing") {
                    Task { try? await model.setSharing(!model.status.sharing) }
                }
                .disabled(model.link != .connected)
                // The service handles the lock hotkey itself, so the item has no
                // shortcut of its own: with the panel open, both would act.
                // The header and General › Shortcuts teach the key. While
                // another computer controls this one, its lock is the one
                // that holds the cursor.
                Button(model.status.locked ? "Unlock" : "Lock to This Computer") {
                    Task { try? await model.setLocked(!model.status.locked) }
                }
                .disabled(
                    model.link != .connected || !model.status.sharing
                        || !model.status.controlledByDeviceID.isEmpty)
                IdentifyScreensButton()
                Button("Add Device…") { AppActions.showMain(openWindow, page: .devices, addDevice: true) }
                if let updater = AppActions.updater {
                    Button("Check for Updates…") { updater.checkForUpdates() }
                }
                Divider()
                // Uninstalling lives in Settings › General only: a menu used
                // every day should not invite it or let it be hit by mistake.
                Button("Quit InputWeave…") { confirmingQuit = true }
            } label: {
                Image(systemName: "ellipsis.circle")
            }
            .menuStyle(.borderlessButton)
            .menuIndicator(.hidden)
            .fixedSize()
            .accessibilityLabel(Text("More"))
        }
    }
}
