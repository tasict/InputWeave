import Foundation
import Observation
import SwiftProtobuf

typealias AgentStatus = Inputweave_App_V1_Status
typealias PairingState = Inputweave_App_V1_PairingState
typealias NearbyDevice = Inputweave_App_V1_NearbyDevice
typealias Arrangement = Inputweave_App_V1_Arrangement
typealias ArrangedDevice = Inputweave_App_V1_ArrangedDevice
typealias DeviceStatus = Inputweave_App_V1_DeviceStatus
typealias FileTransfer = Inputweave_App_V1_FileTransfer
typealias Transfers = Inputweave_App_V1_Transfers
typealias DragArrived = Inputweave_App_V1_DragArrived
typealias DragEnded = Inputweave_App_V1_DragEnded

/// What the background service says about drags of files between
/// computers (docs/architecture.md §9). News only as it happens.
enum DragNews {
    /// Another computer drags files here.
    case arrived(DragArrived)
    /// A drag from here, or one offered here, ended.
    case ended(DragEnded)
}

/// A request the background service refused, with its stable code.
struct AgentRefusal: Error, Equatable {
    let code: String
    let detail: String
}

/// Where the background service said the cursor is, after the user shook
/// the mouse with the cursor on this computer, in
/// its desktop coordinates. Each one counts anew, even at the same place.
struct CursorSighting: Equatable {
    let serial: UInt64
    let x: Double
    let y: Double

    /// The sighting that comes after `previous`.
    static func after(_ previous: CursorSighting?, x: Double, y: Double) -> CursorSighting {
        CursorSighting(serial: (previous?.serial ?? 0) + 1, x: x, y: y)
    }
}

/// The relative mode hotkey was pressed with the cursor on a computer whose
/// InputWeave takes no relative movements; the
/// key went there as any other. Each time counts anew.
struct RelativeRefusal: Equatable {
    let serial: UInt64
    let deviceID: String

    /// The refusal that comes after `previous`.
    static func after(_ previous: RelativeRefusal?, deviceID: String) -> RelativeRefusal {
        RelativeRefusal(serial: (previous?.serial ?? 0) + 1, deviceID: deviceID)
    }
}

/// The background service said to show this computer's name on each of its
/// screens for `durationMs`: its user, or another
/// computer's, asked to identify the screens. Each one counts anew.
struct ScreenIdentification: Equatable {
    let serial: UInt64
    let durationMs: UInt32

    /// The identification that comes after `previous`.
    static func after(_ previous: ScreenIdentification?, durationMs: UInt32) -> ScreenIdentification {
        ScreenIdentification(serial: (previous?.serial ?? 0) + 1, durationMs: durationMs)
    }
}

/// The UI's view of the background service: the state it last reported,
/// and the commands the UI sends it. Decisions stay in the service; this
/// only mirrors and forwards.
@MainActor @Observable
final class AgentModel {
    enum Link: Equatable {
        /// Trying to reach the background service.
        case connecting
        case connected
        /// The service speaks another IPC version, usually mid-update.
        case incompatible
    }

    /// The IPC version this app speaks, matching `IPC_VERSION` in iw-proto.
    static let ipcVersion: UInt32 = 1

    static var defaultSocketPath: String {
        FileManager.default.homeDirectoryForCurrentUser
            .appending(path: "Library/Application Support/InputWeave/agent.sock").path
    }

    /// Where the background service writes its log when launchd runs it
    /// (`default_log_file` in crates/inputweave/src/paths.rs).
    static var logFolder: URL {
        FileManager.default.homeDirectoryForCurrentUser.appending(path: "Library/Logs/InputWeave")
    }

    private(set) var link: Link = .connecting
    /// Since when the service has been out of reach, to tell a restart
    /// apart from a service that stopped responding.
    private(set) var unreachableSince: Date? = .now
    private(set) var status = AgentStatus() {
        didSet {
            if status.edgeDwellMs != 0 { lastSwitchDelayMs = status.edgeDwellMs }
            // Only when there is one to forget, so other statuses leave it be.
            if !wakeRequests.devices.isEmpty { wakeRequests.keep(status.devices.map(\.id)) }
        }
    }
    /// The computers this app asked the service to wake, until it answered;
    /// one no longer listed is forgotten.
    private(set) var wakeRequests = WakeRequests()
    private(set) var pairing = PairingState()
    private(set) var nearby: [NearbyDevice] = []
    private(set) var arrangement = Arrangement()
    private(set) var transfers = Transfers()
    /// The last time the service said where the cursor is; nil until the
    /// user first shakes the mouse with the cursor here.
    private(set) var cursorSighting: CursorSighting?
    /// The last time the service said to show this computer's name on its
    /// screens; nil until the screens are first identified.
    private(set) var screenIdentification: ScreenIdentification?
    /// The last time the relative mode hotkey reached a computer without
    /// relative mode; nil until it first does.
    private(set) var relativeRefusal: RelativeRefusal?
    /// The version the service reported when it connected.
    private(set) var agentVersion = ""
    /// The SHA-256 of the service's executable, as it reported it when it
    /// connected; empty from a service too old to say.
    private(set) var agentDigest = ""

    @ObservationIgnored private let socketPath: String
    @ObservationIgnored private var connection: AgentConnection?
    @ObservationIgnored private var nextRequestID: UInt64 = 1
    @ObservationIgnored private var pending: [UInt64: CheckedContinuation<Inputweave_App_V1_Response, any Error>] = [:]
    @ObservationIgnored private var runner: Task<Void, Never>?
    /// The last switch delay in effect while the app ran, so turning the
    /// option off and on again keeps a time set with `inputweave ctl dwell`.
    @ObservationIgnored private var lastSwitchDelayMs: UInt32?
    /// Told of every drag event, in order; a stored value would lose one
    /// that closely follows another.
    @ObservationIgnored var onDrag: ((DragNews) -> Void)?
    /// Told of every crossing of this computer's screen edges, to or from
    /// another computer, in order: each lights an
    /// edge, and a stored value would lose one that closely follows another.
    @ObservationIgnored var onCursorCrossed: ((Inputweave_App_V1_CursorCrossed) -> Void)?

    init(socketPath: String = AgentModel.defaultSocketPath) {
        self.socketPath = socketPath
    }

    /// Connects to the service, and reconnects whenever it goes away.
    func start() {
        guard runner == nil else { return }
        runner = Task { [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                do {
                    try await self.session()
                } catch {
                    // The service is not running, restarting, or refused
                    // the Hello; try again shortly.
                }
                self.disconnected()
                try? await Task.sleep(for: .seconds(1))
            }
        }
    }

    private func session() async throws {
        let connection = AgentConnection(socketPath: socketPath)
        try await connection.open()
        self.connection = connection
        let messages = connection.messages()
        let reader = Task { [weak self] in
            for try await message in messages {
                self?.handle(message)
            }
        }
        defer { reader.cancel() }

        var hello = Inputweave_App_V1_ClientHello()
        hello.ipcVersion = Self.ipcVersion
        hello.client = "macos-ui/\(Bundle.main.shortVersion)"
        do {
            if case .hello(let agent) = try await request(.hello(hello)) {
                agentVersion = agent.agentVersion
                agentDigest = agent.agentDigest
            }
        } catch let refusal as AgentRefusal where refusal.code == "ipc_version_mismatch" {
            link = .incompatible
            throw refusal
        }
        if case .status(let status) = try await request(.getStatus(.init())) {
            self.status = status
        }
        if case .pairing(let pairing) = try await request(.getPairing(.init())) {
            self.pairing = pairing
        }
        if case .nearby(let nearby) = try await request(.getNearby(.init())) {
            self.nearby = nearby.devices
        }
        if case .arrangement(let arrangement) = try await request(.getArrangement(.init())) {
            self.arrangement = arrangement
        }
        if case .transfers(let transfers) = try await request(.getTransfers(.init())) {
            self.transfers = transfers
        }
        link = .connected
        unreachableSince = nil
        try await reader.value
    }

    private func disconnected() {
        connection?.close()
        connection = nil
        for continuation in pending.values {
            continuation.resume(throwing: AgentConnection.Failure.closed)
        }
        pending.removeAll()
        if link == .connected {
            link = .connecting
        }
        if unreachableSince == nil {
            unreachableSince = .now
        }
    }

    /// Takes in a message from the service. Not private, so tests can hand
    /// it messages as the service sends them.
    func handle(_ message: Inputweave_App_V1_AgentMessage) {
        switch message.msg {
        case .response(let response):
            pending.removeValue(forKey: response.requestID)?.resume(returning: response)
        case .event(let event):
            // The drag events sit outside the oneof (see app.proto).
            if event.hasDragArrived {
                onDrag?(.arrived(event.dragArrived))
            } else if event.hasDragEnded {
                onDrag?(.ended(event.dragEnded))
            } else if event.hasScreensIdentified {
                screenIdentification = .after(
                    screenIdentification, durationMs: event.screensIdentified.durationMs)
            } else if event.hasRelativeUnsupported {
                relativeRefusal = .after(relativeRefusal, deviceID: event.relativeUnsupported.deviceID)
            } else if event.hasCursorCrossed {
                onCursorCrossed?(event.cursorCrossed)
            }
            switch event.event {
            case .statusChanged(let status): self.status = status
            case .pairingChanged(let pairing): self.pairing = pairing
            case .nearbyChanged(let nearby): self.nearby = nearby.devices
            case .arrangementChanged(let arrangement): self.arrangement = arrangement
            case .transfersChanged(let transfers): self.transfers = transfers
            case .cursorLocated(let located):
                cursorSighting = .after(cursorSighting, x: located.x, y: located.y)
            case nil: break
            }
        case nil:
            break
        }
    }

    /// Sends a command and waits for its result. A refusal throws
    /// `AgentRefusal`.
    @discardableResult
    func request(
        _ command: Inputweave_App_V1_ClientMessage.OneOf_Command
    ) async throws -> Inputweave_App_V1_Response.OneOf_Result {
        guard let connection else { throw AgentConnection.Failure.closed }
        let id = nextRequestID
        nextRequestID += 1
        var message = Inputweave_App_V1_ClientMessage()
        message.requestID = id
        message.command = command
        let response = try await withCheckedThrowingContinuation { continuation in
            pending[id] = continuation
            Task {
                do {
                    try await connection.send(message)
                } catch {
                    self.pending.removeValue(forKey: id)?.resume(throwing: error)
                }
            }
        }
        switch response.result {
        case .error(let error):
            throw AgentRefusal(code: error.code, detail: error.detail)
        case let result?:
            return result
        case nil:
            throw AgentRefusal(code: "unexpected", detail: "empty response")
        }
    }

    // MARK: - Commands

    func setSharing(_ enabled: Bool) async throws {
        var set = Inputweave_App_V1_SetSharing()
        set.enabled = enabled
        try await request(.setSharing(set))
    }

    func startPairing(deviceID: String) async throws {
        var start = Inputweave_App_V1_StartPairing()
        start.deviceID = deviceID
        try await request(.startPairing(start))
    }

    func startPairing(address: String) async throws {
        var start = Inputweave_App_V1_StartPairing()
        start.address = address
        try await request(.startPairing(start))
    }

    /// Tries a code; the result is also in `pairing`.
    @discardableResult
    func submitPairingCode(_ code: String) async throws -> PairingState {
        var submit = Inputweave_App_V1_SubmitPairingCode()
        submit.code = code
        guard case .pairing(let state) = try await request(.submitPairingCode(submit)) else {
            throw AgentRefusal(code: "unexpected", detail: "no pairing state")
        }
        pairing = state
        return state
    }

    /// Cancels the pairing started here, or declines the one shown here.
    func cancelPairing() async throws {
        try await request(.cancelPairing(.init()))
    }

    func unpair(_ deviceID: String) async throws {
        var unpair = Inputweave_App_V1_Unpair()
        unpair.deviceID = deviceID
        try await request(.unpair(unpair))
    }

    func rename(_ name: String) async throws {
        var set = Inputweave_App_V1_SetDeviceName()
        set.name = name
        try await request(.setDeviceName(set))
    }

    func setClipboardSync(_ enabled: Bool) async throws {
        var set = Inputweave_App_V1_SetClipboardSync()
        set.enabled = enabled
        try await request(.setClipboardSync(set))
    }

    /// Shares previews of this computer's desktop pictures with paired
    /// computers, or stops; turned off, they drop the ones they have.
    func setPreviewSharing(_ enabled: Bool) async throws {
        var set = Inputweave_App_V1_SetPreviewSharing()
        set.enabled = enabled
        if case .status(let status) = try await request(.setPreviewSharing(set)) {
            self.status = status
        }
    }

    /// Lets other computers control this Mac at the login window, from the
    /// next time it shows, or stops them. Turning it on is refused
    /// ("unsupported") where they cannot type on its lock screen either.
    func setLoginWindowAccess(_ on: Bool) async throws {
        var set = Inputweave_App_V1_SetLoginWindowAccess()
        set.on = on
        if case .status(let status) = try await request(.setLoginWindowAccess(set)) {
            self.status = status
        }
    }

    /// Has the background service list Downloads, Desktop and Documents, so
    /// macOS asks the user about each it was not told of. Answers at once
    /// with the round they are listed in: what came of each folder arrives
    /// in later statuses of that round.
    @discardableResult
    func requestFolderAccess() async throws -> UInt64 {
        guard case .status(let status) = try await request(.requestFolderAccess(Inputweave_App_V1_RequestFolderAccess())) else {
            return 0
        }
        // Statuses of this round may have come before this answer is read.
        if status.folderAccessRound > self.status.folderAccessRound {
            self.status = status
        }
        return status.folderAccessRound
    }

    /// Locks the cursor to the computer it is on, so it crosses no edge,
    /// or unlocks it. The lock hotkey toggles the same lock.
    func setLocked(_ locked: Bool) async throws {
        var set = Inputweave_App_V1_SetLocked()
        set.locked = locked
        if case .status(let status) = try await request(.setLocked(set)) {
            self.status = status
        }
    }

    /// Moves the cursor to the middle of a device's main screen; for this
    /// device, the keyboard and mouse come back here. A refusal's code says
    /// why it cannot go there.
    func jump(to deviceID: String) async throws {
        var jump = Inputweave_App_V1_JumpTo()
        jump.deviceID = deviceID
        if case .status(let status) = try await request(.jumpTo(jump)) {
            self.status = status
        }
    }

    /// How long the cursor rests at an edge before it crosses to the next
    /// computer, in milliseconds; 0 crosses at once.
    func setEdgeDwell(ms: UInt32) async throws {
        var set = Inputweave_App_V1_SetEdgeOptions()
        set.dwellMs = ms
        if case .status(let status) = try await request(.setEdgeOptions(set)) {
            self.status = status
        }
    }

    /// Sets every hotkey at once, as the service writes them
    /// (`Hotkeys` in app.proto). A refusal's code says what is wrong:
    /// "needs_modifier", "unknown_key" or "duplicate".
    func setHotkeys(_ hotkeys: Inputweave_App_V1_Hotkeys) async throws {
        var set = Inputweave_App_V1_SetHotkeys()
        set.hotkeys = hotkeys
        if case .status(let status) = try await request(.setHotkeys(set)) {
            self.status = status
        }
    }

    /// Whether relative mode turns on by itself while an app on the
    /// computer the cursor is on holds the pointer, and off when it lets go.
    func setRelativeAuto(_ on: Bool) async throws {
        var set = Inputweave_App_V1_SetRelativeAuto()
        set.on = on
        if case .status(let status) = try await request(.setRelativeAuto(set)) {
            self.status = status
        }
    }

    /// Makes the cursor cross to another computer only while `modifier`
    /// is held, or (`.none`) whenever it reaches the edge.
    func setSwitchModifier(_ modifier: Inputweave_App_V1_SwitchModifier) async throws {
        var set = Inputweave_App_V1_SetSwitchModifier()
        set.modifier = modifier
        if case .status(let status) = try await request(.setSwitchModifier(set)) {
            self.status = status
        }
    }

    /// A key the user entered; the service holds it until the app has
    /// activated it with Polar (`LicenseController`). A refusal's code says
    /// why ("invalid_license_key", "license_unavailable").
    @discardableResult
    func setLicenseKey(_ key: String) async throws -> Inputweave_App_V1_License {
        var set = Inputweave_App_V1_SetLicenseKey()
        set.key = key
        if case .status(let status) = try await request(.setLicenseKey(set)) {
            self.status = status
        }
        return status.license
    }

    /// What Polar answered about the license (docs/architecture.md §11).
    func recordLicense(_ result: Inputweave_App_V1_RecordLicenseResult) async throws {
        if case .status(let status) = try await request(.recordLicenseResult(result)) {
            self.status = status
        }
    }

    /// Removes the license from this computer while it is still `key`
    /// with `activationID`, as the app saw it when it asked; refused with
    /// "license_stale" once it is another.
    func removeLicense(key: String, activationID: String) async throws {
        var remove = Inputweave_App_V1_RemoveLicense()
        remove.key = key
        remove.activationID = activationID
        if case .status(let status) = try await request(.removeLicense(remove)) {
            self.status = status
        }
    }

    /// Applies this computer's license to another device, which activates
    /// it with Polar itself; how it goes shows in `License.devices`. A
    /// refusal's code says why ("not_licensed", "unknown_device",
    /// "license_full", "licensed_elsewhere", "license_unavailable",
    /// "unsupported").
    func applyLicense(deviceID: String) async throws {
        var apply = Inputweave_App_V1_ApplyLicense()
        apply.deviceID = deviceID
        if case .status(let status) = try await request(.applyLicense(apply)) {
            self.status = status
        }
    }

    /// Revokes this computer's license on another device (`activationID`,
    /// its activation, and whether Polar `deactivated` it, which the device
    /// otherwise does itself), or takes back applying it there (no
    /// activation). A refusal's code says why ("unknown_device",
    /// "license_legacy", "license_stale", "invalid_license_result",
    /// "not_licensed", "license_unavailable", "unsupported").
    func revokeLicense(deviceID: String, activationID: String, deactivated: Bool) async throws {
        var revoke = Inputweave_App_V1_RevokeLicense()
        revoke.deviceID = deviceID
        revoke.activationID = activationID
        revoke.deactivated = deactivated
        if case .status(let status) = try await request(.revokeLicense(revoke)) {
            self.status = status
        }
    }

    /// Polar freed an activation of a key this computer had before, which
    /// the service listed to be freed (`License.to_free`): it forgets it.
    func licenseActivationFreed(_ activationID: String) async throws {
        var freed = Inputweave_App_V1_LicenseActivationFreed()
        freed.activationID = activationID
        if case .status(let status) = try await request(.licenseActivationFreed(freed)) {
            self.status = status
        }
    }

    func setSwapGUIAndControl(_ deviceID: String, _ swap: Bool) async throws {
        var set = Inputweave_App_V1_SetDeviceOptions()
        set.deviceID = deviceID
        set.swapGuiAndControl = swap
        try await request(.setDeviceOptions(set))
    }

    /// Has this computer and every connected one show their names on their
    /// screens for a moment.
    func identifyScreens() async throws {
        try await request(.identifyScreens(.init()))
    }

    /// Names another computer on this Mac only; an empty name shows the
    /// name it gives itself again. A refusal's code says why
    /// ("invalid_name", "unknown_device").
    func renameDevice(_ deviceID: String, to name: String) async throws {
        var rename = Inputweave_App_V1_RenameDevice()
        rename.deviceID = deviceID
        rename.name = name
        if case .status(let status) = try await request(.renameDevice(rename)) {
            self.status = status
        }
    }

    /// Stops sharing the clipboard or files with another computer, or
    /// shares them again; only this Mac stops, the other is not told.
    func setDeviceSharing(_ deviceID: String, clipboardOff: Bool, filesOff: Bool) async throws {
        var set = Inputweave_App_V1_SetDeviceSharing()
        set.deviceID = deviceID
        set.clipboardOff = clipboardOff
        set.filesOff = filesOff
        if case .status(let status) = try await request(.setDeviceSharing(set)) {
            self.status = status
        }
    }

    /// Makes the cursor move faster or slower on another computer while
    /// this Mac controls it, in percent of how the mouse moves it here; only
    /// this Mac uses it. A refusal's code says why ("invalid_speed",
    /// "unknown_device").
    func setDeviceSpeed(_ deviceID: String, percent: Int) async throws {
        var set = Inputweave_App_V1_SetDeviceSpeed()
        set.deviceID = deviceID
        set.percent = UInt32(clamping: percent)
        if case .status(let status) = try await request(.setDeviceSpeed(set)) {
            self.status = status
        }
    }

    /// Wakes another computer from sleep over the network, as it told this
    /// one how to (`DeviceStatus.can_wake`). Answered once the wake-up is
    /// sent; how it goes shows in its `DeviceStatus.wake`. A refusal's code
    /// says why ("unknown_device", "connected", "no_wake_data", "not_sent").
    func wakeDevice(_ deviceID: String) async throws {
        wakeRequests.begin(deviceID)
        defer { wakeRequests.end(deviceID) }
        var wake = Inputweave_App_V1_WakeDevice()
        wake.deviceID = deviceID
        if case .status(let status) = try await request(.wakeDevice(wake)) {
            self.status = status
        }
    }

    /// Whether pushing the cursor against an edge toward an offline
    /// computer that can be woken wakes it.
    func setWakeOnPush(_ on: Bool) async throws {
        var set = Inputweave_App_V1_SetWakeOnPush()
        set.on = on
        if case .status(let status) = try await request(.setWakeOnPush(set)) {
            self.status = status
        }
    }

    /// Takes a computer offline for now, this one or another, or brings it
    /// back: the whole group then neither controls it nor shares the
    /// clipboard or files with it, until someone brings it back. Answered
    /// once the service has the change. A refusal's code says why
    /// ("unknown_device"; "offline_for_now" for another computer while
    /// this one is offline for now, as it can only bring itself back;
    /// "too_many" or "exhausted" when the group can list no more).
    func setOfflineForNow(_ deviceID: String, _ offline: Bool) async throws {
        var set = Inputweave_App_V1_SetOfflineForNow()
        set.deviceID = deviceID
        set.offline = offline
        if case .status(let status) = try await request(.setOfflineForNow(set)) {
            self.status = status
        }
    }

    /// Adds a profile named `name`, which the service makes unique with
    /// " 2", " 3"…, before Other networks. Answered with the new profile's
    /// id, nil when the answer does not list one. A refusal's code says
    /// why ("invalid_name", "too_many_profiles").
    @discardableResult
    func addProfile(named name: String) async throws -> String? {
        let before = Set(status.profiles.map(\.id))
        var add = Inputweave_App_V1_AddProfile()
        add.name = name
        guard case .status(let status) = try await request(.addProfile(add)) else { return nil }
        self.status = status
        return status.profiles.first { !before.contains($0.id) }?.id
    }

    /// Changes what is given of a profile, and only that: its name, whether
    /// it pauses sharing, the clipboard and files, and which computers it
    /// connects. A refusal's code says why ("unknown_profile",
    /// "fixed_profile" to rename Default or Other networks,
    /// "invalid_name", "unknown_device").
    func editProfile(
        _ id: String, name: String? = nil, paused: Bool? = nil, clipboard: Bool? = nil, files: Bool? = nil,
        devices: [(id: String, connect: Bool)] = []
    ) async throws {
        var edit = Inputweave_App_V1_EditProfile()
        edit.profileID = id
        if let name { edit.name = name }
        if let paused { edit.paused = paused }
        if let clipboard { edit.clipboard = clipboard }
        if let files { edit.files = files }
        edit.devices = devices.map { device in
            var entry = Inputweave_App_V1_DeviceInProfile()
            entry.deviceID = device.id
            entry.connect = device.connect
            return entry
        }
        if case .status(let status) = try await request(.editProfile(edit)) {
            self.status = status
        }
    }

    /// Removes one of the user's own profiles with its networks; removing
    /// the chosen one goes back to Automatic.
    func removeProfile(_ id: String) async throws {
        var remove = Inputweave_App_V1_RemoveProfile()
        remove.profileID = id
        if case .status(let status) = try await request(.removeProfile(remove)) {
            self.status = status
        }
    }

    /// Uses a profile whatever the network, or with an empty id goes back
    /// to Automatic, which picks it by the network.
    func selectProfile(_ id: String) async throws {
        var select = Inputweave_App_V1_SelectProfile()
        select.profileID = id
        if case .status(let status) = try await request(.selectProfile(select)) {
            self.status = status
        }
    }

    /// Adds the network this Mac is on now to a profile, taking it from the
    /// one that had it. A refusal's code says why ("other_networks",
    /// "no_network", "too_many_networks", "unknown_profile").
    func addNetworkToProfile(_ id: String) async throws {
        var add = Inputweave_App_V1_AddNetworkToProfile()
        add.profileID = id
        if case .status(let status) = try await request(.addNetworkToProfile(add)) {
            self.status = status
        }
    }

    /// Takes a network out of the profile that lists it.
    func removeNetwork(_ id: String) async throws {
        var remove = Inputweave_App_V1_RemoveNetwork()
        remove.networkID = id
        if case .status(let status) = try await request(.removeNetwork(remove)) {
            self.status = status
        }
    }

    /// Recognizes networks by their Wi-Fi name too, which this app then
    /// reads and reports (`WifiNameReporter`), or only by their router.
    func setWifiNames(_ on: Bool) async throws {
        var set = Inputweave_App_V1_SetWifiNames()
        set.on = on
        if case .status(let status) = try await request(.setWifiNames(set)) {
            self.status = status
        }
    }

    /// Tells the service the Wi-Fi name this app read for the network with
    /// the id `networkID`, empty when this Mac is not on Wi-Fi.
    func reportWifiName(networkID: String, ssid: String) async throws {
        var report = Inputweave_App_V1_ReportWifiName()
        report.networkID = networkID
        report.ssid = ssid
        if case .status(let status) = try await request(.reportWifiName(report)) {
            self.status = status
        }
    }

    /// Moves devices' desktop origins on the shared canvas.
    func move(_ positions: [(id: String, x: Double, y: Double)]) async throws {
        var set = Inputweave_App_V1_SetArrangement()
        set.positions = positions.map { position in
            var p = Inputweave_App_V1_DevicePosition()
            p.deviceID = position.id
            p.x = position.x
            p.y = position.y
            return p
        }
        if case .arrangement(let arrangement) = try await request(.setArrangement(set)) {
            self.arrangement = arrangement
        }
    }

    /// Sends files to a device; progress shows in `transfers`.
    func sendFiles(_ urls: [URL], to deviceID: String) async throws {
        var send = Inputweave_App_V1_SendFiles()
        send.deviceID = deviceID
        send.paths = urls.map(\.path)
        if case .transfers(let transfers) = try await request(.sendFiles(send)) {
            self.transfers = transfers
        }
    }

    func cancelTransfer(_ id: String) async throws {
        var cancel = Inputweave_App_V1_CancelTransfer()
        cancel.transferID = id
        try await request(.cancelTransfer(cancel))
    }

    /// Takes a finished transfer off the list; the file stays where it is.
    func removeTransfer(_ id: String) async throws {
        var remove = Inputweave_App_V1_RemoveTransfer()
        remove.transferID = id
        if case .transfers(let transfers) = try await request(.removeTransfer(remove)) {
            self.transfers = transfers
        }
    }

    /// Takes every finished transfer off the list; the files stay where
    /// they are, and transfers still going stay listed.
    func clearTransfers() async throws {
        if case .transfers(let transfers) = try await request(.clearTransfers_p(.init())) {
            self.transfers = transfers
        }
    }

    /// Accepts or declines a file offered to this computer.
    func answerFileOffer(_ id: String, accept: Bool) async throws {
        var answer = Inputweave_App_V1_AnswerFileOffer()
        answer.transferID = id
        answer.accept = accept
        try await request(.answerFileOffer(answer))
    }

    /// Where received files go (an empty path means the default folder),
    /// and whether to ask first.
    func setFileSettings(folderPath: String, ask: Bool) async throws {
        var set = Inputweave_App_V1_SetFileSettings()
        set.receiveFolder = folderPath
        set.askBeforeReceiving = ask
        if case .transfers(let transfers) = try await request(.setFileSettings(set)) {
            self.transfers = transfers
        }
    }

    /// A drag of files from here entered a strip along a seam: the service
    /// lists the items and lets the cursor cross with the button held.
    func beginDrag(_ urls: [URL]) async throws -> Inputweave_App_V1_DragBegun {
        var begin = Inputweave_App_V1_BeginDrag()
        begin.items = urls.map { url in
            var item = Inputweave_App_V1_DragPath()
            item.path = url.path
            // From the URL alone: the service looks for itself.
            item.isDir = url.hasDirectoryPath
            return item
        }
        guard case .dragBegun(let begun) = try await request(.beginDrag(begin)) else {
            throw AgentRefusal(code: "unexpected", detail: "no drag")
        }
        return begun
    }

    /// The drag from here ended without crossing.
    func endDrag(_ id: String) async throws {
        var end = Inputweave_App_V1_EndDrag()
        end.dragID = id
        if case .status(let status) = try await request(.endDrag(end)) {
            self.status = status
        }
    }

    /// The drag another computer offered was dropped in `destination`.
    func acceptDrag(_ id: String, destination: String) async throws {
        var accept = Inputweave_App_V1_AcceptDrag()
        accept.dragID = id
        accept.destination = destination
        if case .transfers(let transfers) = try await request(.acceptDrag(accept)) {
            self.transfers = transfers
        }
    }

    /// The drag another computer offered was not dropped here.
    func declineDrag(_ id: String) async throws {
        var decline = Inputweave_App_V1_DeclineDrag()
        decline.dragID = id
        if case .status(let status) = try await request(.declineDrag(decline)) {
            self.status = status
        }
    }

    /// The service's report for bug reports: versions, state,
    /// configuration and the end of its logs, without anything secret.
    func diagnostics() async throws -> String {
        guard case .diagnostics(let diagnostics) = try await request(.getDiagnostics(.init())) else {
            throw AgentRefusal(code: "unexpected", detail: "no diagnostics")
        }
        return diagnostics.report
    }

    /// Whether the service is connected now, or gets connected within
    /// `limit`: `wait(until:)` on the model's own change notification,
    /// raced against a timer.
    func connected(within limit: Duration) async -> Bool {
        await withTaskGroup(of: Void.self) { group in
            group.addTask { await self.wait { self.link == .connected } }
            group.addTask { try? await Task.sleep(for: limit) }
            await group.next()
            group.cancelAll()
        }
        return link == .connected
    }

    // MARK: - Derived state

    var thisDeviceID: String { status.thisDeviceID }

    /// Out of reach for longer than this, the service is not responding
    /// rather than starting.
    static let unresponsiveAfter: TimeInterval = 10

    /// Whether the service has been out of reach for longer than a restart
    /// takes, as of `now`. While it is updating it is not counted.
    func isUnresponsive(at now: Date) -> Bool {
        guard link == .connecting, let since = unreachableSince else { return false }
        return now.timeIntervalSince(since) > Self.unresponsiveAfter
    }

    func device(_ id: String) -> DeviceStatus? {
        status.devices.first { $0.id == id }
    }

    func name(of id: String) -> String {
        if id == status.thisDeviceID { return status.thisDeviceName }
        return device(id)?.name ?? arrangement.devices.first { $0.id == id }?.name ?? id
    }

    /// The switch delay the General page's option turns on: the last one in
    /// effect while the app ran, or else the service's default.
    var switchDelayMs: UInt32 {
        lastSwitchDelayMs ?? status.defaultDwellMs
    }

    /// The device the cursor is on: the active one, or this one when the
    /// service has not said.
    var cursorDeviceID: String {
        status.activeDeviceID.isEmpty ? status.thisDeviceID : status.activeDeviceID
    }

    /// Whether the cursor can be sent to a computer: not the one it is on,
    /// and not while it could not go anywhere. Moving it to where it already
    /// is would only warp the pointer.
    func canJump(to id: String) -> Bool {
        link == .connected && status.sharing && !status.locked
            && id != cursorDeviceID
            && (id == thisDeviceID || device(id)?.state == .connected)
            && !outOfReach(id)
    }

    /// Whether input from here cannot reach a computer: its screen is
    /// locked where it cannot, it takes none now (a Linux desktop that has
    /// not allowed remote control), or its user paused sharing there. The
    /// service keeps the cursor off it.
    func outOfReach(_ id: String) -> Bool {
        device(id).map {
            ($0.screenLocked && !$0.lockScreenInput) || $0.inputUnavailable || $0.sharingPaused
        } ?? false
    }

    /// Whether this device is controlling another one right now.
    var controllingOther: Bool {
        !status.activeDeviceID.isEmpty && status.activeDeviceID != status.thisDeviceID
    }

    var missingPermissions: [Inputweave_App_V1_Permission] {
        status.permissions.filter { !$0.granted }.map(\.permission)
    }

    /// The background service asks macOS for the folders it protects;
    /// services before it, and the login window's, list none.
    var asksForFolders: Bool {
        !status.folderAccess.isEmpty
    }

    /// macOS may be asking the user about one of those folders now.
    var askingForFolders: Bool {
        status.folderAccess.contains { $0.state == .asking }
    }

    /// The folders are still being listed in `round`, as
    /// `requestFolderAccess` returned it; the status is of that round or a
    /// later one by then. False once the service started another round, or
    /// started over.
    func listingFolders(in round: UInt64) -> Bool {
        status.folderAccessRound == round && askingForFolders
    }

    /// Some folder was never asked about on this Mac.
    var foldersToAsk: Bool {
        asksForFolders && (!status.folderAccessAsked || status.folderAccess.contains { $0.state == .unknown })
    }

    /// Devices files can be sent to right now: those without a reason they
    /// can't (`SendTargets.unavailable`: connected, and files not turned off
    /// with them or in the profile in use).
    var fileReceivers: [DeviceStatus] {
        let profileFilesOff = ProfileText.filesOff(status)
        return status.devices.filter { SendTargets.unavailable($0, profileFilesOff: profileFilesOff) == nil }
    }

    /// Whether files can be sent to a computer right now.
    func canReceiveFiles(_ id: String) -> Bool {
        fileReceivers.contains { $0.id == id }
    }

    /// Whether files are turned off with a computer on this Mac.
    func filesOff(_ id: String) -> Bool {
        device(id)?.filesOff == true
    }

    /// Whether this Mac's user gave a computer a name of their own: the
    /// name it gives itself, when it differs, or nil.
    func reportedName(of id: String) -> String? {
        guard let device = device(id), !device.reportedName.isEmpty, device.reportedName != device.name else {
            return nil
        }
        return device.reportedName
    }
}

#if DEBUG
extension AgentModel {
    /// A model with fixed state, for previews and snapshot tests.
    static func sample(
        status: AgentStatus, pairing: PairingState = PairingState(),
        nearby: [NearbyDevice] = [], arrangement: Arrangement = Arrangement(),
        transfers: Transfers = Transfers(), link: Link = .connected, unreachableSince: Date? = nil,
        relativeRefusal: RelativeRefusal? = nil
    ) -> AgentModel {
        let model = AgentModel(socketPath: "/nonexistent")
        model.link = link
        model.unreachableSince = link == .connected ? nil : unreachableSince ?? .now
        model.status = status
        model.pairing = pairing
        model.nearby = nearby
        model.arrangement = arrangement
        model.transfers = transfers
        model.relativeRefusal = relativeRefusal
        return model
    }
}
#endif

extension Bundle {
    var shortVersion: String {
        infoDictionary?["CFBundleShortVersionString"] as? String ?? "0"
    }
}
