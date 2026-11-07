import SwiftUI

/// The menu bar panel: state, the profile in use and the sharing
/// switch (while Automatic picked a paused Other networks, adding the
/// network to a profile; while this computer is offline for now, bringing
/// it back), the mini map (click a computer to move the cursor there, drop
/// files on one to send them), exceptions, transfers, then settings.
struct MenuPanel: View {
    @Environment(AgentModel.self) private var model
    @Environment(BackgroundService.self) private var service
    @Environment(\.openWindow) private var openWindow
    @State private var confirmingQuit = false

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            header
            if model.link == .connected && ProfileText.offersAddingNetwork(model.status) {
                AddNetworkMenu()
            } else if model.link == .connected && model.offlineHere && !model.profilePaused {
                Button(OfflineText.bringBack()) { model.setOffline(model.thisDeviceID, false) }
            }
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
                PanelTitle(title: PanelText.title(model), capsule: PanelText.capsule(model))
                if let subtitle = PanelText.subtitle(model) {
                    Text(subtitle)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            Spacer()
            // Off while the profile in use pauses sharing: choosing another
            // profile is what brings it back.
            Toggle(
                "Sharing",
                isOn: Binding(
                    get: { model.status.sharing && !model.profilePaused },
                    set: { on in Task { try? await model.setSharing(on) } }
                )
            )
            .toggleStyle(.switch)
            .labelsHidden()
            .disabled(model.link != .connected || model.profilePaused)
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
                .disabled(model.link != .connected || model.profilePaused)
                if model.link == .connected && model.profilesSupported {
                    ProfileMenu { AppActions.showMain(openWindow, page: .profiles) }
                }
                // Taking this computer offline for now, or bringing it back;
                // the whole group then leaves it be.
                if let item = OfflineText.thisComputerItem(model.status) {
                    Button(item) { model.setOffline(model.thisDeviceID, !model.status.offlineForNow) }
                        .disabled(model.link != .connected)
                }
                // The service handles the lock hotkey itself, so the item has no
                // shortcut of its own: with the panel open, both would act.
                // The header and General › Shortcuts teach the key. While
                // another computer controls this one, its lock is the one
                // that holds the cursor.
                Button(model.status.locked ? "Unlock" : "Lock to This Computer") {
                    Task { try? await model.setLocked(!model.status.locked) }
                }
                .disabled(!model.canToggleLock)
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

/// The panel's title, with the profile in use in a neutral capsule after
/// it; the capsule goes beneath when both do not fit on one line.
private struct PanelTitle: View {
    let title: String
    let capsule: String?

    var body: some View {
        if let capsule {
            ViewThatFits(in: .horizontal) {
                HStack(spacing: 7) {
                    heading
                    ProfileCapsule(name: capsule)
                }
                VStack(alignment: .leading, spacing: 3) {
                    heading
                    ProfileCapsule(name: capsule)
                }
            }
            .accessibilityElement(children: .combine)
        } else {
            heading
        }
    }

    private var heading: some View {
        Text(title)
            .font(.headline)
            .fixedSize(horizontal: false, vertical: true)
    }
}

/// The profile in use, after the panel's title.
private struct ProfileCapsule: View {
    let name: String

    var body: some View {
        Text(name)
            .font(.caption.weight(.medium))
            .foregroundStyle(.secondary)
            .lineLimit(1)
            .padding(.horizontal, 7)
            .padding(.vertical, 1)
            .background(.quaternary, in: Capsule())
    }
}

/// "Add This Network To…": while Automatic picked a paused Other networks
/// for a network in no profile, the profiles it can go to; choosing one
/// moves it there, and the service switches to it. Off for the few seconds
/// the service knows the network by its router alone.
private struct AddNetworkMenu: View {
    @Environment(AgentModel.self) private var model

    var body: some View {
        Menu("Add This Network To…") {
            ForEach(ProfileText.addable(model.status), id: \.id) { profile in
                Button(ProfileText.name(profile)) {
                    model.changeProfiles("Adding the network to a profile") { try await $0.addNetworkToProfile(profile.id) }
                }
            }
        }
        .fixedSize()
        .disabled(!ProfileText.canAddNetwork(model.status))
    }
}

/// The ⋯ menu's Profile submenu: Automatic and every profile, the one in
/// effect checked, then Manage Profiles….
private struct ProfileMenu: View {
    @Environment(AgentModel.self) private var model
    let manage: () -> Void

    var body: some View {
        Menu("Profile") {
            // Automatic, or the profile the user chose.
            Picker("Profile", selection: Binding(
                get: { model.status.profileChosen ? model.status.profileID : "" },
                set: { id in model.changeProfiles("Choosing a profile") { try await $0.selectProfile(id) } }
            )) {
                Text(ProfileText.automatic()).tag("")
                ForEach(model.status.profiles, id: \.id) { profile in
                    Text(ProfileText.name(profile)).tag(profile.id)
                }
            }
            .pickerStyle(.inline)
            .labelsHidden()
            Divider()
            Button("Manage Profiles…", action: manage)
        }
    }
}
