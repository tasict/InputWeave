import Foundation
import os

typealias AgentProfile = Inputweave_App_V1_Profile
typealias AgentNetwork = Inputweave_App_V1_Network

/// What the app shows about profiles (`Status.profiles`): named sets of
/// settings the background service picks by the network this Mac is on,
/// unless the user picks one. The service decides all of it: which profile
/// is in use, which computers it leaves out (`DeviceStatus.not_in_profile`,
/// listed as offline), and that a paused one connects to none, when every
/// other computer is plainly offline. This only names profiles and networks
/// and picks the words: the Profiles page, the panel's capsule and paused
/// title, the ⋯ menu's submenu, and the HUD when the profile in use changes
/// by itself. A service from before (`Status.profiles_supported` false) has
/// nothing of it shown.
enum ProfileText {
    static let log = Logger(subsystem: "cc.ctsite.inputweave", category: "profiles")
    /// The Profiles page's symbol in the sidebar, also beside the status of
    /// a computer the profile in use leaves out.
    static let symbol = "square.stack.3d.up"

    /// The profile in use; nil from a service from before.
    static func inUse(_ status: AgentStatus) -> AgentProfile? {
        guard status.profilesSupported else { return nil }
        return status.profiles.first { $0.id == status.profileID }
    }

    /// Whether the profile in use is paused: this Mac neither shows up on
    /// the network nor connects to any computer then.
    static func paused(_ status: AgentStatus) -> Bool {
        inUse(status)?.paused == true
    }

    /// Whether the profile in use sends and receives no files.
    static func filesOff(_ status: AgentStatus) -> Bool {
        inUse(status).map { !$0.files } ?? false
    }

    /// Whether `device` shows as not in this profile: the profile in use
    /// does not connect it. The service says so only after a computer
    /// taken offline for now, and never while the profile is paused or
    /// this Mac is offline for now, when every other computer is just
    /// offline.
    static func shows(_ device: DeviceStatus?, status: AgentStatus) -> Bool {
        status.profilesSupported && device?.notInProfile == true
    }

    /// Whether another computer is kept away by the profile in use: left
    /// out of it, or any while it is paused. Its menu then has only
    /// Rename…, and it can be neither woken nor taken offline.
    static func keepsAway(_ device: DeviceStatus?, status: AgentStatus) -> Bool {
        guard device != nil else { return false }
        return shows(device, status: status) || paused(status)
    }

    /// The status of a computer the profile in use leaves out.
    static func notInProfile(bundle: Bundle = .main) -> String {
        String(localized: "Not in this profile", bundle: bundle)
    }

    /// Default and Other networks, which can be neither renamed nor
    /// removed.
    static func isFixed(_ profile: AgentProfile) -> Bool {
        profile.kind == .default || profile.kind == .otherNetworks
    }

    /// What to call a profile: the two fixed ones by the app's words, the
    /// user's own by the name they gave it.
    static func name(_ profile: AgentProfile, bundle: Bundle = .main) -> String {
        switch profile.kind {
        case .default: String(localized: "Default", bundle: bundle)
        case .otherNetworks: String(localized: "Other networks", bundle: bundle)
        default: profile.name
        }
    }

    /// The base name of a profile added on the Profiles page; the service
    /// makes it unique with " 2", " 3"….
    static func newName(bundle: Bundle = .main) -> String {
        String(localized: "New Profile", bundle: bundle)
    }

    /// What a rename the service refused does at the name field.
    struct RenameRefused: Equatable {
        /// Said under the field until the user edits the name again; none
        /// for a refusal the app only logs.
        let message: String?
        /// The field shows the profile's name again.
        let restoresName: Bool
    }

    /// What it does for the refusal `code` (nil when the request failed
    /// otherwise), while the user is typing in the field (`typing`) or
    /// not: a name another profile has ("name_taken") is said so and taken
    /// back; anything else only takes the name back once the user left the
    /// field, so what they type is not lost.
    static func renameRefused(_ code: String?, typing: Bool, bundle: Bundle = .main) -> RenameRefused {
        if code == "name_taken" {
            return RenameRefused(
                message: String(localized: "Another profile already has this name.", bundle: bundle), restoresName: true)
        }
        return RenameRefused(message: nil, restoresName: !typing)
    }

    /// Whether the message under the name field still stands with `typed`
    /// in it: until the user edits the name the field showed again after
    /// the refusal (`restored`).
    static func keepsRenameMessage(typed: String, restored: String) -> Bool {
        typed == restored
    }

    /// "Automatic" in the ⋯ menu's submenu.
    static func automatic(bundle: Bundle = .main) -> String {
        String(localized: "Automatic", bundle: bundle)
    }

    /// Automatic on the Profiles page, with the profile it uses now. While
    /// the user has chosen a profile, which one Automatic would pick is
    /// not known here, so it is Automatic alone.
    static func automaticNow(_ status: AgentStatus, bundle: Bundle = .main) -> String {
        guard !status.profileChosen, let profile = inUse(status) else { return automatic(bundle: bundle) }
        let name = name(profile, bundle: bundle)
        return String(localized: "Automatic (now: \(name))", bundle: bundle)
    }

    /// How a network is shown: by its name, with its router beneath; by
    /// its router alone while it has no name; by its kind with neither
    /// ("Wired network", "Wi-Fi network", "VPN").
    struct NetworkLabel: Equatable {
        let title: String
        let subtitle: String?
        let symbol: String
    }

    static func label(_ network: AgentNetwork, bundle: Bundle = .main) -> NetworkLabel {
        let router = network.router
        let symbol = symbol(network.kind)
        if !network.name.isEmpty {
            return NetworkLabel(
                title: network.name,
                subtitle: router.isEmpty ? nil : String(localized: "Router \(router)", bundle: bundle),
                symbol: symbol)
        }
        if router.isEmpty {
            return NetworkLabel(title: kindName(network.kind, bundle: bundle), subtitle: nil, symbol: symbol)
        }
        return NetworkLabel(title: String(localized: "Router \(router)", bundle: bundle), subtitle: nil, symbol: symbol)
    }

    /// What to call a network with neither a name nor a router: by its
    /// kind, one of a kind not known as wired, as the other apps do.
    static func kindName(_ kind: Inputweave_App_V1_NetworkKind, bundle: Bundle = .main) -> String {
        switch kind {
        case .wifi: String(localized: "Wi-Fi network", bundle: bundle)
        case .vpn: "VPN"
        default: String(localized: "Wired network", bundle: bundle)
        }
    }

    /// The symbol before a network: Wi-Fi, a lock for a VPN, else a cable,
    /// as it is called.
    static func symbol(_ kind: Inputweave_App_V1_NetworkKind) -> String {
        switch kind {
        case .wifi: "wifi"
        case .vpn: "lock.shield"
        default: "cable.connector.horizontal"
        }
    }

    /// The network this Mac is on, as the Profiles page shows it: "Current
    /// network: Office-5G"; nil while it is on none.
    static func current(_ status: AgentStatus, bundle: Bundle = .main) -> NetworkLabel? {
        guard status.hasNetwork else { return nil }
        let label = label(status.network, bundle: bundle)
        return NetworkLabel(
            title: String(localized: "Current network: \(label.title)", bundle: bundle),
            subtitle: label.subtitle, symbol: label.symbol)
    }

    /// Said in place of the current network while this Mac is on none.
    static func noNetwork(bundle: Bundle = .main) -> String {
        String(localized: "Not connected to a network.", bundle: bundle)
    }

    /// The profile that lists the network with the id `network`, if any;
    /// a network is in one profile at most.
    static func holder(of network: String, in status: AgentStatus) -> AgentProfile? {
        status.profiles.first { profile in profile.networks.contains { $0.id == network } }
    }

    /// The profiles a network can be added to: every one but Other
    /// networks, which is used on every network not in another.
    static func addable(_ status: AgentStatus) -> [AgentProfile] {
        status.profiles.filter { $0.kind != .otherNetworks }
    }

    /// Whether the network this Mac is on can be added to a profile now:
    /// there is one, and the service knows it by more than its router's
    /// address, which it does a few seconds after joining it; until then
    /// it refuses to add it ("network_unresolved").
    static func canAddNetwork(_ status: AgentStatus) -> Bool {
        status.hasNetwork && status.network.resolved
    }

    /// Whether the network this Mac is on can be added to `profile`: it can
    /// be added now, the profile lists networks, and it is not in it
    /// already.
    static func canAddCurrent(to profile: AgentProfile, status: AgentStatus) -> Bool {
        guard canAddNetwork(status), profile.kind != .otherNetworks else { return false }
        return !profile.networks.contains { $0.id == status.network.id }
    }

    // MARK: - The panel

    /// The capsule after the panel's title: the profile in use. None while
    /// it is paused, when the title names it, and none from a service from
    /// before.
    static func capsule(_ status: AgentStatus, bundle: Bundle = .main) -> String? {
        guard let profile = inUse(status), !profile.paused else { return nil }
        return name(profile, bundle: bundle)
    }

    /// The panel's title while the profile in use is paused: "Other
    /// networks: sharing paused".
    static func pausedTitle(_ profile: AgentProfile, bundle: Bundle = .main) -> String {
        let name = name(profile, bundle: bundle)
        return String(localized: "\(name): sharing paused", bundle: bundle)
    }

    /// Whether the panel offers to add the network this Mac is on to a
    /// profile: Automatic picked Other networks for it, as it is in no
    /// other profile, and that one is paused. Not while the user chose
    /// that profile.
    static func offersAddingNetwork(_ status: AgentStatus) -> Bool {
        guard let profile = inUse(status) else { return false }
        return profile.paused && profile.kind == .otherNetworks && !status.profileChosen && status.hasNetwork
    }

    /// The line under the paused title when the panel offers that.
    static func notInAnyProfile(bundle: Bundle = .main) -> String {
        String(localized: "This network isn’t in any profile.", bundle: bundle)
    }

    // MARK: - The HUD

    /// Follows the times the profile in use changed by itself
    /// (`Status.profile_switches`) from status to status for the HUD: news
    /// is the profile changed to. The first status after connecting to the
    /// service, a restarted one too, is only remembered, as is a count lower
    /// than before, from a service that started over.
    struct Watch {
        /// The count in the last status; nil while there was none since
        /// connecting.
        private var seen: UInt64?

        /// Takes in the latest status, nil while the background service is
        /// out of reach, and returns the profile it changed to, if it did.
        mutating func update(_ status: AgentStatus?) -> AgentProfile? {
            guard let status else {
                seen = nil
                return nil
            }
            let before = seen
            seen = status.profileSwitches
            guard status.profilesSupported, let before, status.profileSwitches > before else { return nil }
            return inUse(status)
        }
    }

    /// The HUD's words as the profile in use changed by itself to
    /// `profile`, and whether sharing is paused in it.
    static func switched(to profile: AgentProfile, bundle: Bundle = .main) -> String {
        let name = name(profile, bundle: bundle)
        if profile.paused {
            return String(localized: "Switched to \(name). Sharing is paused.", bundle: bundle)
        }
        return String(localized: "Switched to \(name).", bundle: bundle)
    }
}

extension AgentModel {
    /// Whether the service knows profiles; one from before has nothing of
    /// them shown.
    var profilesSupported: Bool {
        status.profilesSupported
    }

    /// The profile in use.
    var profileInUse: AgentProfile? {
        ProfileText.inUse(status)
    }

    /// Whether the profile in use is paused.
    var profilePaused: Bool {
        ProfileText.paused(status)
    }

    /// Whether a computer shows as not in this profile.
    func showsNotInProfile(_ id: String) -> Bool {
        ProfileText.shows(device(id), status: status)
    }

    /// Whether the profile in use keeps another computer away: not in it,
    /// or any while it is paused.
    func keptAway(_ id: String) -> Bool {
        ProfileText.keepsAway(device(id), status: status)
    }

    /// Sends a profile command, with no confirmation: each is easily
    /// changed back. A refusal is only logged; the status shows how things
    /// stand.
    func changeProfiles(_ what: String, _ command: @escaping @MainActor (AgentModel) async throws -> Void) {
        Task {
            do {
                try await command(self)
            } catch let refusal as AgentRefusal {
                ProfileText.log.notice("\(what, privacy: .public) was refused: \(refusal.code, privacy: .public) \(refusal.detail, privacy: .public)")
            } catch {
                ProfileText.log.notice("\(what, privacy: .public) failed: \(error.localizedDescription, privacy: .public)")
            }
        }
    }
}
