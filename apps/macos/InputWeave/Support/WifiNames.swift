import Foundation

/// Recognizing networks by their Wi-Fi name as well as their router
/// (`SetWifiNames`). macOS lets an app read the Wi-Fi name only with
/// location access, which this app asks for when the user turns the switch
/// on, never before; it then tells the background service the name of the
/// network it is on (`ReportWifiName`), and the service decides what comes
/// of it. This decides when to ask, when to tell, and when to turn the
/// setting off; `WifiNameReporter` does it.
enum WifiNames {
    /// Whether this app may read the Wi-Fi name.
    enum Access: Equatable {
        /// Not asked yet, or asked again after the user reset it.
        case unknown
        case granted
        /// Denied, restricted, or location services are off.
        case denied
    }

    /// What turning the switch on does.
    enum Start: Equatable {
        /// Turn the setting on at once: access was granted before.
        case turnOn
        /// Ask for location access first, and turn it on once granted.
        case ask
        /// The switch goes back off: access was denied before, and macOS
        /// does not ask again.
        case refuse
    }

    static func start(_ access: Access) -> Start {
        switch access {
        case .granted: .turnOn
        case .unknown: .ask
        case .denied: .refuse
        }
    }

    /// What the answer to asking does to the setting: on once granted;
    /// denied, back off, which takes telling the service only while it has
    /// it on. Nil while the answer is still to come.
    static func answered(_ access: Access, serviceOn: Bool) -> Bool? {
        switch access {
        case .unknown: nil
        case .granted: true
        case .denied: serviceOn ? false : nil
        }
    }

    /// What the app does about location access while the setting stands as
    /// `status` says.
    enum Step: Equatable {
        /// Nothing: the setting is off, and the app never asks then.
        case idle
        /// Read the Wi-Fi name: access is granted.
        case read
        /// Ask for access: the setting is on, but access is not decided (as
        /// after the user reset it), and the app has not asked since it
        /// opened.
        case ask
        /// Turn the setting off: the user denied access, or it is
        /// restricted.
        case turnOff
        /// Nothing until access is granted: the user is being asked, or the
        /// app asked once since it opened and access is still not decided.
        case wait
    }

    /// The step for the setting in `status` with `access`, while the user
    /// is being asked (`asking`, after turning the switch on) or not, and
    /// whether the app already asked since it opened (`askedThisLaunch`).
    static func step(_ status: AgentStatus, access: Access, asking: Bool, askedThisLaunch: Bool) -> Step {
        guard status.profilesSupported, status.wifiNames else { return .idle }
        switch access {
        case .granted: return .read
        // What the user answers when asked says what comes of it.
        case .denied: return asking ? .wait : .turnOff
        case .unknown: return asking || askedThisLaunch ? .wait : .ask
        }
    }

    /// Whether this app reads the Wi-Fi name: while the setting is on and
    /// it may.
    static func reads(_ status: AgentStatus, access: Access) -> Bool {
        status.profilesSupported && status.wifiNames && access == .granted
    }

    /// The least time between two readings of the Wi-Fi name, apart from
    /// the one when the service says this Mac is on another network, and
    /// between two reports of the same reading.
    static let interval: Duration = .seconds(5)

    /// What the Wi-Fi interface said when it was read.
    struct Sighting: Equatable {
        /// There is a Wi-Fi interface, and it is on.
        let powered: Bool
        /// It is joined to a network.
        let associated: Bool
        /// The name it gave; nil when it gave none.
        let ssid: String?
    }

    /// The name a reading gives: empty when this Mac is not on Wi-Fi (the
    /// interface is off or joined to no network), nil when the read failed,
    /// as when the interface is joined but gives no name for a moment. A
    /// failed read keeps the reading before it, as an empty name would
    /// replace a name in use.
    static func ssid(_ sighting: Sighting) -> String? {
        guard sighting.powered, sighting.associated else { return "" }
        guard let ssid = sighting.ssid, !ssid.isEmpty else { return nil }
        return ssid
    }

    /// A reading, and what the service is told: the Wi-Fi name read for the
    /// network the service said this Mac was on, empty when it is not on
    /// Wi-Fi.
    struct Report: Equatable {
        let networkID: String
        let ssid: String
    }

    /// The report sent last, and when.
    struct Sent: Equatable {
        let report: Report
        let at: Date
    }

    /// What was sent last, kept from status to status: forgotten once this
    /// app is not connected to the service, or the setting is off, whoever
    /// turned it off, so connecting again or turning it on again tells it
    /// again.
    static func kept(_ sent: Sent?, status: AgentStatus, connected: Bool) -> Sent? {
        guard connected, status.profilesSupported, status.wifiNames else { return nil }
        return sent
    }

    /// Whether the service holds a Wi-Fi name, any, for the network it says
    /// this Mac is on (`Status.wifi_name_held`).
    static func held(_ status: AgentStatus) -> Bool {
        status.hasWifiNameHeld && status.wifiNameHeld.networkID == status.network.id
    }

    /// What to tell the service at `now`, if anything: the reading for the
    /// network it says this Mac is on, at once when it is not what was sent
    /// last; the same again only while the service holds nothing for that
    /// network (it may have come while the service waited to tell whether
    /// the network changed), at most every `interval`. Once the service
    /// holds something for the network, even another name (it may have
    /// refused an empty one), nothing more until the reading changes.
    static func report(_ status: AgentStatus, access: Access, reading: Report?, sent: Sent?, now: Date) -> Report? {
        guard reads(status, access: access), status.hasNetwork, !status.network.id.isEmpty,
            let reading, reading.networkID == status.network.id
        else {
            return nil
        }
        guard let sent, sent.report == reading else { return reading }
        if held(status) { return nil }
        let seconds = Double(interval.components.seconds)
        return now.timeIntervalSince(sent.at) >= seconds ? reading : nil
    }
}
