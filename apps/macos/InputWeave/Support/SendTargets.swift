import Foundation

/// Where files sent from Finder's Services menu (Send with InputWeave) go:
/// the computers that can take them, why the others can't, and the words
/// for the choice. The background service decides whether a computer is
/// connected and whether files are turned off with it; this only reads
/// that. The Windows app's `SendTargets` and the Linux app's `send_choice`
/// decide the same way.
enum SendTargets {
    /// Why files can't go to a computer now.
    enum Unavailable: Equatable {
        /// It is not connected.
        case offline
        /// File transfer with it is turned off on this Mac
        /// (`SetDeviceSharing`).
        case filesOff
    }

    /// Why files can't go to `device` now, the first reason that applies;
    /// nil when they can.
    static func unavailable(_ device: DeviceStatus) -> Unavailable? {
        if device.state != .connected { return .offline }
        if device.filesOff { return .filesOff }
        return nil
    }

    /// What the chooser says under a computer files can't go to.
    static func reason(_ why: Unavailable, bundle: Bundle = .main) -> String {
        switch why {
        case .offline: String(localized: "Offline", bundle: bundle)
        case .filesOff: String(localized: "File transfer with this computer is off", bundle: bundle)
        }
    }

    /// A computer in the chooser.
    struct Target: Equatable, Identifiable {
        let id: String
        let name: String
        let os: String
        /// "Connected · 3 ms" for one that can take files, else why not.
        let status: String
        let available: Bool
    }

    /// Every other computer, those files can go to first, each group in the
    /// service's order.
    static func targets(_ devices: [DeviceStatus], bundle: Bundle = .main) -> [Target] {
        let all = devices.map { device in
            let why = unavailable(device)
            let status = why.map { reason($0, bundle: bundle) } ?? ConnectionText.connected(
                rttMs: device.rttMs, screenLocked: device.screenLocked, lockScreenInput: device.lockScreenInput,
                inputUnavailable: device.inputUnavailable, sharingPaused: device.sharingPaused, bundle: bundle)
            return Target(id: device.id, name: device.name, os: device.os, status: status, available: why == nil)
        }
        return all.filter(\.available) + all.filter { !$0.available }
    }

    /// What sending does with the computers there are.
    enum Choice: Equatable {
        /// No computer is connected: say so, with only OK.
        case nowhere
        /// Exactly one can take files: send to it at once.
        case only(String)
        /// Ask which, listing every other computer: more than one can take
        /// files, or computers are connected but none can, each with why
        /// (and Send off).
        case ask
    }

    static func choice(_ devices: [DeviceStatus]) -> Choice {
        guard devices.contains(where: { $0.state == .connected }) else { return .nowhere }
        let available = devices.filter { unavailable($0) == nil }
        return available.count == 1 ? .only(available[0].id) : .ask
    }

    /// The computer the chooser shows chosen as the computers change: the
    /// one the user clicked (`clicked`) while files can go to it, which
    /// comes back when it can again; else the one chosen until now
    /// (`shown`, by the user or by default) while it can; else the first
    /// that can take files; nil when none can. So a computer that connects
    /// ahead of the one shown never takes its place, and a default the
    /// choice moved away from does not come back.
    static func chosen(clicked: String?, shown: String?, in targets: [Target]) -> String? {
        let can = { (id: String?) in targets.first { $0.id == id && $0.available }?.id }
        return can(clicked) ?? can(shown) ?? targets.first(where: \.available)?.id
    }

    /// The items of a request that arrives while the chooser is open, added
    /// to those it already has: each file once (by its standardized path),
    /// in the order they came.
    static func merged(_ open: [URL], _ more: [URL]) -> [URL] {
        var seen = Set(open.map(\.standardizedFileURL.path))
        var all = open
        for url in more where seen.insert(url.standardizedFileURL.path).inserted {
            all.append(url)
        }
        return all
    }

    /// Under the chooser's title: "Report.pdf", "Report.pdf and 1 more
    /// item", "Report.pdf and 2 more items".
    static func summary(_ urls: [URL], bundle: Bundle = .main) -> String {
        guard let first = urls.first else { return "" }
        let name = first.lastPathComponent.isEmpty ? first.path : first.lastPathComponent
        let more = urls.count - 1
        switch more {
        case 0: return name
        case 1: return String(localized: "\(name) and 1 more item", bundle: bundle)
        default: return String(localized: "\(name) and \(more) more items", bundle: bundle)
        }
    }

    /// The HUD once the files are on their way: "Sending 2 items to
    /// Windows Desktop…".
    static func sending(items: Int, to name: String, bundle: Bundle = .main) -> String {
        items == 1
            ? String(localized: "Sending 1 item to \(name)…", bundle: bundle)
            : String(localized: "Sending \(items) items to \(name)…", bundle: bundle)
    }
}
