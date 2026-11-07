import Foundation

/// The panel's headline, the profile capsule after it, and the subtitle.
enum PanelText {
    @MainActor
    static func title(_ model: AgentModel) -> String {
        switch model.link {
        case .incompatible:
            return String(localized: "Updating the background service…")
        case .connecting:
            if model.isUnresponsive(at: .now) {
                return String(localized: "The background service is not responding.")
            }
            return String(localized: "Starting the background service…")
        case .connected:
            break
        }
        let status = model.status
        // A paused profile keeps this computer off the network altogether,
        // whatever else holds.
        if let profile = model.profileInUse, profile.paused {
            return ProfileText.pausedTitle(profile)
        }
        // Offline for now, this computer neither controls another nor is
        // controlled, whatever sharing says.
        if model.offlineHere {
            return OfflineText.panelTitle()
        }
        // Pausing keeps the lock, but nothing crosses while paused anyway;
        // and this device's own lock means nothing while another controls it.
        if !status.sharing {
            return String(localized: "Sharing paused")
        }
        if !status.controlledByDeviceID.isEmpty {
            return String(localized: "Controlled by \(model.name(of: status.controlledByDeviceID))")
        }
        if !status.relativeMouse.isEmpty {
            return String(localized: "Relative mouse on \(model.name(of: status.relativeMouse))")
        }
        if status.locked {
            return String(localized: "Locked to \(model.name(of: model.cursorDeviceID))")
        }
        if model.controllingOther {
            return String(localized: "Controlling \(model.name(of: status.activeDeviceID))")
        }
        return String(localized: "Sharing keyboard and mouse")
    }

    /// The profile in use, in a capsule after every title but a paused
    /// profile's, which names it; none until the service answers.
    @MainActor
    static func capsule(_ model: AgentModel) -> String? {
        guard model.link == .connected else { return nil }
        return ProfileText.capsule(model.status)
    }

    @MainActor
    static func subtitle(_ model: AgentModel) -> String? {
        guard model.link == .connected else { return nil }
        if model.profilePaused {
            // Only when Automatic picked Other networks for a network in no
            // profile, with a way to add it to one; a profile the user chose
            // says nothing more.
            return ProfileText.offersAddingNetwork(model.status) ? ProfileText.notInAnyProfile() : nil
        }
        if model.offlineHere {
            return OfflineText.panelSubtitle(model.status)
        }
        if !model.status.controlledByDeviceID.isEmpty {
            return String(localized: "Move this computer's mouse to take it back.")
        }
        if model.status.sharing && !model.status.relativeMouse.isEmpty {
            if let shortcut = model.relativeShortcut {
                return String(localized: "Press \(shortcut) to end relative mouse.")
            }
            return model.takeBackHint
        }
        if model.status.sharing && model.status.locked {
            return String(localized: "Press \(model.lockShortcut) to unlock.")
        }
        if model.status.sharing && model.controllingOther {
            return model.takeBackHint
        }
        if model.status.sharing && !model.status.thisDeviceName.isEmpty {
            return String(localized: "Cursor on \(model.status.thisDeviceName)")
        }
        return nil
    }
}
