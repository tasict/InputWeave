import Foundation
import Observation
import os
import Security

typealias LicenseRecord = Inputweave_App_V1_License

/// What the license flow needs from the background service, which keeps
/// the license record (docs/architecture.md §11): the app only asks Polar
/// and reports the answers.
@MainActor
protocol LicenseAgent: AnyObject {
    var license: LicenseRecord { get }
    /// The service is reachable, so commands can go out.
    var licenseLinked: Bool { get }
    /// What Polar shows as the activation's label.
    var thisComputerName: String { get }
    /// Returns the record as the service answered it.
    @discardableResult func setLicenseKey(_ key: String) async throws -> LicenseRecord
    func recordLicense(_ result: Inputweave_App_V1_RecordLicenseResult) async throws
    /// Removes the license, only while it is still `key` with
    /// `activationID`; refused with "license_stale" once it is not.
    func removeLicense(key: String, activationID: String) async throws
    /// Applies this computer's license to another device.
    func applyLicense(deviceID: String) async throws
    /// Revokes it there (`activationID`, the device's activation, and
    /// whether Polar deactivated it), or takes back applying it (no
    /// activation).
    func revokeLicense(deviceID: String, activationID: String, deactivated: Bool) async throws
    /// Stops trusting a device (Remove Device).
    func unpair(_ deviceID: String) async throws
    /// The device is connected, as the service last said, or taken offline
    /// for now, which keeps its connection: what the service tells it
    /// reaches it.
    func isConnected(_ deviceID: String) -> Bool
    /// Polar freed an activation of an earlier key (`License.to_free`).
    func licenseActivationFreed(_ activationID: String) async throws
}

extension AgentModel: LicenseAgent {
    var license: LicenseRecord { status.license }

    func isConnected(_ deviceID: String) -> Bool {
        device(deviceID).map { $0.state == .connected || $0.offlineForNow } ?? false
    }
    var licenseLinked: Bool { link == .connected }
    var thisComputerName: String { status.thisDeviceName.isEmpty ? "Mac" : status.thisDeviceName }
}

/// The license work the app still has to do, in order: what it owes Polar
/// and the background service. Kept across launches, so a restart never
/// activates a key twice or forgets a seat to free.
struct LicenseOutbox: Codable, Equatable, Sendable {
    enum Kind: String, Codable, Sendable {
        /// Ask Polar to activate the key the service holds.
        case activate
        /// Tell the service of an activation Polar made.
        case report
        /// Tell the service Polar refused the key (`refusal`).
        case refuse
        /// Ask Polar to free an activation.
        case free
        /// Ask the service to remove the license, while it is still `key`
        /// with `activationID`, as it was when this was queued.
        case remove
        /// Give the service a key the user entered; never kept.
        case setKey
        /// Tell the service the license was revoked on another device
        /// (`deviceID`, its `activationID`), and whether Polar deactivated
        /// that (`deactivated`): asked once before this was queued.
        case revoke
        /// Tell the service Polar freed an activation it listed to be
        /// freed (`License.to_free`).
        case freed
    }

    struct Op: Codable, Equatable, Identifiable, Sendable {
        var id = UUID().uuidString
        var kind: Kind
        var key: String
        var activationID = ""
        var email = ""
        var validatedAt: UInt64 = 0
        /// For `refuse`: SEATS_FULL or INVALID.
        var refusal = 0
        /// For `refuse`: Polar said the key is no longer active.
        var noLongerValid = false
        /// For `free`: the user removed the license here.
        var fromRemove = false
        /// For `revoke`: the device the license is revoked on, and whether
        /// Polar deactivated its activation.
        var deviceID = ""
        var deactivated = false
        /// For `free` and then `remove`: another device revoked the license
        /// here without freeing this computer's activation, so it is freed
        /// and the license removed without a word.
        var revoked = false
        /// For `free`: an activation of an earlier key the service listed to
        /// be freed (`License.to_free`), which it is told of once freed.
        var toFree = false
        /// For the user's `remove`: how many times the service said the
        /// license had changed since it was asked for (`license_stale`).
        var staleTries = 0
        /// Failed attempts, and when to try again.
        var tries = 0
        var notBefore: Date?
        /// For `activate`: the soonest the user's Activate may bring the
        /// next try forward to. An answer that was lost may still have
        /// taken a seat, so an activation is never asked for again at once.
        var soonest: Date?

        enum CodingKeys: String, CodingKey {
            case id, kind, key, activationID, email, validatedAt, refusal, noLongerValid, fromRemove, deviceID, deactivated
            case revoked, toFree, staleTries, tries, notBefore, soonest
        }

        /// The service's ops run in order, one after another; Polar's
        /// (activate, free) only need their own wait to be over.
        var isService: Bool {
            kind == .report || kind == .refuse || kind == .remove || kind == .setKey || kind == .revoke || kind == .freed
        }

        /// Goes to the service only once the store holds it: a report
        /// carries an activation a restart must not lose, and a removal
        /// the free queued with it, whose activation id the service then
        /// forgets.
        var needsKeeping: Bool { kind == .report || kind == .remove }
    }

    var ops: [Op] = []
    /// The key whose activation the service refused for good, with what
    /// its record said then: not activated again while it says the same.
    /// Dropped once the record says anything else. Also a license revoked
    /// here that the service refused to remove: not removed again while
    /// the record says the same.
    var stalled: [String: String] = [:]
    /// Polar asked for no request before this (`Retry-After`), whoever
    /// asks.
    var polarNotBefore: Date?

    enum CodingKeys: String, CodingKey {
        case ops, stalled, polarNotBefore
    }

    /// What is kept: everything but the user's typing.
    var kept: LicenseOutbox {
        LicenseOutbox(ops: ops.filter { $0.kind != .setKey }, stalled: stalled, polarNotBefore: polarNotBefore)
    }

    mutating func remove(_ op: Op) {
        ops.removeAll { $0.id == op.id }
    }

    /// Puts `next` where `op` was, so it runs next in its turn.
    mutating func replace(_ op: Op, with next: Op) {
        if let index = ops.firstIndex(where: { $0.id == op.id }) {
            ops[index] = next
        }
    }

    /// The user entered a key (`op`, a setKey), which the service starts
    /// over as waiting for activation. Nothing the service owes waits any
    /// longer, so the key goes now; a refusal from before is dropped, and
    /// an activation from before is reported after the key, so it keeps
    /// its seat. A waiting activation comes no sooner than `soonest`.
    mutating func enter(_ op: Op) {
        let key = op.key
        stalled[key] = nil
        ops.removeAll { $0.kind == .refuse && $0.key == key }
        let reports = ops.filter { $0.kind == .report && $0.key == key }
        ops.removeAll { $0.kind == .report && $0.key == key }
        ops.append(op)
        ops.append(contentsOf: reports)
        for index in ops.indices {
            if ops[index].isService {
                ops[index].notBefore = nil
            } else if ops[index].kind == .activate, ops[index].key == key,
                let soonest = ops[index].soonest, let notBefore = ops[index].notBefore, soonest < notBefore {
                ops[index].notBefore = soonest
            }
        }
    }

    /// The user removed the license (`key`, this computer's
    /// `activationID`). Nothing is activated or reported from now on: an
    /// activation Polar made that the service has not taken is freed
    /// instead, and this computer's is freed once the service removed it.
    /// Revoking it on other devices goes on, and so does every free queued
    /// already, such as one of an activation revoked here; only the removal
    /// that would have followed that one is dropped, as this one removes
    /// the license.
    mutating func removal(key: String, activationID: String) {
        stalled = [:]
        ops.removeAll { $0.kind == .remove && $0.revoked }
        var frees = ops.filter { $0.kind == .report }.map {
            Op(kind: .free, key: $0.key, activationID: $0.activationID, fromRemove: true)
        }
        ops.removeAll { $0.kind == .activate || $0.kind == .report || $0.kind == .refuse }
        if !key.isEmpty, !activationID.isEmpty, !frees.contains(where: { $0.activationID == activationID }),
            !ops.contains(where: { $0.kind == .free && $0.activationID == activationID }) {
            frees.append(Op(kind: .free, key: key, activationID: activationID, fromRemove: true))
        }
        ops.append(contentsOf: frees)
        ops.append(Op(kind: .remove, key: key, activationID: activationID))
    }

    /// Another device revoked the license here (`key`), leaving this
    /// computer's `activationID` for it to free: Polar frees it first, and
    /// only then is the license removed, each tried until done.
    mutating func quietRemoval(key: String, activationID: String) {
        if !key.isEmpty, !activationID.isEmpty, !ops.contains(where: { $0.kind == .free && $0.activationID == activationID }) {
            ops.append(Op(kind: .free, key: key, activationID: activationID, revoked: true))
        }
        ops.append(Op(kind: .remove, key: key, activationID: activationID, revoked: true))
    }

    /// Queues a free for each activation of an earlier key the service
    /// lists (`License.to_free`) and nothing frees yet, and tells the
    /// service once Polar freed it.
    mutating func freeEarlier(_ activations: [Inputweave_App_V1_LicenseActivation]) {
        for activation in activations where !activation.key.isEmpty && !activation.activationID.isEmpty {
            let id = activation.activationID
            guard !ops.contains(where: {
                $0.activationID == id && ($0.kind == .free || $0.kind == .freed || ($0.kind == .remove && $0.revoked))
            }) else { continue }
            ops.append(Op(kind: .free, key: activation.key, activationID: id, toFree: true))
        }
    }

    /// The user revoked this computer's license on another device
    /// (`deviceID`, its `activationID`), Polar having deactivated that or
    /// not: the service is told, until it answers. Once only, however
    /// often it is asked for.
    mutating func revocation(key: String, deviceID: String, activationID: String, deactivated: Bool) {
        guard !ops.contains(where: { $0.kind == .revoke && $0.deviceID == deviceID && $0.activationID == activationID })
        else { return }
        ops.append(Op(kind: .revoke, key: key, activationID: activationID, deviceID: deviceID, deactivated: deactivated))
    }
}

/// An outbox is read leniently: an operation this build does not know (a
/// kind from a newer one) is skipped, and a field it lacks has its default.
extension LicenseOutbox {
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        ops = try container.decodeIfPresent([Lenient<Op>].self, forKey: .ops)?.compactMap(\.value) ?? []
        stalled = try container.decodeIfPresent([String: String].self, forKey: .stalled) ?? [:]
        polarNotBefore = try container.decodeIfPresent(Date.self, forKey: .polarNotBefore)
    }

    private struct Lenient<Value: Decodable>: Decodable {
        let value: Value?

        init(from decoder: Decoder) throws {
            value = try? Value(from: decoder)
        }
    }
}

extension LicenseOutbox.Op {
    init(from decoder: Decoder) throws {
        let container = try decoder.container(keyedBy: CodingKeys.self)
        kind = try container.decode(LicenseOutbox.Kind.self, forKey: .kind)
        key = try container.decode(String.self, forKey: .key)
        id = try container.decodeIfPresent(String.self, forKey: .id) ?? UUID().uuidString
        activationID = try container.decodeIfPresent(String.self, forKey: .activationID) ?? ""
        email = try container.decodeIfPresent(String.self, forKey: .email) ?? ""
        validatedAt = try container.decodeIfPresent(UInt64.self, forKey: .validatedAt) ?? 0
        refusal = try container.decodeIfPresent(Int.self, forKey: .refusal) ?? 0
        noLongerValid = try container.decodeIfPresent(Bool.self, forKey: .noLongerValid) ?? false
        fromRemove = try container.decodeIfPresent(Bool.self, forKey: .fromRemove) ?? false
        deviceID = try container.decodeIfPresent(String.self, forKey: .deviceID) ?? ""
        deactivated = try container.decodeIfPresent(Bool.self, forKey: .deactivated) ?? false
        revoked = try container.decodeIfPresent(Bool.self, forKey: .revoked) ?? false
        toFree = try container.decodeIfPresent(Bool.self, forKey: .toFree) ?? false
        staleTries = try container.decodeIfPresent(Int.self, forKey: .staleTries) ?? 0
        tries = try container.decodeIfPresent(Int.self, forKey: .tries) ?? 0
        notBefore = try container.decodeIfPresent(Date.self, forKey: .notBefore)
        soonest = try container.decodeIfPresent(Date.self, forKey: .soonest)
    }
}

/// What reading the kept outbox found.
enum LicenseOutboxRead: Equatable, Sendable {
    /// The outbox, empty when none is kept.
    case kept(LicenseOutbox)
    /// One is kept but cannot be read now (the keychain refused, or is
    /// locked): it is left alone and read again later.
    case unreadable
}

/// Where the outbox is kept: the keychain, or memory in tests and previews.
protocol LicenseOutboxStore: Sendable {
    func load() async -> LicenseOutboxRead
    /// Keeps `outbox`; false when it could not be written.
    func save(_ outbox: LicenseOutbox) async -> Bool
}

/// The outbox holds license keys, so it is kept as a generic password in
/// this user's login keychain rather than in the app's defaults. iCloud
/// never syncs that keychain; the data protection keychain, whose items
/// can be kept to this device alone, needs a keychain entitlement this
/// app does not have. The keychain is used off the main thread, where it
/// may wait for the user to allow access.
final class KeychainLicenseOutboxStore: LicenseOutboxStore {
    static let account = "outbox"
    /// Where an outbox this build cannot decode at all is set aside, so
    /// starting over never overwrites it.
    static let asideAccount = "outbox.unreadable"
    private static let log = Logger(subsystem: "cc.ctsite.inputweave", category: "license")
    let service: String

    /// Development builds of the license kept the outbox in the app's
    /// defaults; that copy is deleted, not moved.
    init(service: String = "cc.ctsite.inputweave.license", defaults: UserDefaults = .standard) {
        self.service = service
        defaults.removeObject(forKey: Preferences.licenseOutbox)
    }

    func load() async -> LicenseOutboxRead {
        let service = service
        return await Task.detached { Self.read(service: service) }.value
    }

    func save(_ outbox: LicenseOutbox) async -> Bool {
        let service = service
        return await Task.detached { Self.write(outbox, service: service) }.value
    }

    private static func read(service: String) -> LicenseOutboxRead {
        var query = item(service, account)
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var found: CFTypeRef?
        let status = SecItemCopyMatching(query as CFDictionary, &found)
        if status == errSecItemNotFound { return .kept(LicenseOutbox()) }
        guard status == errSecSuccess, let data = found as? Data else {
            log.error("Couldn't read the license outbox: \(status)")
            return .unreadable
        }
        if let outbox = try? JSONDecoder().decode(LicenseOutbox.self, from: data) { return .kept(outbox) }
        guard put(data, service: service, account: asideAccount) else { return .unreadable }
        log.error("Couldn't decode the license outbox; it is kept aside as \(asideAccount, privacy: .public)")
        return .kept(LicenseOutbox())
    }

    private static func write(_ outbox: LicenseOutbox, service: String) -> Bool {
        if outbox == LicenseOutbox() {
            let status = SecItemDelete(item(service, account) as CFDictionary)
            if status == errSecSuccess || status == errSecItemNotFound { return true }
            log.error("Couldn't delete the license outbox: \(status)")
            return false
        }
        guard let data = try? JSONEncoder().encode(outbox) else { return false }
        return put(data, service: service, account: account)
    }

    private static func put(_ data: Data, service: String, account: String) -> Bool {
        var status = SecItemUpdate(item(service, account) as CFDictionary, [kSecValueData as String: data] as CFDictionary)
        if status == errSecItemNotFound {
            var new = item(service, account)
            new[kSecValueData as String] = data
            new[kSecAttrLabel as String] = "InputWeave license"
            status = SecItemAdd(new as CFDictionary, nil)
        }
        if status != errSecSuccess { log.error("Couldn't keep the license outbox: \(status)") }
        return status == errSecSuccess
    }

    private static func item(_ service: String, _ account: String) -> [String: Any] {
        [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
    }
}

@MainActor
final class MemoryLicenseOutboxStore: LicenseOutboxStore {
    var outbox = LicenseOutbox()
    /// How many times a save was asked for.
    private(set) var writes = 0
    /// Every save fails, as a keychain that cannot be written does.
    var failSaves = false
    /// The outbox cannot be read, as a keychain that refuses access.
    var unreadable = false

    func load() async -> LicenseOutboxRead {
        unreadable ? .unreadable : .kept(outbox)
    }

    func save(_ outbox: LicenseOutbox) async -> Bool {
        writes += 1
        guard !failSaves else { return false }
        self.outbox = outbox
        return true
    }
}

/// Where Buy a License and Manage Computers go. Info.plist, which
/// project.yml writes, is the one place they are named.
struct LicenseLinks: Equatable {
    var checkout: URL?
    var customerPortal: URL?

    static func fromBundle(_ bundle: Bundle = .main) -> LicenseLinks {
        fromInfo(bundle.infoDictionary ?? [:])
    }

    /// From an Info.plist's values.
    static func fromInfo(_ info: [String: Any]) -> LicenseLinks {
        func url(_ key: String) -> URL? {
            (info[key] as? String).flatMap { $0.isEmpty ? nil : URL(string: $0) }
        }
        return LicenseLinks(checkout: url("InputWeaveCheckoutURL"), customerPortal: url("InputWeaveCustomerPortalURL"))
    }
}

/// The license flow. Its background work is one outbox, run in two lanes,
/// each an operation at a time: the background service's operations in
/// their order, and Polar's. Each operation is safe to repeat and waits on
/// its own after a failure. What the views show is derived from the
/// outbox and the service's record.
@MainActor @Observable
final class LicenseController {
    /// Why the Enter License Key sheet could not finish.
    enum KeyError: Equatable {
        case notAKey
        case seatsFull
        /// Refunded or revoked.
        case noLongerValid
        case polarUnreachable
        /// The service cannot read its license file right now.
        case unavailable
        /// The service has not been reachable for a while; the key waits.
        case serviceNotResponding
        /// The service did not take the key or the answer.
        case failed
    }

    /// How far the kept outbox has been read.
    enum Loading {
        case loading, loaded
        /// The keychain would not give it: no background work until it does.
        case unreadable
    }

    /// The Enter License Key sheet is open.
    var entering = false
    /// The key the user entered is on its way.
    private(set) var activating = false
    private(set) var keyError: KeyError?
    /// The user's own Activate just succeeded: the label sews itself once
    /// (`labelSewn` clears it).
    private(set) var justActivated = false
    private(set) var outbox = LicenseOutbox()
    private(set) var loading = Loading.loading
    /// The service has been out of reach for a while, with work for it
    /// waiting.
    private(set) var serviceAway = false
    /// The service refused Remove License for good: said once, on the next
    /// License page.
    private(set) var removalRefused = false
    /// The last write of the outbox failed; what has to be kept first
    /// waits.
    private(set) var saveFailing = false
    /// The devices Apply License or Don't Apply was just sent for, until
    /// the service answers.
    private(set) var applying: Set<String> = []
    let links: LicenseLinks

    /// Polar confirms a license at most this often, while License or About
    /// is open,
    static let validateEvery: TimeInterval = 30 * 86_400
    /// and tries again no sooner than this after an attempt.
    static let attemptEvery: TimeInterval = 3_600
    /// Two 404s at least this far apart make the license invalid.
    static let notFoundGap: TimeInterval = 60
    /// The user's Remove License is asked for at most this often, as the
    /// license keeps changing under it (`license_stale`).
    static let removeAttempts = 3
    /// Waits before an operation's next try.
    static let retryDelays: [Duration] = [.seconds(30), .seconds(120), .seconds(600), .seconds(3_600)]
    /// Refusals that are only for now: the service cannot read its license
    /// file, or is stopping. `unexpected` is this app's own code for an
    /// answer it could not read (`AgentModel`), never the service's. Any
    /// other code the service answers with stands, known or not.
    static let temporaryRefusals: Set<String> = ["license_unavailable", "agent_stopping", "unexpected"]
    /// Waits before writing the outbox again after the keychain refused.
    static let saveDelays: [Duration] = [.seconds(1), .seconds(5), .seconds(30), .seconds(60)]
    /// How long the service may be out of reach before the views say so.
    static let awayAfter: Duration = .seconds(10)

    @ObservationIgnored private let agent: any LicenseAgent
    @ObservationIgnored private let polar: PolarClient
    @ObservationIgnored private let store: any LicenseOutboxStore
    @ObservationIgnored private let now: @MainActor () -> Date
    @ObservationIgnored private let sleep: @Sendable (Duration) async -> Void
    @ObservationIgnored private let log = Logger(subsystem: "cc.ctsite.inputweave", category: "license")
    /// What the store holds, so an unchanged outbox is not written again.
    @ObservationIgnored private var saved = LicenseOutbox()
    /// The operations whose outcome the Enter License Key sheet waits for:
    /// the user's key, then what is done for it after the service took it.
    @ObservationIgnored private var sheetOps: Set<String> = []
    @ObservationIgnored private var loader: Task<Void, Never>?
    @ObservationIgnored private var writing: Task<Void, Never>?
    @ObservationIgnored private var saveWake: Task<Void, Never>?
    @ObservationIgnored private var saveTries = 0
    @ObservationIgnored private var serviceLane: Task<Void, Never>?
    @ObservationIgnored private var polarLane: Task<Void, Never>?
    @ObservationIgnored private var wake: Task<Void, Never>?
    @ObservationIgnored private var wakeAt: Date?
    @ObservationIgnored private var awayWatch: Task<Void, Never>?
    @ObservationIgnored private var validation: Task<Void, Never>?
    @ObservationIgnored private var confirm: Task<Void, Never>?
    @ObservationIgnored private var lastValidationAttempt: Date?
    @ObservationIgnored private var firstNotFound: (activationID: String, at: Date)?
    /// The record and link `update()` last ran for.
    @ObservationIgnored private var seen: (license: LicenseRecord, linked: Bool)?
    /// License pages and About windows on screen.
    @ObservationIgnored private var surfaces = 0
    /// Holds the lanes and writes, so previews keep the state they were given.
    @ObservationIgnored private var held = false

    /// Reads the kept outbox in the background; until it is read, the
    /// views show the service's record and no background work runs.
    init(
        agent: any LicenseAgent, polar: PolarClient, links: LicenseLinks = .fromBundle(),
        store: any LicenseOutboxStore = KeychainLicenseOutboxStore(),
        now: @escaping @MainActor () -> Date = { Date() },
        sleep: @escaping @Sendable (Duration) async -> Void = { try? await Task.sleep(for: $0) }
    ) {
        self.agent = agent
        self.polar = polar
        self.links = links
        self.store = store
        self.now = now
        self.sleep = sleep
        load()
    }

    var license: LicenseRecord { agent.license }

    // MARK: - Derived

    /// Shown as licensed: never an activation that is being removed or
    /// freed.
    var isLicensed: Bool {
        let license = agent.license
        guard license.state == .licensed else { return false }
        return !outbox.ops.contains {
            ($0.kind == .remove && !$0.revoked && $0.key == license.key)
                || ($0.kind == .free && $0.activationID == license.activationID)
        }
    }

    /// Remove License's first attempt is under way. A removal for a
    /// license revoked from another device is never shown: the user here
    /// did nothing.
    var removing: Bool {
        guard let remove = outbox.ops.first(where: { $0.kind == .remove && !$0.revoked }) else { return false }
        return remove.tries == 0 && !serviceAway && !saveFailing && loading != .unreadable
    }

    /// Remove License was tried and the service has not taken it yet, or
    /// cannot be asked for now.
    var removalPending: Bool {
        outbox.ops.contains(where: { $0.kind == .remove && !$0.revoked }) && !removing
    }

    /// The removed activation's seat is still taken: Polar could not be
    /// told yet.
    var seatNotFreedYet: Bool {
        outbox.ops.contains { $0.kind == .free && $0.fromRemove && !$0.revoked && $0.tries > 0 }
    }

    /// Revoke License for `deviceID` waits to reach the service.
    func revoking(_ deviceID: String) -> Bool {
        outbox.ops.contains { $0.kind == .revoke && $0.deviceID == deviceID }
    }

    // MARK: - Driven by the service's status

    /// Follows the service's record from now on.
    func start() {
        observe()
        changed()
    }

    private func observe() {
        withObservationTracking {
            _ = agent.license
            _ = agent.licenseLinked
        } onChange: { [weak self] in
            Task { @MainActor in
                self?.observe()
                self?.changed()
            }
        }
    }

    /// `update()`, when the record or the link is not what it last was.
    private func changed() {
        let license = agent.license
        let linked = agent.licenseLinked
        if let seen, seen.license == license, seen.linked == linked { return }
        seen = (license, linked)
        update()
    }

    /// The service's record may have changed: queues an activation for a
    /// key that waits for one, runs the outbox, and confirms the license
    /// while it is on screen.
    func update() {
        guard loading == .loaded else { return }
        let license = agent.license
        if agent.licenseLinked {
            let current = signature(license)
            let time = now()
            change { outbox in
                outbox.stalled = outbox.stalled.filter { $0.key == license.key && $0.value == current }
                outbox.freeEarlier(license.toFree)
                if let until = outbox.polarNotBefore, until <= time { outbox.polarNotBefore = nil }
            }
            queueActivation(license)
            queueRevokedRemoval(license)
        }
        kick()
        if surfaces > 0 { confirmIfDue() }
    }

    /// The License page or About appeared.
    func surfaceAppeared() {
        surfaces += 1
        if loading == .unreadable { load() }
        update()
    }

    func surfaceDisappeared() {
        surfaces = max(surfaces - 1, 0)
        if surfaces == 0 { removalRefused = false }
    }

    /// The label's sewing played; it does not play again on the next visit.
    func labelSewn() {
        justActivated = false
    }

    // MARK: - The user's actions

    /// Enter License Key › Activate.
    func submit(_ raw: String) {
        let key = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        keyError = nil
        justActivated = false
        activating = true
        let op = LicenseOutbox.Op(kind: .setKey, key: key)
        sheetOps = [op.id]
        change { $0.enter(op) }
        if loading == .unreadable { load() }
        kick()
    }

    /// Closes the sheet; work already under way carries on.
    func cancelEntering() {
        entering = false
        activating = false
        keyError = nil
        sheetOps = []
    }

    func beginEntering() {
        keyError = nil
        entering = true
    }

    /// Try Again, after every seat was taken: the same key starts over.
    func tryAgain() {
        let key = agent.license.key
        guard !key.isEmpty else { return }
        change { $0.enter(.init(kind: .setKey, key: key)) }
        if loading == .unreadable { load() }
        kick()
    }

    /// Remove License, once confirmed: removes the license here, then frees
    /// this computer's seat with Polar; each is tried until done.
    func remove() {
        let license = agent.license
        justActivated = false
        removalRefused = false
        firstNotFound = nil
        change { $0.removal(key: license.key, activationID: license.activationID) }
        if loading == .unreadable { load() }
        kick()
    }

    /// Apply License: the service sends the key to the device, whose app
    /// activates it; nothing goes to Polar from here. Its row then shows
    /// what the service says.
    func apply(_ deviceID: String) async {
        await sendForDevice(deviceID, "Applying the license") {
            try await $0.applyLicense(deviceID: deviceID)
        }
    }

    /// Don't Apply: takes back applying the license to a device that has
    /// not activated it yet (`LicensedDevice.can_take_back`).
    func dontApply(_ deviceID: String) async {
        await sendForDevice(deviceID, "Taking back the license") {
            try await $0.revokeLicense(deviceID: deviceID, activationID: "", deactivated: false)
        }
    }

    /// Sends a command for a device's row once at a time; a refusal leaves
    /// the row as the service shows it, and is only logged.
    private func sendForDevice(
        _ deviceID: String, _ what: String, _ command: @MainActor (any LicenseAgent) async throws -> Void
    ) async {
        guard !applying.contains(deviceID) else { return }
        applying.insert(deviceID)
        defer { applying.remove(deviceID) }
        do {
            try await command(agent)
        } catch let refusal as AgentRefusal {
            log.notice("\(what, privacy: .public) for \(deviceID, privacy: .public) was refused: \(refusal.code, privacy: .public) \(refusal.detail, privacy: .public)")
        } catch {
            log.notice("\(what, privacy: .public) for \(deviceID, privacy: .public) failed: \(error.localizedDescription, privacy: .public)")
        }
    }

    /// Revoke License, once confirmed: Polar is asked once to deactivate
    /// the device's activation with this computer's key, then the service
    /// is told, with whether Polar did, so the device goes back to trial
    /// (freeing its activation itself when Polar did not). Telling the
    /// service is kept and tried until it answers.
    func revoke(_ deviceID: String, activationID: String) async {
        guard !applying.contains(deviceID),
            !outbox.ops.contains(where: { $0.kind == .revoke && $0.deviceID == deviceID && $0.activationID == activationID })
        else { return }
        applying.insert(deviceID)
        defer { applying.remove(deviceID) }
        let key = agent.license.key
        let deactivated = await deactivateOnce(key: key, activationID: activationID, of: deviceID)
        change { $0.revocation(key: key, deviceID: deviceID, activationID: activationID, deactivated: deactivated) }
        if loading == .unreadable { load() }
        kick()
    }

    /// Asks Polar once to deactivate another device's activation, unless
    /// Polar asked to wait: whether it did, or said the activation was
    /// gone. Never asked again by this app; the device frees it otherwise.
    private func deactivateOnce(key: String, activationID: String, of deviceID: String) async -> Bool {
        guard !key.isEmpty, !activationID.isEmpty else { return false }
        guard !polarWaiting else {
            log.notice("Polar asked to wait: the activation of \(deviceID, privacy: .public) is left to it")
            return false
        }
        switch await polar.deactivate(key: key, activationID: activationID) {
        case .done:
            return true
        case .rateLimited(let wait):
            slowDown(wait)
        case .refused, .unreachable:
            break
        }
        log.notice("Polar did not deactivate the activation of \(deviceID, privacy: .public); it is left to it")
        return false
    }

    /// Remove Device: with `revoking` ("Also revoke its license"), first
    /// revokes the license there. For a device licensed with this
    /// computer's key Polar is asked once to deactivate its activation, as
    /// for Revoke License, and the service is told whether it did; one
    /// activating the key or waiting for it has it taken back. The device
    /// is unpaired once the service answered, whatever it answered. The
    /// notice to show after, if the license could not be revoked: decided
    /// by the service's answer too.
    func removeDevice(_ deviceID: String, revoking: Bool) async -> LicenseText.RemoveNotice? {
        var notice: LicenseText.RemoveNotice?
        if revoking, let device = agent.license.devices.first(where: { $0.id == deviceID }),
            [.licensed, .activating, .waiting].contains(device.use) {
            var activationID = ""
            var deactivated = false
            if device.use == .licensed {
                activationID = device.activationID
                deactivated = await deactivateOnce(key: agent.license.key, activationID: activationID, of: deviceID)
            }
            var revoked = true
            do {
                try await agent.revokeLicense(deviceID: deviceID, activationID: activationID, deactivated: deactivated)
            } catch {
                revoked = false
                log.notice("Revoking the license on \(deviceID, privacy: .public) failed: \(String(describing: error), privacy: .public)")
            }
            // As the service's answer shows it, before it stops trusting it.
            let connected = agent.isConnected(deviceID)
            notice = LicenseText.removeNotice(device, deactivated: deactivated, revoked: revoked, connected: connected)
        }
        do {
            try await agent.unpair(deviceID)
        } catch {
            log.notice("Removing \(deviceID, privacy: .public) failed: \(String(describing: error), privacy: .public)")
        }
        return notice
    }

    /// For the tests: waits until the outbox is read and written and both
    /// lanes and any confirmation are idle.
    func settle() async {
        while let task = loader ?? writing ?? serviceLane ?? polarLane ?? validation {
            await task.value
        }
    }

    /// Queues an activation for a key the record says waits for one, unless
    /// something for that key is queued or the key is stalled.
    private func queueActivation(_ license: LicenseRecord) {
        guard license.state == .pendingActivation, !license.key.isEmpty,
            outbox.stalled[license.key] != signature(license),
            !outbox.ops.contains(where: { $0.key == license.key && !$0.revoked && $0.kind != .free && $0.kind != .revoke })
        else { return }
        change { $0.ops.append(.init(kind: .activate, key: license.key)) }
    }

    /// Another device revoked the license here without freeing this
    /// computer's activation: Polar frees it, then the license is removed,
    /// without a word. Once, while nothing removes it already, and not
    /// again for a record the service refused to remove.
    private func queueRevokedRemoval(_ license: LicenseRecord) {
        guard license.state == .revoked, outbox.stalled[license.key] != signature(license),
            !outbox.ops.contains(where: { $0.kind == .remove }) else { return }
        firstNotFound = nil
        change { $0.quietRemoval(key: license.key, activationID: license.activationID) }
    }

    /// The record still says the license was revoked here, with `op`'s key
    /// and activation.
    private func stillRevoked(_ op: LicenseOutbox.Op) -> Bool {
        let license = agent.license
        return license.state == .revoked && license.key == op.key && license.activationID == op.activationID
    }

    /// A license revoked here is removed only once its activation is
    /// freed; meanwhile the service's other operations go on.
    private func waitsForItsFree(_ op: LicenseOutbox.Op) -> Bool {
        op.kind == .remove && op.revoked
            && outbox.ops.contains { $0.kind == .free && $0.revoked && $0.activationID == op.activationID }
    }

    /// Polar asked for no request now (`Retry-After`).
    private var polarWaiting: Bool {
        outbox.polarNotBefore.map { now() < $0 } ?? false
    }

    /// Polar said to wait `wait`: no request to it before then, whoever
    /// asks.
    private func slowDown(_ wait: Duration) {
        let until = now().addingTimeInterval(TimeInterval(wait.components.seconds))
        change { outbox in
            if outbox.polarNotBefore.map({ $0 < until }) ?? true { outbox.polarNotBefore = until }
        }
    }

    // MARK: - Reading and writing the outbox

    private func load() {
        guard loader == nil, loading != .loaded else { return }
        loading = .loading
        let store = store
        loader = Task { [weak self] in
            let read = await store.load()
            guard !Task.isCancelled, let self else { return }
            self.loader = nil
            self.read(read)
        }
    }

    /// The kept outbox, with what the user asked for while it was read.
    private func read(_ read: LicenseOutboxRead) {
        switch read {
        case .kept(let kept):
            var merged = kept
            for op in outbox.ops {
                switch op.kind {
                case .setKey: merged.enter(op)
                case .remove where !op.revoked: merged.removal(key: op.key, activationID: op.activationID)
                case .revoke:
                    merged.revocation(
                        key: op.key, deviceID: op.deviceID, activationID: op.activationID, deactivated: op.deactivated)
                case .activate, .report, .refuse, .free, .remove, .freed: break
                }
            }
            loading = .loaded
            saved = kept
            outbox = merged
            persist()
            update()
        case .unreadable:
            // Left as it is, and read again when the user next looks or
            // acts; a key the user entered cannot be kept meanwhile.
            loading = .unreadable
            let keys = outbox.ops.filter { $0.kind == .setKey }
            outbox.ops.removeAll { $0.kind == .setKey }
            if keys.contains(where: forSheet) {
                sheetOps = []
                finish(.failed)
            }
        }
    }

    /// Changes the outbox in one step, and writes it.
    private func change(_ body: (inout LicenseOutbox) -> Void) {
        var next = outbox
        body(&next)
        guard next != outbox else { return }
        outbox = next
        persist()
    }

    /// Writes what is kept unless the store has it already, one write at a
    /// time; a write that fails is tried again, with the newest outbox,
    /// until it succeeds.
    private func persist() {
        guard loading == .loaded, !held, writing == nil, saveWake == nil, outbox.kept != saved else { return }
        let store = store
        writing = Task { [weak self] in
            // Taken when the write starts, so changes made meanwhile go in
            // the same write.
            guard let kept = self?.outbox.kept else { return }
            let written = await store.save(kept)
            guard let self else { return }
            self.writing = nil
            if written {
                self.saved = kept
                self.saveTries = 0
                self.saveFailing = false
                self.persist()
                self.kick()
            } else {
                self.saveFailed()
            }
        }
    }

    private func saveFailed() {
        saveFailing = true
        // What the sheet waits for cannot go until the outbox is kept: its
        // report, or its key behind a removal not kept yet. It says so, and
        // keeps following.
        let blocked = outbox.ops.first(where: \.isService).map { $0.needsKeeping && !isSaved($0) } ?? false
        if outbox.ops.contains(where: { forSheet($0) && (($0.needsKeeping && !isSaved($0)) || ($0.isService && blocked)) }) {
            finish(.failed)
        }
        let delay = Self.saveDelays[min(saveTries, Self.saveDelays.count - 1)]
        saveTries += 1
        let sleep = sleep
        saveWake = Task { [weak self] in
            await sleep(delay)
            guard let self else { return }
            self.saveWake = nil
            self.persist()
        }
    }

    /// The store holds `op`.
    private func isSaved(_ op: LicenseOutbox.Op) -> Bool {
        saved.ops.contains { $0.id == op.id }
    }

    private func isQueued(_ op: LicenseOutbox.Op) -> Bool {
        outbox.ops.contains { $0.id == op.id }
    }

    // MARK: - The two lanes

    private enum Lane {
        /// The background service's operations, in their order, while it
        /// is reachable.
        case service
        /// Polar's, each once its wait is over.
        case polar
    }

    /// Starts each idle lane that has work it can run now, and wakes them
    /// for work that waits.
    private func kick() {
        guard loading == .loaded, !held else { return }
        if serviceLane == nil, nextOp(.service) != nil {
            serviceLane = Task { [weak self] in await self?.drain(.service) }
        }
        if polarLane == nil, nextOp(.polar) != nil {
            polarLane = Task { [weak self] in await self?.drain(.polar) }
        }
        scheduleWake()
        watchService()
    }

    private func drain(_ lane: Lane) async {
        while let op = nextOp(lane) {
            await perform(op)
            kick()
        }
        switch lane {
        case .service: serviceLane = nil
        case .polar: polarLane = nil
        }
        kick()
    }

    /// The operation `lane` can run now. The service's first only, once
    /// the store holds what it needs kept; Polar's first whose wait is
    /// over, but never the free of an activation whose removal the service
    /// has not taken yet.
    private func nextOp(_ lane: Lane) -> LicenseOutbox.Op? {
        let time = now()
        func due(_ op: LicenseOutbox.Op) -> Bool { op.notBefore.map { $0 <= time } ?? true }
        switch lane {
        case .service:
            guard agent.licenseLinked, let op = outbox.ops.first(where: { $0.isService && !waitsForItsFree($0) }),
                due(op), !op.needsKeeping || isSaved(op) else { return nil }
            return op
        case .polar:
            guard !polarWaiting else { return nil }
            return outbox.ops.first { op in
                guard !op.isService, due(op) else { return false }
                return op.kind != .free || op.revoked || op.activationID.isEmpty
                    || !outbox.ops.contains { $0.kind == .remove && !$0.revoked && $0.activationID == op.activationID }
            }
        }
    }

    /// Wakes the lanes when the next wait that matters is over: Polar's,
    /// and the service's first while it is reachable. The others cannot
    /// run sooner however long they have waited; the service coming back
    /// runs its first through `update()`.
    private func scheduleWake() {
        let time = now()
        var serviceTurn = agent.licenseLinked
        var next: Date?
        for op in outbox.ops {
            if op.isService {
                guard serviceTurn, !waitsForItsFree(op) else { continue }
                serviceTurn = false
            }
            if let notBefore = op.notBefore, notBefore > time { next = min(next ?? notBefore, notBefore) }
        }
        if let until = outbox.polarNotBefore, until > time, outbox.ops.contains(where: { !$0.isService }) {
            next = min(next ?? until, until)
        }
        guard next != wakeAt || wake == nil else { return }
        wake?.cancel()
        wake = nil
        wakeAt = next
        guard let next else { return }
        let delay = next.timeIntervalSince(time)
        let sleep = sleep
        wake = Task { [weak self] in
            await sleep(.milliseconds(Int(delay * 1000)))
            guard !Task.isCancelled, let self else { return }
            self.wake = nil
            self.wakeAt = nil
            self.kick()
        }
    }

    /// Says so when the service has been out of reach for `awayAfter` with
    /// work for it waiting; the work stays queued.
    private func watchService() {
        guard !agent.licenseLinked, outbox.ops.contains(where: \.isService) else {
            awayWatch?.cancel()
            awayWatch = nil
            if serviceAway {
                serviceAway = false
                if keyError == .serviceNotResponding, outbox.ops.contains(where: forSheet) {
                    keyError = nil
                    activating = true
                }
            }
            return
        }
        if serviceAway {
            // A key entered while the service is still away hears so at once.
            sayServiceAway()
            return
        }
        guard awayWatch == nil else { return }
        let sleep = sleep
        awayWatch = Task { [weak self] in
            await sleep(Self.awayAfter)
            guard !Task.isCancelled, let self else { return }
            self.awayWatch = nil
            guard !self.agent.licenseLinked, self.outbox.ops.contains(where: \.isService) else { return }
            self.serviceAway = true
            self.sayServiceAway()
        }
    }

    private func sayServiceAway() {
        guard keyError != .serviceNotResponding, outbox.ops.contains(where: { $0.isService && forSheet($0) }) else { return }
        activating = false
        keyError = .serviceNotResponding
    }

    private func perform(_ op: LicenseOutbox.Op) async {
        switch op.kind {
        case .setKey: await setKey(op)
        case .activate: await activate(op)
        case .report: await report(op)
        case .refuse: await refuse(op)
        case .free: await free(op)
        case .remove: await removeFromService(op)
        case .revoke: await revokeOnService(op)
        case .freed: await tellFreed(op)
        }
    }

    // MARK: - The operations

    private func setKey(_ op: LicenseOutbox.Op) async {
        done(op)
        do {
            let license = try await agent.setLicenseKey(op.key)
            if forSheet(op) {
                if license.state == .licensed, license.key == op.key, isLicensed {
                    // Already licensed here with it: nothing to activate.
                    finish(nil)
                } else {
                    // What is done for the key from now on answers the sheet.
                    queueActivation(license)
                    for queued in outbox.ops where queued.key == op.key && (queued.kind == .activate || queued.kind == .report) {
                        sheetOps.insert(queued.id)
                    }
                    if !outbox.ops.contains(where: forSheet) {
                        sheetOps = []
                        finish(.failed)
                    }
                }
            }
            update()
        } catch let refusal as AgentRefusal {
            guard forSheet(op) else { return }
            sheetOps = []
            switch refusal.code {
            case "invalid_license_key": finish(.notAKey)
            case "license_unavailable": finish(.unavailable)
            default: finish(.failed)
            }
        } catch {
            guard forSheet(op) else { return }
            sheetOps = []
            finish(.failed)
        }
    }

    private func activate(_ op: LicenseOutbox.Op) async {
        let license = agent.license
        guard license.state == .pendingActivation, license.key == op.key,
            outbox.stalled[op.key] != signature(license) else {
            // The record moved on: nothing waits for this activation.
            done(op)
            if forSheet(op) { finish(.failed) }
            return
        }
        let answer = await polar.activate(key: op.key, label: agent.thisComputerName)
        guard isQueued(op) else {
            // Removed meanwhile: an activation Polar made takes no seat.
            if case .activated(let id, _, _) = answer {
                change { $0.ops.append(.init(kind: .free, key: op.key, activationID: id, fromRemove: true)) }
            }
            return
        }
        switch answer {
        case .activated(let id, let email, let at):
            // Kept before it is reported, so a restart reports it rather
            // than activating again.
            replace(op, with: .init(kind: .report, key: op.key, activationID: id, email: email, validatedAt: stamp(at)))
            // While the keychain refuses, the sheet says the license is not
            // saved yet, and keeps following.
            if saveTries > 0, forSheet(op) { finish(.failed) }
        case .activatedUnnamed:
            // A seat was taken that cannot be reported or freed: never
            // asked for again by itself, since that would take another.
            log.error("Polar activated the key without an activation id that can be read")
            let record = signature(agent.license)
            change { outbox in
                outbox.remove(op)
                outbox.stalled[op.key] = record
            }
            if forSheet(op) { finish(.failed) }
        case .notPermitted:
            // Every seat taken, or the key no longer active: the key alone
            // tells which. A removal meanwhile drops the answer.
            let probe = await polar.validate(key: op.key, activationID: nil)
            guard isQueued(op) else { return }
            switch probe {
            case .valid:
                replace(op, with: .init(kind: .refuse, key: op.key, refusal: Inputweave_App_V1_LicenseRefusal.seatsFull.rawValue))
            case .notFound:
                replace(op, with: .init(
                    kind: .refuse, key: op.key, refusal: Inputweave_App_V1_LicenseRefusal.invalid.rawValue, noLongerValid: true))
            case .rateLimited(let wait):
                slowDown(wait)
                retry(op, wait)
                if forSheet(op) { finish(.polarUnreachable) }
            case .unreachable:
                retry(op)
                if forSheet(op) { finish(.polarUnreachable) }
            }
        case .invalid:
            replace(op, with: .init(kind: .refuse, key: op.key, refusal: Inputweave_App_V1_LicenseRefusal.invalid.rawValue))
        case .rejected:
            // Polar could not read something other than the key: no
            // refusal to record, and asked again later.
            log.error("Polar could not read an activation request")
            retry(op)
            if forSheet(op) { finish(.failed) }
        case .rateLimited(let wait):
            slowDown(wait)
            retry(op, wait)
            if forSheet(op) { finish(.polarUnreachable) }
        case .unreachable:
            retry(op)
            if forSheet(op) { finish(.polarUnreachable) }
        }
    }

    private func report(_ op: LicenseOutbox.Op) async {
        var activated = Inputweave_App_V1_LicenseActivated()
        activated.activationID = op.activationID
        activated.customerEmail = op.email
        activated.validatedAt = op.validatedAt
        var result = Inputweave_App_V1_RecordLicenseResult()
        result.key = op.key
        result.activated = activated
        do {
            try await agent.recordLicense(result)
            done(op)
            if forSheet(op) { finish(nil) }
        } catch let refusal as AgentRefusal where Self.temporaryRefusals.contains(refusal.code) {
            retry(op)
            if forSheet(op) { finish(refusal.code == "license_unavailable" ? .unavailable : .failed) }
        } catch let refusal as AgentRefusal {
            if refusal.code == "already_licensed", agent.license.activationID == op.activationID {
                done(op)
                if forSheet(op) { finish(nil) }
                return
            }
            // Any other answer stands, known or not: the activation takes
            // no seat, and the key is not activated again while the record
            // stays as it is.
            let record = signature(agent.license)
            change { outbox in
                outbox.replace(op, with: .init(kind: .free, key: op.key, activationID: op.activationID))
                outbox.stalled[op.key] = record
            }
            if forSheet(op) { finish(refusal.code == "already_licensed" ? nil : .failed) }
        } catch {
            // No answer: the same report goes again later (the same
            // activation twice is no change to the service).
            retry(op)
            if forSheet(op) { finish(.failed) }
        }
    }

    private func refuse(_ op: LicenseOutbox.Op) async {
        var result = Inputweave_App_V1_RecordLicenseResult()
        result.key = op.key
        result.refused = Inputweave_App_V1_LicenseRefusal(rawValue: op.refusal) ?? .invalid
        do {
            try await agent.recordLicense(result)
            done(op)
            if forSheet(op) {
                finish(result.refused == .seatsFull ? .seatsFull : op.noLongerValid ? .noLongerValid : .notAKey)
            }
        } catch let refusal as AgentRefusal where Self.temporaryRefusals.contains(refusal.code) {
            retry(op)
            if forSheet(op) { finish(refusal.code == "license_unavailable" ? .unavailable : .failed) }
        } catch is AgentRefusal {
            // The record moved on; there is nothing to say.
            done(op)
            if forSheet(op) { finish(.failed) }
        } catch {
            retry(op)
            if forSheet(op) { finish(.failed) }
        }
    }

    /// Runs whether or not the service can be reached. An activation
    /// revoked here is freed whatever the record says now: it is never
    /// licensed again. One the service lists to be freed is told of once
    /// done: Polar freed it or will never take it.
    private func free(_ op: LicenseOutbox.Op) async {
        let answer = await polar.deactivate(key: op.key, activationID: op.activationID)
        guard isQueued(op) else { return }
        switch answer {
        case .done:
            freedDone(op)
        case .refused:
            log.notice("Polar refused to free activation \(op.activationID, privacy: .public); dropped")
            freedDone(op)
        case .rateLimited(let wait):
            slowDown(wait)
            retry(op, wait)
        case .unreachable:
            retry(op)
        }
    }

    private func freedDone(_ op: LicenseOutbox.Op) {
        if op.toFree || listedToFree(op.activationID) {
            replace(op, with: .init(kind: .freed, key: op.key, activationID: op.activationID))
        } else {
            done(op)
        }
    }

    /// The service lists `activationID` to be freed (`License.to_free`).
    private func listedToFree(_ activationID: String) -> Bool {
        !activationID.isEmpty && agent.license.toFree.contains { $0.activationID == activationID }
    }

    /// The service forgets an activation it listed to be freed; one it no
    /// longer lists changes nothing. A refusal that is not only for now
    /// leaves it to list it again.
    private func tellFreed(_ op: LicenseOutbox.Op) async {
        do {
            try await agent.licenseActivationFreed(op.activationID)
            done(op)
        } catch let refusal as AgentRefusal where !Self.temporaryRefusals.contains(refusal.code) {
            log.error("The background service refused that activation \(op.activationID, privacy: .public) was freed: \(refusal.code, privacy: .public)")
            done(op)
        } catch {
            retry(op)
        }
    }

    /// Removes the license as it was when it was asked for (`op`'s key and
    /// activation); the service refuses that as stale once it is another,
    /// which is done too. After its free, a license revoked here is
    /// removed only while the record still says so; one the service
    /// meanwhile lists to be freed (the key came here again) is said to be
    /// freed instead, and otherwise nothing more is done.
    private func removeFromService(_ op: LicenseOutbox.Op) async {
        if op.revoked, !stillRevoked(op) {
            afterQuietRemoval(op)
            return
        }
        do {
            try await agent.removeLicense(key: op.key, activationID: op.activationID)
            done(op)
        } catch let refusal as AgentRefusal where refusal.code == "license_stale" {
            if op.revoked {
                log.notice("The license revoked here changed before it was removed; it stays")
                afterQuietRemoval(op)
            } else {
                removeAgain(op)
            }
        } catch let refusal as AgentRefusal where !Self.temporaryRefusals.contains(refusal.code) {
            log.error("The background service refused to remove the license: \(refusal.code, privacy: .public)")
            removalRefusedForGood(op)
        } catch {
            retry(op)
            // A key the user entered waits behind the removal; the sheet
            // says it did not go through yet, and keeps following.
            if outbox.ops.contains(where: { $0.kind == .setKey && forSheet($0) }) { finish(.failed) }
        }
    }

    /// The user's Remove License met a newer license than the one it named:
    /// the user still wants it gone, so it is asked for again with the
    /// license as the record has it now, `removeAttempts` times in all,
    /// and then counts as refused. The activation it named is no longer the
    /// record's, so its free goes on; the current one's is freed once the
    /// service removed it. With no license left, the removal is done.
    private func removeAgain(_ op: LicenseOutbox.Op) {
        let license = agent.license
        guard !license.key.isEmpty else {
            log.notice("The license was gone before it was removed")
            done(op)
            return
        }
        guard op.staleTries + 1 < Self.removeAttempts else {
            log.error("The background service refused to remove the license: it kept changing")
            removalRefusedForGood(op)
            return
        }
        log.notice("The license changed before it was removed; the current one is removed")
        var next = LicenseOutbox.Op(kind: .remove, key: license.key, activationID: license.activationID)
        next.staleTries = op.staleTries + 1
        change { outbox in
            outbox.replace(op, with: next)
            let id = next.activationID
            if !id.isEmpty, !outbox.ops.contains(where: { $0.kind == .free && $0.activationID == id }) {
                outbox.ops.append(.init(kind: .free, key: next.key, activationID: id, fromRemove: true))
            }
        }
    }

    /// For good: the license stays here, and so does its seat. One revoked
    /// from another device is not removed again while the record says the
    /// same, and nothing is said of it.
    private func removalRefusedForGood(_ op: LicenseOutbox.Op) {
        let record = signature(agent.license)
        change { outbox in
            outbox.remove(op)
            if !op.activationID.isEmpty {
                outbox.ops.removeAll { $0.kind == .free && $0.activationID == op.activationID }
            }
            if op.revoked { outbox.stalled[op.key] = record }
        }
        if !op.revoked { removalRefused = true }
    }

    /// The license revoked here is no longer to be removed: its freed
    /// activation is said to be freed when the service lists it so.
    private func afterQuietRemoval(_ op: LicenseOutbox.Op) {
        if listedToFree(op.activationID) {
            replace(op, with: .init(kind: .freed, key: op.key, activationID: op.activationID))
        } else {
            done(op)
        }
    }

    /// Revoking is safe to repeat: a device with nothing to revoke changes
    /// nothing. A refusal that is not only for now is dropped; the device
    /// stays as the service shows it.
    private func revokeOnService(_ op: LicenseOutbox.Op) async {
        do {
            try await agent.revokeLicense(deviceID: op.deviceID, activationID: op.activationID, deactivated: op.deactivated)
            done(op)
        } catch let refusal as AgentRefusal where !Self.temporaryRefusals.contains(refusal.code) {
            log.error("The background service refused to revoke the license on \(op.deviceID, privacy: .public): \(refusal.code, privacy: .public)")
            done(op)
        } catch {
            retry(op)
        }
    }

    // MARK: - Small steps

    private func done(_ op: LicenseOutbox.Op) {
        change { $0.remove(op) }
    }

    /// `next` takes `op`'s place, and its part in the sheet; nothing when
    /// `op` is gone.
    private func replace(_ op: LicenseOutbox.Op, with next: LicenseOutbox.Op) {
        guard isQueued(op) else { return }
        if sheetOps.contains(op.id) { sheetOps.insert(next.id) }
        change { $0.replace(op, with: next) }
    }

    /// Waits before `op`'s next try: `wait`, or a growing delay.
    private func retry(_ op: LicenseOutbox.Op, _ wait: Duration? = nil) {
        let time = now()
        change { outbox in
            guard let index = outbox.ops.firstIndex(where: { $0.id == op.id }) else { return }
            let tries = outbox.ops[index].tries
            let delay = wait ?? Self.retryDelays[min(tries, Self.retryDelays.count - 1)]
            outbox.ops[index].tries = tries + 1
            outbox.ops[index].notBefore = time.addingTimeInterval(TimeInterval(delay.components.seconds))
            if outbox.ops[index].kind == .activate {
                let soonest = wait ?? Self.retryDelays[0]
                outbox.ops[index].soonest = time.addingTimeInterval(TimeInterval(soonest.components.seconds))
            }
        }
    }

    private func forSheet(_ op: LicenseOutbox.Op) -> Bool {
        sheetOps.contains(op.id)
    }

    private func finish(_ error: KeyError?) {
        activating = false
        keyError = error
        if error == nil {
            entering = false
            justActivated = true
            sheetOps = []
        }
    }

    // MARK: - Confirming

    private func confirmIfDue() {
        let license = agent.license
        guard agent.licenseLinked, isLicensed, !license.activationID.isEmpty, validation == nil, !polarWaiting else { return }
        let time = now()
        if let last = lastValidationAttempt, time.timeIntervalSince(last) < Self.attemptEvery { return }
        let confirmed = Date(timeIntervalSince1970: TimeInterval(license.validatedAt))
        guard time.timeIntervalSince(confirmed) >= Self.validateEvery else { return }
        startValidation(license)
    }

    private func startValidation(_ license: LicenseRecord) {
        lastValidationAttempt = now()
        validation = Task { [weak self] in
            await self?.validate(license)
            self?.validation = nil
        }
    }

    private func validate(_ license: LicenseRecord) async {
        let key = license.key
        let id = license.activationID
        switch await polar.validate(key: key, activationID: id) {
        case .valid(let email, let at):
            firstNotFound = nil
            var validated = Inputweave_App_V1_LicenseValidated()
            validated.validatedAt = stamp(at)
            validated.customerEmail = email
            var result = Inputweave_App_V1_RecordLicenseResult()
            result.key = key
            result.activationID = id
            result.validated = validated
            _ = try? await agent.recordLicense(result)
        case .notFound:
            if let first = firstNotFound, first.activationID == id,
                now().timeIntervalSince(first.at) >= Self.notFoundGap {
                firstNotFound = nil
                var result = Inputweave_App_V1_RecordLicenseResult()
                result.key = key
                result.activationID = id
                result.refused = .invalid
                _ = try? await agent.recordLicense(result)
            } else {
                if firstNotFound?.activationID != id { firstNotFound = (id, now()) }
                // Asks once more a minute later, open page or not.
                confirm?.cancel()
                let sleep = sleep
                confirm = Task { [weak self] in
                    await sleep(.seconds(Int(Self.notFoundGap) + 1))
                    guard !Task.isCancelled else { return }
                    self?.confirmNotFound(id)
                }
            }
        case .rateLimited(let wait):
            // Nothing changes; no request to Polar until it said, and the
            // next appearance an hour on asks again.
            slowDown(wait)
        case .unreachable:
            // Nothing changes; the next appearance an hour on asks again.
            break
        }
    }

    private func confirmNotFound(_ id: String) {
        let license = agent.license
        guard isLicensed, license.activationID == id, validation == nil else { return }
        startValidation(license)
    }

    /// What the service's record said when it refused, to tell when it
    /// changed.
    private func signature(_ license: LicenseRecord) -> String {
        "\(license.state.rawValue)|\(license.key)|\(license.activationID)|\(license.sourceDeviceID)|\(license.optedOut)"
    }

    /// When Polar answered, for `validated_at`: its `Date` header, unless
    /// that is missing, before 2020 or more than a day ahead of this Mac's
    /// clock, which the service would refuse; then this Mac's clock.
    func stamp(_ at: Date?) -> UInt64 {
        let local = now()
        guard let at, at.timeIntervalSince1970 >= 1_577_836_800, at.timeIntervalSince(local) <= 86_400 else {
            return UInt64(max(local.timeIntervalSince1970, 0))
        }
        return UInt64(at.timeIntervalSince1970)
    }
}

#if DEBUG
extension LicenseController {
    /// Fixed state, for previews and snapshot tests.
    func preview(
        entering: Bool = false, activating: Bool = false, keyError: KeyError? = nil, outbox: LicenseOutbox? = nil,
        serviceAway: Bool = false, removalRefused: Bool = false
    ) {
        held = true
        loader?.cancel()
        loader = nil
        loading = .loaded
        self.entering = entering
        self.activating = activating
        self.keyError = keyError
        self.serviceAway = serviceAway
        self.removalRefused = removalRefused
        if let outbox { self.outbox = outbox }
    }
}
#endif
