import Foundation

typealias LicensedDevice = Inputweave_App_V1_LicensedDevice
typealias LicenseUse = Inputweave_App_V1_LicenseUse

/// What the license views say, apart from the views.
enum LicenseText {
    /// The buyer's email with all but its first character hidden:
    /// "t•••@gmail.com".
    static func maskedEmail(_ email: String) -> String {
        guard let at = email.firstIndex(of: "@"), at != email.startIndex else { return "" }
        return "\(email[email.startIndex])•••\(email[at...])"
    }

    /// A license is for this many computers at most.
    static let seats = 5

    // MARK: - The list of a background service from before applying

    struct Computer: Equatable, Identifiable {
        var id: String
        var name: String
        var thisComputer: Bool
    }

    /// The computers on this license that this one knows of, as a
    /// background service that does not apply licenses lists them
    /// (`License.apply_supported` false): itself, then the paired ones
    /// licensed with the same key.
    static func computers(_ license: LicenseRecord, thisName: String) -> [Computer] {
        guard license.state == .licensed else { return [] }
        return [Computer(id: "", name: thisName, thisComputer: true)]
            + license.devices.filter(\.licensed).map { Computer(id: $0.id, name: $0.name, thisComputer: false) }
    }

    // MARK: - Computers on this license

    /// How a computer stands on this computer's license, as its row says.
    enum Standing: Equatable {
        /// This computer: Licensed.
        case thisComputer
        /// Activated there with this computer's key.
        case licensed
        /// On trial there, as far as this computer knows.
        case trial
        /// Its app activates the key this computer applied.
        case activating
        /// Applied while it is offline: the key goes to it as it connects.
        case waiting
        /// Polar said every place on the license is taken.
        case noPlaces
        /// Licensed there with another key.
        case otherLicense
    }

    /// The line under a computer's name about where it is.
    enum Reach: Equatable {
        case thisComputer
        /// Connected: no line.
        case connected
        case offline
        /// Taken offline for now; still on the license, as connected.
        case offlineForNow
    }

    /// The button at the end of a row.
    enum Action: Equatable {
        case none
        case apply
        /// Revoke License…, after a confirmation.
        case revoke
        /// Don't Apply: takes back applying it, at once.
        case dontApply
    }

    struct Row: Equatable, Identifiable {
        var id: String
        var name: String
        var os: String
        var reach: Reach
        var standing: Standing
        /// It runs InputWeave 1.0 (`LicensedDevice.legacy`).
        var legacy = false
        /// Its activation at Polar, which Revoke License deactivates.
        var activationID = ""
        /// What was applied there can be taken back
        /// (`LicensedDevice.can_take_back`).
        var canTakeBack = false
        /// On InputWeave 1.0, it did not take the key applied from here
        /// (`LicensedDevice.legacy_refused`).
        var legacyRefused = false
        /// An apply from here waits for it, one it has not shown yet
        /// (`LicensedDevice.waiting_to_apply`).
        var waitingToApply = false

        /// A computer on InputWeave 1.0 cannot be told of a revoke, so it
        /// is offered none. Don't Apply takes back the key while it waits
        /// for the computer, or until the computer activated it, and shows
        /// only while it can (`canTakeBack`), here and under Waiting to
        /// Apply.
        var action: Action {
            switch standing {
            case .thisComputer, .otherLicense: .none
            case .licensed: legacy ? .none : .revoke
            case .trial, .noPlaces: .apply
            case .activating, .waiting: canTakeBack ? .dontApply : .none
            }
        }

        /// The line under its name about InputWeave 1.0: licensed there, or
        /// the key went there, which 1.0 cannot be told to drop.
        var showsLegacyNote: Bool {
            guard legacy else { return false }
            switch standing {
            case .licensed: return true
            case .activating, .waiting: return !canTakeBack
            default: return false
            }
        }

        /// The line under its name saying 1.0 did not take the license.
        var showsLegacyRefused: Bool {
            standing == .trial && legacyRefused
        }

        /// It takes one of the license's places, or will once it connects.
        var takesPlace: Bool {
            [.thisComputer, .licensed, .activating, .waiting].contains(standing)
        }
    }

    static func standing(_ use: LicenseUse) -> Standing {
        switch use {
        case .licensed: .licensed
        case .activating: .activating
        case .waiting: .waiting
        case .noPlaces: .noPlaces
        case .otherLicense: .otherLicense
        case .none, .UNRECOGNIZED: .trial
        }
    }

    /// "Computers on this license": this computer first, then every
    /// computer it trusts, in the background service's order. One the
    /// license does not list is on trial.
    static func rows(_ status: AgentStatus, thisName: String) -> [Row] {
        let listed = Dictionary(status.license.devices.map { ($0.id, $0) }, uniquingKeysWith: { first, _ in first })
        let this = Row(id: status.thisDeviceID, name: thisName, os: "macos", reach: .thisComputer, standing: .thisComputer)
        return [this] + status.devices.map { device in
            let entry = listed[device.id]
            let reach: Reach = if OfflineText.shows(device, status: status) {
                .offlineForNow
            } else if device.state == .offline {
                .offline
            } else {
                .connected
            }
            return Row(
                id: device.id, name: device.name, os: device.os, reach: reach,
                standing: standing(entry?.use ?? .none), legacy: entry?.legacy ?? false,
                activationID: entry?.activationID ?? "", canTakeBack: entry?.canTakeBack ?? false,
                legacyRefused: entry?.legacyRefused ?? false, waitingToApply: entry?.waitingToApply ?? false)
        }
    }

    /// While this computer is not licensed (seats full, invalid, or while
    /// it removes its own), the computers an apply from here still waits
    /// for, which they have not shown yet: "Waiting to Apply" on the
    /// License page. Never one waiting with a key it holds already. None
    /// while the license can't be read.
    static func waitingToApply(_ status: AgentStatus) -> [Row] {
        guard status.license.state != .unavailable else { return [] }
        return rows(status, thisName: "").filter(\.waitingToApply)
    }

    /// The "N of 5" of the list: this computer and those licensed.
    static func licensedCount(_ rows: [Row]) -> Int {
        rows.filter { $0.standing == .thisComputer || $0.standing == .licensed }.count
    }

    /// Every place is taken, or promised to a computer activating the key
    /// or waiting for it: Apply License is turned off.
    static func allPlacesUsed(_ rows: [Row]) -> Bool {
        rows.filter(\.takesPlace).count >= seats
    }

    /// Some computer found every place on the license taken.
    static func noPlaces(_ rows: [Row]) -> Bool {
        rows.contains { $0.standing == .noPlaces }
    }

    /// On trial: the first computer that could apply the license here,
    /// for the note above the buttons. Never one on InputWeave 1.0, which
    /// cannot apply it.
    static func licensedElsewhere(_ license: LicenseRecord) -> String? {
        guard license.applySupported else { return nil }
        return license.devices.first { ($0.use == .licensed || $0.use == .otherLicense) && !$0.legacy }?.name
    }

    /// Every place was taken when the key was entered here: the warning and
    /// Try Again. A key another computer applied here stays plainly on
    /// trial; that computer's list says why.
    static func seatsFullHere(_ license: LicenseRecord) -> Bool {
        license.state == .seatsFull && license.sourceDeviceID.isEmpty
    }

    /// The title of Revoke License's confirmation.
    static func revokeTitle(_ name: String, bundle: Bundle = .main) -> String {
        String(localized: "Revoke the license on \(name)?", bundle: bundle)
    }

    static func revokeMessage(_ name: String, bundle: Bundle = .main) -> String {
        String(
            localized: "\(name) goes back to trial with every feature still working, and one of your 5 places is freed. You can apply it again later.",
            bundle: bundle)
    }

    /// On trial, where to get the license from (`licensedElsewhere`).
    static func licensedElsewhereNote(_ name: String, bundle: Bundle = .main) -> String {
        String(
            localized: "\(name) is licensed. To use the license here too, apply it to this computer from \(name)’s License page.",
            bundle: bundle)
    }

    // MARK: - Removing a computer

    /// What Remove Device's confirmation says of the license.
    enum RemoveOffer: Equatable {
        case none
        /// "Also revoke its license", checked to begin with.
        case revoke
        /// It runs InputWeave 1.0: a line about freeing its place in
        /// Polar's customer portal instead.
        case legacy
    }

    /// Only while this computer is licensed (`licensedHere`), for a computer
    /// licensed with its key, activating it or waiting for it.
    static func removeOffer(_ deviceID: String, license: LicenseRecord, licensedHere: Bool) -> RemoveOffer {
        guard licensedHere, let device = license.devices.first(where: { $0.id == deviceID }),
            [.licensed, .activating, .waiting].contains(device.use) else { return .none }
        return device.legacy ? .legacy : .revoke
    }

    /// Why the license on a removed computer could not be revoked.
    enum RemoveNotice: Equatable {
        /// Licensed there, and Polar did not deactivate its activation.
        case notDeactivated
        /// Offline, and it may hold the key already: taking it back would
        /// reach it only once it connects, which it no longer does here.
        case outOfReach
        /// Polar deactivated its activation, so its place is free, but the
        /// service did not take the revoke: the computer was never told.
        case notTold

        /// Whether Polar's customer portal is offered: not once the place
        /// is free.
        var offersPortal: Bool { self != .notTold }
    }

    /// What Remove Device with "Also revoke its license" says after, for
    /// `device` as it stood: nothing when its license was revoked, or the
    /// key it never received was taken back. When the service refused the
    /// revoke, or could not be asked (`revoked` false), it was not revoked.
    /// A licensed device that never said its activation is told by the
    /// service instead, which reaches it while it is `connected`, as the
    /// service's answer shows it.
    static func removeNotice(
        _ device: LicensedDevice, deactivated: Bool, revoked: Bool = true, connected: Bool = false
    ) -> RemoveNotice? {
        guard revoked else {
            if device.use == .licensed && deactivated { return .notTold }
            return device.use == .waiting && device.keyOutOfReach ? .outOfReach : .notDeactivated
        }
        return switch device.use {
        case .licensed: deactivated || (device.activationID.isEmpty && connected) ? nil : .notDeactivated
        case .waiting: device.keyOutOfReach ? .outOfReach : nil
        default: nil
        }
    }

    static func legacyRemoveNote(_ name: String, bundle: Bundle = .main) -> String {
        String(
            localized: "\(name) runs InputWeave 1.0, so its license keeps its place after you remove it. To free it, deactivate it in Polar’s customer portal.",
            bundle: bundle)
    }

    /// The notice after a computer called `name` was removed whose license
    /// could not be revoked, for `notice`.
    static func notRevokedTitle(_ notice: RemoveNotice, name: String, bundle: Bundle = .main) -> String {
        switch notice {
        case .notDeactivated, .outOfReach:
            String(localized: "\(name) was removed, but its license couldn’t be revoked.", bundle: bundle)
        case .notTold:
            String(localized: "\(name) was removed, and its place on the license is free.", bundle: bundle)
        }
    }

    /// What to do about it, or what it means.
    static func notRevokedMessage(_ notice: RemoveNotice, bundle: Bundle = .main) -> String {
        switch notice {
        case .notDeactivated:
            String(localized: "Deactivate it in Polar’s customer portal to free its place.", bundle: bundle)
        case .outOfReach:
            String(
                localized: "It was offline. If it activates the license later, free its place in Polar’s customer portal.",
                bundle: bundle)
        case .notTold:
            String(
                localized: "It couldn’t be told, so it may still show as licensed until it next checks its license with Polar.",
                bundle: bundle)
        }
    }
}
