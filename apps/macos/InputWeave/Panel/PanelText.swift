import Foundation

/// The panel's headline and subtitle.
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

    @MainActor
    static func subtitle(_ model: AgentModel) -> String? {
        guard model.link == .connected else { return nil }
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
