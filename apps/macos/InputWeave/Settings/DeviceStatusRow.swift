import SwiftUI

/// The inspector's status row: how the computer is connected; while
/// another computer is offline, how long it has been, Wake when it can be
/// woken, and a note on waking it (`WakeText`); while it is taken offline
/// for now, which computer did it, and the computers that still control it
/// (`OfflineText`); and that the profile in use leaves it out
/// (`ProfileText`).
struct DeviceStatusRow: View {
    @Environment(AgentModel.self) private var model
    let deviceID: String
    let thisDevice: Bool

    var body: some View {
        if thisDevice {
            LabeledContent("Status") { Text("This computer") }
        } else if let device = model.device(deviceID) {
            if OfflineText.shows(device, status: model.status) {
                OfflineForNowStatus(device: device)
            } else if ProfileText.shows(device, status: model.status) {
                // Neither woken nor taken offline from here meanwhile.
                LabeledContent("Status") {
                    HStack(spacing: 6) {
                        // The words say it.
                        Image(systemName: ProfileText.symbol)
                            .accessibilityHidden(true)
                        Text(ProfileText.notInProfile())
                    }
                }
            } else if device.state == .connected {
                LabeledContent("Status") {
                    Text(ConnectionText.connected(
                        rttMs: device.rttMs, screenLocked: device.screenLocked,
                        lockScreenInput: device.lockScreenInput, inputUnavailable: device.inputUnavailable,
                        sharingPaused: device.sharingPaused))
                        .monospacedDigit()
                }
            } else {
                offline(device)
            }
        } else {
            LabeledContent("Status") { Text("Offline") }
        }
    }

    /// Refreshed twice a minute, for how long it has been offline.
    private func offline(_ device: DeviceStatus) -> some View {
        let supported = model.wakeSupported
        let offer = model.wakeOffer(deviceID)
        // While this computer is offline for now, or the profile in use is
        // paused, every other one is plainly offline, and none can be woken
        // from here.
        let note = model.offlineHere || model.profilePaused ? nil : WakeText.note(device, supported: supported)
        return TimelineView(.periodic(from: .now, by: 30)) { context in
            let text = WakeText.offlineStatus(device, supported: supported, now: context.date)
            let waking = WakeText.waking(device, supported: supported)
            let moon = !waking && WakeText.showsMoon(device, supported: supported)
            VStack(alignment: .leading, spacing: 6) {
                if offer == .hidden {
                    LabeledContent("Status") {
                        OfflineStatus(text: text, waking: waking, moon: moon)
                    }
                } else {
                    // Wake beside the label, which leaves the status the
                    // row's width below it: the inspector is too narrow for
                    // all three on one line.
                    VStack(alignment: .leading, spacing: 2) {
                        HStack {
                            Text("Status")
                                .accessibilityHidden(true)
                            Spacer()
                            Button("Wake") { model.wake(deviceID) }
                                .buttonStyle(.borderedProminent)
                                .controlSize(.small)
                                .disabled(offer == .disabled)
                        }
                        OfflineStatus(text: text, waking: waking, moon: moon)
                            .foregroundStyle(.secondary)
                            .accessibilityElement(children: .ignore)
                            .accessibilityLabel(Text("Status"))
                            .accessibilityValue(Text(text))
                            .accessibilitySortPriority(1)
                    }
                    .accessibilityElement(children: .contain)
                }
                if let note {
                    WakeNote(note: note, name: device.name)
                }
            }
        }
    }
}

/// The status of another computer taken offline for now: the slashed
/// network and "Offline for now", with which computer took it offline
/// beneath; then, while it runs InputWeave 1.0 itself, a warning for each
/// other computer of the group still on 1.0, which knows nothing of it and
/// still controls it.
private struct OfflineForNowStatus: View {
    @Environment(AgentModel.self) private var model
    let device: DeviceStatus

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            LabeledContent("Status") {
                VStack(alignment: .trailing, spacing: 2) {
                    HStack(spacing: 6) {
                        // The words say it.
                        Image(systemName: OfflineText.symbol)
                            .accessibilityHidden(true)
                        Text(OfflineText.status())
                    }
                    Text(OfflineText.setBy(device, thisDeviceID: model.thisDeviceID))
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.trailing)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            ForEach(OfflineText.unaware(for: device, status: model.status), id: \.id) { older in
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Image(systemName: "exclamationmark.triangle.fill")
                        .foregroundStyle(.orange)
                        .accessibilityHidden(true)
                    Text(OfflineText.unawareWarning(name: older.name))
                        .fixedSize(horizontal: false, vertical: true)
                }
                .font(.callout)
            }
        }
    }
}

/// An offline computer's status, after a spinner while it is being woken,
/// or the moon while it can be woken.
private struct OfflineStatus: View {
    let text: String
    let waking: Bool
    let moon: Bool

    var body: some View {
        HStack(spacing: 6) {
            // The words say it.
            if waking {
                ProgressView()
                    .controlSize(.small)
                    .accessibilityHidden(true)
            } else if moon {
                Image(systemName: WakeText.moon)
                    .accessibilityHidden(true)
            }
            Text(text)
                .monospacedDigit()
        }
    }
}

/// The note under an offline computer's status: that it didn't wake up,
/// with a link on turning wake for network access on, or that it can't be
/// woken yet.
private struct WakeNote: View {
    let note: WakeText.Note
    let name: String

    var body: some View {
        switch note {
        case .notWoken:
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(.orange)
                    .accessibilityHidden(true)
                // The link follows the words, and opens in the browser.
                Text(linked)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .font(.callout)
        case .cannotWake:
            Text(WakeText.text(note, name: name))
                .foregroundStyle(.secondary)
                .font(.callout)
                .fixedSize(horizontal: false, vertical: true)
        }
    }

    private var linked: AttributedString {
        var link = AttributedString(WakeText.helpLink())
        link.link = WakeText.helpURL()
        return AttributedString(WakeText.text(note, name: name) + " ") + link
    }
}
