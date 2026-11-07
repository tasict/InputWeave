import Foundation
import os

typealias WakeState = Inputweave_App_V1_WakeState

/// What the app shows about waking another computer from sleep over the
/// network (`WakeDevice`): its status while it is offline, Wake in the
/// inspector and the canvas menu, the note under its status, the moon
/// beside its name, and the HUD's news. The background service decides
/// whether a computer can be woken (`DeviceStatus.can_wake`) and follows
/// the waking (`DeviceStatus.wake`); this only picks what to show for it.
/// A service from before waking (`Status.wake_supported` false) has
/// nothing of it shown. Only ever about another computer: the service
/// does not list this one among the devices.
enum WakeText {
    static let log = Logger(subsystem: "cc.ctsite.inputweave", category: "wake")
    /// The symbol after the name of a computer that can be woken.
    static let moon = "moon.zzz"

    /// Wake as the inspector's button and the canvas menu offer it.
    enum Offer: Equatable {
        /// Not offered: the computer can't be woken from here.
        case hidden
        /// Shown, but not now: it is being woken, or this app asked and the
        /// service has not answered yet.
        case disabled
        case enabled
    }

    /// The note under an offline computer's status in the inspector.
    enum Note: Equatable {
        /// It was woken and did not connect: a warning, with how to turn
        /// wake for network access on.
        case notWoken
        /// It never told this computer how to wake it.
        case cannotWake
    }

    /// Whether `device` is being woken: a wake-up went to it and it has
    /// not connected since. Its status shows a spinner then.
    static func waking(_ device: DeviceStatus, supported: Bool) -> Bool {
        supported && device.state != .connected && device.wake == .waking
    }

    /// The status of `device` while it is offline, as of `now`: "Waking…"
    /// while it is being woken; how long it has been offline when it can be
    /// woken and that is known; else "Offline".
    static func offlineStatus(_ device: DeviceStatus, supported: Bool, now: Date, bundle: Bundle = .main) -> String {
        if waking(device, supported: supported) {
            return String(localized: "Waking…", bundle: bundle)
        }
        guard supported, device.canWake, device.offlineSinceUnixMs != 0 else {
            return String(localized: "Offline", bundle: bundle)
        }
        let since = Date(timeIntervalSince1970: Double(device.offlineSinceUnixMs) / 1000)
        let ago = AgoText.text(since: since, now: now, bundle: bundle)
        return String(localized: "Offline · \(ago)", bundle: bundle)
    }

    /// Whether and how Wake is offered for `device`; `requested`: this
    /// app's own request to wake it is on its way. Waking it again after
    /// it didn't wake up tries again.
    static func offer(_ device: DeviceStatus?, supported: Bool, requested: Bool) -> Offer {
        guard supported, let device, device.canWake else { return .hidden }
        return device.wake == .waking || requested ? .disabled : .enabled
    }

    /// The note under the status of `device`, if any.
    static func note(_ device: DeviceStatus, supported: Bool) -> Note? {
        guard supported else { return nil }
        if device.canWake {
            return device.wake == .notWoken ? .notWoken : nil
        }
        return device.state != .connected ? .cannotWake : nil
    }

    /// The words of `note` about the computer called `name`.
    static func text(_ note: Note, name: String, bundle: Bundle = .main) -> String {
        switch note {
        case .notWoken:
            String(localized: "\(name) didn’t wake up. Wake for network access may be off on it, or it may not be on a wired network.", bundle: bundle)
        case .cannotWake:
            String(localized: "\(name) hasn’t told this computer how to wake it yet. You can wake it after it connects again.", bundle: bundle)
        }
    }

    /// The link after the note that it didn't wake up.
    static func helpLink(bundle: Bundle = .main) -> String {
        String(localized: "How to turn on wake for network access", bundle: bundle)
    }

    /// Where that link goes: the user guide in the app's language,
    /// `localization` as the app's bundle picked it.
    static func helpURL(localization: String? = Bundle.main.preferredLocalizations.first) -> URL {
        if localization?.hasPrefix("zh") == true {
            return URL(string: "https://inputweave.ctsite.cc/zh-Hant/user-guide#wake-for-network-access")!
        }
        return URL(string: "https://inputweave.ctsite.cc/user-guide#wake-for-network-access")!
    }

    /// Whether the moon goes after the name of `device` on the canvas, the
    /// mini map and the devices list: it can be woken, whatever the waking.
    static func showsMoon(_ device: DeviceStatus?, supported: Bool) -> Bool {
        supported && device?.canWake == true
    }

    /// News about waking for the HUD.
    enum News: Equatable {
        /// A wake-up went to the computer `device`, from here or by pushing
        /// the cursor toward it.
        case waking(device: String)
        /// The computer `device` connected while it was being woken.
        case awake(device: String)

        var device: String {
            switch self {
            case .waking(let device), .awake(let device): device
            }
        }
    }

    /// Follows each computer's waking from status to status for the HUD.
    /// The first status after connecting to the service, a restarted one
    /// too, is only remembered; a computer no longer listed is forgotten.
    struct Watch {
        /// Each computer's waking in the last status; nil while there was
        /// none since connecting.
        private var last: [String: WakeState]?

        /// Takes in the latest status, nil while the background service is
        /// out of reach, and returns what to tell: for the first computer
        /// in the status that it is about, as the HUD shows one at a time.
        mutating func update(_ status: AgentStatus?) -> News? {
            guard let status else {
                last = nil
                return nil
            }
            let devices = status.devices.filter { $0.id != status.thisDeviceID }
            let before = last
            last = Dictionary(devices.map { ($0.id, $0.wake) }, uniquingKeysWith: { first, _ in first })
            guard status.wakeSupported, let before else { return nil }
            for device in devices {
                let was = before[device.id]
                if device.wake == .waking && was != .waking {
                    return .waking(device: device.id)
                }
                if was == .waking && device.state == .connected {
                    return .awake(device: device.id)
                }
            }
            return nil
        }
    }

    /// The HUD's words for `news` about the computer called `name`.
    static func message(_ news: News, name: String, bundle: Bundle = .main) -> String {
        switch news {
        case .waking: String(localized: "Waking \(name)…", bundle: bundle)
        case .awake: String(localized: "\(name) is awake.", bundle: bundle)
        }
    }
}

/// The computers this app asked the background service to wake, from
/// asking until the service answered: Wake is not offered again for them
/// meanwhile.
struct WakeRequests: Equatable {
    private(set) var devices: Set<String> = []

    func contains(_ id: String) -> Bool {
        devices.contains(id)
    }

    mutating func begin(_ id: String) {
        devices.insert(id)
    }

    mutating func end(_ id: String) {
        devices.remove(id)
    }

    /// Only the computers in `ids` are still listed: the others are
    /// forgotten.
    mutating func keep(_ ids: some Sequence<String>) {
        devices.formIntersection(ids)
    }
}

extension AgentModel {
    /// Whether the service can wake other computers; one from before
    /// waking has nothing of it shown.
    var wakeSupported: Bool {
        status.wakeSupported
    }

    /// Whether and how Wake is offered for a computer.
    func wakeOffer(_ id: String) -> WakeText.Offer {
        WakeText.offer(device(id), supported: wakeSupported, requested: wakeRequests.contains(id))
    }

    /// Asks the service to wake a computer. A refusal is only logged: the
    /// status it sends shows what happened, as the warning that it didn't
    /// wake up when nothing could be sent.
    func wake(_ id: String) {
        Task {
            do {
                try await wakeDevice(id)
            } catch let refusal as AgentRefusal {
                WakeText.log.notice("Waking \(id, privacy: .public) was refused: \(refusal.code, privacy: .public) \(refusal.detail, privacy: .public)")
            } catch {
                WakeText.log.notice("Waking \(id, privacy: .public) failed: \(error.localizedDescription, privacy: .public)")
            }
        }
    }
}
