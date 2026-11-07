import Foundation
import os

typealias OfflineChange = Inputweave_App_V1_OfflineChange

/// What the app shows about computers taken offline for now
/// (`SetOfflineForNow`): no computer of the group controls one or is
/// controlled by it, and none shares the clipboard or files with it, until
/// someone brings it back. The background service decides all of it: it
/// lists such a computer as offline, and every other computer too while
/// this one is offline for now (`DeviceStatus.state`), so the canvas, the
/// mini map, jumping and sending treat them as offline already. This only
/// picks what to show: its status and symbol, who took it offline, the
/// warning about computers still on InputWeave 1.0, the inspector's button
/// and the menus' items, the panel while this computer is offline for now,
/// and the HUD's news. A service from before (`Status.offline_supported`
/// false) has nothing of it shown.
enum OfflineText {
    static let log = Logger(subsystem: "cc.ctsite.inputweave", category: "offline")
    /// The symbol of a computer taken offline for now: after its name, and
    /// beside its status.
    static let symbol = "network.slash"

    /// Whether this computer is offline for now.
    static func here(_ status: AgentStatus) -> Bool {
        status.offlineSupported && status.offlineForNow
    }

    /// Whether `device` shows as taken offline for now. Not while this
    /// computer is: every other computer is plainly offline then, the way
    /// a computer not reached is.
    static func shows(_ device: DeviceStatus?, status: AgentStatus) -> Bool {
        status.offlineSupported && !status.offlineForNow && device?.offlineForNow == true
    }

    /// The status of a computer taken offline for now; no round-trip time,
    /// as nothing goes between them.
    static func status(bundle: Bundle = .main) -> String {
        String(localized: "Offline for now", bundle: bundle)
    }

    /// Whether `id`, a device that took a computer offline, is this
    /// computer, `this`. An empty id, a device the service does not know,
    /// never is.
    static func isThis(_ id: String, _ this: String) -> Bool {
        !id.isEmpty && id == this
    }

    /// The line under that status: which computer took `device` offline,
    /// by what this one calls it, that this one did, or that another did
    /// when the service does not know which.
    static func setBy(_ device: DeviceStatus, thisDeviceID: String, bundle: Bundle = .main) -> String {
        if isThis(device.offlineByDeviceID, thisDeviceID) {
            return String(localized: "Set on this computer.", bundle: bundle)
        }
        let name = device.offlineByName
        if name.isEmpty {
            return String(localized: "Set by another computer.", bundle: bundle)
        }
        return String(localized: "Set by \(name).", bundle: bundle)
    }

    /// The computers to warn about under the status of `device`: those of
    /// the group that are connected and still run InputWeave 1.0, which
    /// still control it. Only while it is taken offline for now and runs
    /// 1.0 itself: a computer on 1.1 keeps those away on its own, and
    /// when it is the only one on 1.0, the others keep off it.
    static func unaware(for device: DeviceStatus, status: AgentStatus) -> [DeviceStatus] {
        guard device.offlineForNow && device.offlineUnaware else { return [] }
        return status.devices.filter { $0.offlineUnaware && $0.id != device.id }
    }

    /// The warning about one of them, the computer called `name`, under
    /// the status of a computer taken offline for now.
    static func unawareWarning(name: String, bundle: Bundle = .main) -> String {
        String(
            localized: "\(name) runs an older InputWeave that doesn’t know about this and still controls it. Update \(name) to 1.1.",
            bundle: bundle)
    }

    /// What the inspector's button and the canvas menu offer for another
    /// computer.
    enum Offer: Equatable {
        /// Nothing: this computer, one the service does not know, a service
        /// from before, or this computer offline for now, which can only
        /// bring itself back.
        case hidden
        case takeOffline
        case bringBack
    }

    static func offer(_ device: DeviceStatus?, status: AgentStatus) -> Offer {
        guard status.offlineSupported, !status.offlineForNow, let device, device.id != status.thisDeviceID else {
            return .hidden
        }
        return device.offlineForNow ? .bringBack : .takeOffline
    }

    /// The words of the button or menu item for `offer`.
    static func label(_ offer: Offer, bundle: Bundle = .main) -> String? {
        switch offer {
        case .hidden: nil
        case .takeOffline: String(localized: "Take Offline", bundle: bundle)
        case .bringBack: bringBack(bundle: bundle)
        }
    }

    /// Bringing back a computer taken offline for now: another one in the
    /// inspector and the canvas menu, this one in the panel.
    static func bringBack(bundle: Bundle = .main) -> String {
        String(localized: "Bring Back Online", bundle: bundle)
    }

    /// The note under the inspector's button, the same either way.
    static func help(bundle: Bundle = .main) -> String {
        String(
            localized: "None of your computers control it or share the clipboard and files with it until someone brings it back. It can bring itself back too.",
            bundle: bundle)
    }

    /// The panel's ⋯ menu item for this computer: taking it offline, or
    /// bringing it back while it is. None from a service from before.
    static func thisComputerItem(_ status: AgentStatus, bundle: Bundle = .main) -> String? {
        guard status.offlineSupported else { return nil }
        if status.offlineForNow {
            return bringBack(bundle: bundle)
        }
        return String(localized: "Take This Computer Offline", bundle: bundle)
    }

    /// The panel's title while this computer is offline for now.
    static func panelTitle(bundle: Bundle = .main) -> String {
        String(localized: "This computer is offline for now", bundle: bundle)
    }

    /// The line under it: which computer took this one offline, that this
    /// one did, or that another did when the service does not know which,
    /// and what that means.
    static func panelSubtitle(_ status: AgentStatus, bundle: Bundle = .main) -> String {
        if isThis(status.offlineByDeviceID, status.thisDeviceID) {
            return String(
                localized: "Set on this computer. Your other computers don’t control this one or share the clipboard and files with it.",
                bundle: bundle)
        }
        let name = status.offlineByName
        if name.isEmpty {
            return String(
                localized: "Set by another computer. Your other computers don’t control this one or share the clipboard and files with it.",
                bundle: bundle)
        }
        return String(
            localized: "Set by \(name). Your other computers don’t control this one or share the clipboard and files with it.",
            bundle: bundle)
    }

    /// The computers the panel and Troubleshooting say can't be reached:
    /// the offline ones, but not one taken offline for now, which is meant
    /// to be, and none while this computer is offline for now, when every
    /// other computer shows offline.
    static func unreachable(_ status: AgentStatus) -> [DeviceStatus] {
        if here(status) { return [] }
        return status.devices.filter { $0.state != .connected && !$0.offlineForNow }
    }

    /// News about computers taken offline for now for the HUD.
    enum News: Equatable {
        /// This computer took `device` offline for now, or itself.
        case offline(device: String)
        /// This computer brought `device`, another, back.
        case back(device: String)
        /// Another computer took this one offline for now: `name` is what
        /// this computer calls it, as the change says (`OfflineChange.by_name`);
        /// empty when the service knows no name for it.
        case takenOffline(name: String)
    }

    /// What `change`, counted in `status`, tells: what this computer did,
    /// except bringing itself back, which needs no word; and another
    /// computer taking this one offline.
    static func news(_ change: OfflineChange, status: AgentStatus) -> News? {
        let this = status.thisDeviceID
        if isThis(change.byDeviceID, this) {
            if change.offline {
                return .offline(device: change.deviceID)
            }
            return isThis(change.deviceID, this) ? nil : .back(device: change.deviceID)
        }
        if isThis(change.deviceID, this) && change.offline {
            return .takenOffline(name: change.byName)
        }
        return nil
    }

    /// Follows the changes the service counts (`Status.offline_changes`)
    /// from status to status for the HUD. The first status after
    /// connecting to the service, a restarted one too, is only remembered,
    /// as is a count lower than before, from a service that started over.
    struct Watch {
        /// The count in the last status; nil while there was none since
        /// connecting.
        private var seen: UInt64?

        /// Takes in the latest status, nil while the background service is
        /// out of reach, and returns what to tell.
        mutating func update(_ status: AgentStatus?) -> News? {
            guard let status else {
                seen = nil
                return nil
            }
            let before = seen
            seen = status.offlineChanges
            guard status.offlineSupported, let before, status.offlineChanges > before else { return nil }
            return news(status.lastOfflineChange, status: status)
        }
    }

    /// The HUD's words for `news`, about the computer called `name`: the
    /// one taken offline or brought back, or the one that took this one
    /// offline, empty when the service does not know which.
    static func message(_ news: News, name: String, bundle: Bundle = .main) -> String {
        switch news {
        case .offline: String(localized: "\(name) is offline for now.", bundle: bundle)
        case .back: String(localized: "\(name) is back online.", bundle: bundle)
        case .takenOffline:
            name.isEmpty
                ? String(localized: "Another computer took this computer offline for now.", bundle: bundle)
                : String(localized: "\(name) took this computer offline for now.", bundle: bundle)
        }
    }

    /// The HUD's words as the keyboard and mouse came back from the
    /// computer called `name`, taken offline for now while this one
    /// controlled it, here or elsewhere.
    static func wentOffline(_ name: String, bundle: Bundle = .main) -> String {
        String(
            localized: "“\(name)” went offline for now. The keyboard and mouse are back on this computer.",
            bundle: bundle)
    }
}

extension AgentModel {
    /// Whether the service knows computers taken offline for now; one from
    /// before has nothing of it shown.
    var offlineSupported: Bool {
        status.offlineSupported
    }

    /// Whether this computer is offline for now.
    var offlineHere: Bool {
        OfflineText.here(status)
    }

    /// Whether a computer shows as taken offline for now.
    func showsOfflineForNow(_ id: String) -> Bool {
        OfflineText.shows(device(id), status: status)
    }

    /// What the inspector and the canvas menu offer for a computer.
    func offlineOffer(_ id: String) -> OfflineText.Offer {
        OfflineText.offer(device(id), status: status)
    }

    /// The computers the panel and Troubleshooting say can't be reached.
    var unreachableDevices: [DeviceStatus] {
        OfflineText.unreachable(status)
    }

    /// Whether the cursor can be locked or unlocked from the panel's ⋯
    /// menu: not while another computer controls this one, where its lock
    /// holds the cursor, nor while sharing is paused or this computer is
    /// offline for now, when it goes nowhere anyway.
    var canToggleLock: Bool {
        link == .connected && status.sharing && status.controlledByDeviceID.isEmpty && !offlineHere
    }

    /// The name the HUD gives for `news`: for this computer taken offline
    /// by another, the name the change itself gives that one, empty when
    /// the service knows none.
    func name(of news: OfflineText.News) -> String {
        switch news {
        case .offline(let device), .back(let device): name(of: device)
        case .takenOffline(let name): name
        }
    }

    /// Takes a computer, this one too, offline for now or brings it back,
    /// with no confirmation: it is easily undone, and the HUD tells what
    /// happened. A refusal is only logged; the status shows how things
    /// stand.
    func setOffline(_ id: String, _ offline: Bool) {
        Task {
            do {
                try await setOfflineForNow(id, offline)
            } catch let refusal as AgentRefusal {
                OfflineText.log.notice("Taking \(id, privacy: .public) offline (\(offline)) was refused: \(refusal.code, privacy: .public) \(refusal.detail, privacy: .public)")
            } catch {
                OfflineText.log.notice("Taking \(id, privacy: .public) offline (\(offline)) failed: \(error.localizedDescription, privacy: .public)")
            }
        }
    }
}
