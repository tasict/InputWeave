import CryptoKit
import Foundation
import XCTest

/// The app's IPC client against the real background service: the
/// `inputweave` binary embedded in the app built alongside these tests, run
/// with mock input backends and its own files in a temporary directory,
/// which is its home too. Its Downloads, Desktop and Documents are in that
/// directory (`folders`), so it never lists this Mac's own.
@MainActor
final class AgentModelTests: XCTestCase {
    private var agent: Process?
    private var app: URL!
    private var directory: URL!

    override func setUp() async throws {
        app = Bundle(for: Self.self).bundleURL.deletingLastPathComponent().appending(path: "InputWeave.app")
        let binary = app.appending(path: "Contents/Library/LoginItems/InputWeave Agent.app/Contents/MacOS/inputweave")
        try XCTSkipUnless(FileManager.default.isExecutableFile(atPath: binary.path), "build the InputWeave app first")
        // Unix socket paths are limited to 104 bytes; keep it short.
        directory = FileManager.default.temporaryDirectory.appending(path: "iw-\(UUID().uuidString.prefix(8))")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let config = """
            [device]
            id = "mac"
            name = "Test Mac"

            [network]
            listen = "127.0.0.1:0"
            discovery = false

            [paths]
            ipc_socket = "\(directory.appending(path: "a.sock").path)"

            [backend]
            kind = "mock"
            folders = "\(directory.path)"
            displays = [{ id = "builtin", x = 0, y = 0, width = 1512, height = 982, scale = 2.0, primary = true }]

            # Trusted without pairing, never reached.
            [[peers]]
            id = "pc"
            name = "Windows Desktop"
            fingerprint = "\(Array(repeating: "ab", count: 32).joined(separator: ":"))"
            """
        try config.write(to: directory.appending(path: "config.toml"), atomically: true, encoding: .utf8)
        let process = Process()
        process.executableURL = binary
        process.arguments = ["agent", "--config", directory.appending(path: "config.toml").path]
        process.environment = ProcessInfo.processInfo.environment.merging(["HOME": directory.path]) { $1 }
        process.standardError = FileHandle.nullDevice
        try process.run()
        agent = process
    }

    override func tearDown() async throws {
        agent?.terminate()
        agent?.waitUntilExit()
        if let directory { try? FileManager.default.removeItem(at: directory) }
    }

    private func eventually(_ what: String, _ condition: () -> Bool) async throws {
        let deadline = Date.now.addingTimeInterval(10)
        while !condition() {
            if Date.now > deadline { XCTFail("timed out waiting for \(what)"); return }
            try await Task.sleep(for: .milliseconds(50))
        }
    }

    func testTalksToTheBackgroundService() async throws {
        let model = AgentModel(socketPath: directory.appending(path: "a.sock").path)
        model.start()
        // As the Services menu waits for it.
        let connected = await model.connected(within: .seconds(10))
        XCTAssertTrue(connected)
        XCTAssertEqual(model.link, .connected)
        XCTAssertEqual(model.status.thisDeviceID, "mac")
        XCTAssertEqual(model.status.thisDeviceName, "Test Mac")
        XCTAssertTrue(model.status.sharing)
        XCTAssertEqual(model.arrangement.devices.first?.screens.first?.width, 1512)

        // The service names its build: the SHA-256 of its signed executable,
        // which is also what the app's LaunchAgent plist says it carries.
        let binary = try XCTUnwrap(agent?.executableURL)
        let digest = SHA256.hash(data: try Data(contentsOf: binary)).map { String(format: "%02x", $0) }.joined()
        XCTAssertEqual(model.agentDigest, digest)
        let plist = app.appending(path: "Contents/Library/LaunchAgents/cc.ctsite.inputweave.agent.plist")
        XCTAssertEqual(OutdatedService.bundledDigest(plist: plist), digest)

        try await model.setSharing(false)
        try await eventually("sharing paused") { !model.status.sharing }

        try await model.rename("Renamed Mac")
        try await eventually("the new name") { model.status.thisDeviceName == "Renamed Mac" }

        do {
            try await model.unpair("ghost")
            XCTFail("unpairing an unknown device succeeded")
        } catch let refusal as AgentRefusal {
            XCTAssertEqual(refusal.code, "not_paired")
        }

        // The service goes away and comes back: the model reconnects.
        agent?.terminate()
        agent?.waitUntilExit()
        try await eventually("losing the connection") { model.link == .connecting }
        try await setUpAgentAgain()
        try await eventually("a new connection") { model.link == .connected }
        XCTAssertEqual(model.status.thisDeviceName, "Renamed Mac")
    }

    func testFileSettingsAndRefusedSends() async throws {
        let model = AgentModel(socketPath: directory.appending(path: "a.sock").path)
        model.start()
        try await eventually("a connection") { model.link == .connected }
        XCTAssertFalse(model.transfers.receiveFolder.isEmpty)

        let report = try await model.diagnostics()
        XCTAssertTrue(report.contains("InputWeave diagnostics"), report)
        XCTAssertTrue(report.contains("Test Mac"), report)

        let folder = directory.appending(path: "Received")
        try await model.setFileSettings(folderPath: folder.path, ask: true)
        XCTAssertEqual(model.transfers.receiveFolder, folder.path)
        XCTAssertTrue(model.transfers.askBeforeReceiving)

        let file = directory.appending(path: "notes.txt")
        try "hello".write(to: file, atomically: true, encoding: .utf8)
        do {
            try await model.sendFiles([file], to: "ghost")
            XCTFail("sending to an unknown device succeeded")
        } catch let refusal as AgentRefusal {
            XCTAssertEqual(refusal.code, "unknown_device")
        }

        // Nothing was sent, so there is nothing to take off the history.
        do {
            try await model.removeTransfer("18f0a-1-1")
            XCTFail("removing an unknown transfer succeeded")
        } catch let refusal as AgentRefusal {
            XCTAssertEqual(refusal.code, "not_found")
        }
        try await model.clearTransfers()
        XCTAssertTrue(model.transfers.transfers.isEmpty)
    }

    /// The drag commands reach the service, which refuses what makes no
    /// sense without a drag going on.
    func testDragCommands() async throws {
        let model = AgentModel(socketPath: directory.appending(path: "a.sock").path)
        model.start()
        try await eventually("a connection") { model.link == .connected }
        XCTAssertFalse(model.status.hasDragging)

        let file = directory.appending(path: "notes.txt")
        try "hello".write(to: file, atomically: true, encoding: .utf8)
        func refusal(_ body: () async throws -> Void) async -> String? {
            do {
                try await body()
                return nil
            } catch {
                return (error as? AgentRefusal)?.code
            }
        }
        // No mouse button is down on the mock input.
        let begin = await refusal { _ = try await model.beginDrag([file, directory]) }
        XCTAssertEqual(begin, "not_dragging")
        let end = await refusal { try await model.endDrag("18f0a-1-1") }
        XCTAssertEqual(end, "not_found")
        let accept = await refusal { try await model.acceptDrag("18f0a-1-1", destination: self.directory.path) }
        XCTAssertEqual(accept, "not_found")
        let relative = await refusal { try await model.acceptDrag("18f0a-1-1", destination: "Desktop") }
        XCTAssertEqual(relative, "invalid_folder")
        let decline = await refusal { try await model.declineDrag("18f0a-1-1") }
        XCTAssertEqual(decline, "not_found")
    }

    func testLockJumpAndSwitchDelay() async throws {
        let model = AgentModel(socketPath: directory.appending(path: "a.sock").path)
        model.start()
        try await eventually("a connection") { model.link == .connected }
        XCTAssertFalse(model.status.locked)
        XCTAssertEqual(model.status.edgeDwellMs, 0)

        try await model.setLocked(true)
        XCTAssertTrue(model.status.locked)
        do {
            try await model.jump(to: "mac")
            XCTFail("jumping while locked succeeded")
        } catch let refusal as AgentRefusal {
            XCTAssertEqual(refusal.code, "locked")
        }
        try await model.setLocked(false)
        XCTAssertFalse(model.status.locked)

        // Back to this computer, where the cursor already is.
        try await model.jump(to: "mac")
        do {
            try await model.jump(to: "ghost")
            XCTFail("jumping to an unknown device succeeded")
        } catch let refusal as AgentRefusal {
            XCTAssertEqual(refusal.code, "unknown_device")
        }

        // The option turns on the service's default delay.
        XCTAssertEqual(model.status.defaultDwellMs, 150)
        XCTAssertEqual(model.switchDelayMs, 150)
        try await model.setEdgeDwell(ms: model.switchDelayMs)
        XCTAssertEqual(model.status.edgeDwellMs, 150)
        // One set elsewhere (`inputweave ctl dwell`) is kept across off and on.
        try await model.setEdgeDwell(ms: 300)
        try await model.setEdgeDwell(ms: 0)
        XCTAssertEqual(model.switchDelayMs, 300)
        do {
            try await model.setEdgeDwell(ms: 5000)
            XCTFail("a five second switch delay was accepted")
        } catch let refusal as AgentRefusal {
            XCTAssertEqual(refusal.code, "invalid_dwell")
        }
        try await model.setEdgeDwell(ms: 0)
        XCTAssertEqual(model.status.edgeDwellMs, 0)
    }

    /// Identifying the screens, naming another computer and turning things
    /// off with it, against a computer the configuration file trusts.
    func testIdentifyRenameAndDeviceSharing() async throws {
        let model = AgentModel(socketPath: directory.appending(path: "a.sock").path)
        model.start()
        try await eventually("a connection") { model.link == .connected }
        try await eventually("the trusted computer") { model.device("pc") != nil }

        XCTAssertNil(model.screenIdentification)
        try await model.identifyScreens()
        try await eventually("the screens identified") { model.screenIdentification != nil }
        XCTAssertEqual(model.screenIdentification?.durationMs, 2500)

        try await model.renameDevice("pc", to: "Studio PC")
        XCTAssertEqual(model.device("pc")?.name, "Studio PC")
        XCTAssertEqual(model.reportedName(of: "pc"), "Windows Desktop")
        try await eventually("the arrangement's name") {
            model.arrangement.devices.first { $0.id == "pc" }?.name == "Studio PC"
        }
        func refusal(_ body: () async throws -> Void) async -> String? {
            do {
                try await body()
                return nil
            } catch {
                return (error as? AgentRefusal)?.code
            }
        }
        let tooLong = await refusal { try await model.renameDevice("pc", to: String(repeating: "x", count: 65)) }
        XCTAssertEqual(tooLong, "invalid_name")
        let ghost = await refusal { try await model.renameDevice("ghost", to: "x") }
        XCTAssertEqual(ghost, "unknown_device")
        try await model.renameDevice("pc", to: "")
        XCTAssertEqual(model.device("pc")?.name, "Windows Desktop")
        XCTAssertNil(model.reportedName(of: "pc"))

        try await model.setDeviceSharing("pc", clipboardOff: true, filesOff: true)
        XCTAssertEqual(model.device("pc")?.clipboardOff, true)
        XCTAssertTrue(model.filesOff("pc"))
        let off = await refusal { try await model.setDeviceSharing("ghost", clipboardOff: true, filesOff: false) }
        XCTAssertEqual(off, "unknown_device")
        try await model.setDeviceSharing("pc", clipboardOff: false, filesOff: false)
        XCTAssertFalse(model.filesOff("pc"))

        // The cursor speed, here for a computer from the configuration file.
        XCTAssertEqual(CursorSpeed.shown(model.device("pc"), thisDevice: false), 100)
        try await model.setDeviceSpeed("pc", percent: 150)
        XCTAssertEqual(model.device("pc")?.speedPercent, 150)
        let invalid = await refusal { try await model.setDeviceSpeed("pc", percent: 155) }
        XCTAssertEqual(invalid, "invalid_speed")
        let unknown = await refusal { try await model.setDeviceSpeed("ghost", percent: 150) }
        XCTAssertEqual(unknown, "unknown_device")
        XCTAssertEqual(model.device("pc")?.speedPercent, 150)
        try await model.setDeviceSpeed("pc", percent: 100)
        XCTAssertEqual(model.device("pc")?.speedPercent, 100)
    }

    func testHotkeysJumpNumbersAndSwitchModifier() async throws {
        let model = AgentModel(socketPath: directory.appending(path: "a.sock").path)
        model.start()
        try await eventually("a connection") { model.link == .connected }
        XCTAssertTrue(model.status.hasHotkeys)
        XCTAssertEqual(model.status.hotkeys, ShortcutText.defaults)
        XCTAssertEqual(model.status.switchModifier, SwitchModifier.none)
        // This computer, alone in the arrangement, is number 1.
        XCTAssertEqual(model.arrangement.devices.first?.jumpSlot, 1)
        XCTAssertEqual(model.jumpShortcut(slot: 1), "⌃⌥⇧1")

        var hotkeys = ShortcutText.defaults
        hotkeys.lock = "shift+control+KeyK"
        hotkeys.jumpModifiers = ""
        try await model.setHotkeys(hotkeys)
        // As the service writes it.
        XCTAssertEqual(model.status.hotkeys.lock, "control+shift+KeyK")
        XCTAssertEqual(model.lockShortcut, "⌃⇧K")
        XCTAssertNil(model.jumpShortcut(slot: 1))

        func refusal(_ hotkeys: Hotkeys) async -> String? {
            do {
                try await model.setHotkeys(hotkeys)
                return nil
            } catch {
                return (error as? AgentRefusal)?.code
            }
        }
        var plain = hotkeys
        plain.lock = "KeyK"
        let needsModifier = await refusal(plain)
        XCTAssertEqual(needsModifier, "needs_modifier")
        var same = hotkeys
        same.lock = same.takeBack
        let duplicate = await refusal(same)
        XCTAssertEqual(duplicate, "duplicate")
        try await model.setHotkeys(ShortcutText.defaults)
        XCTAssertEqual(model.status.hotkeys, ShortcutText.defaults)

        try await model.setSwitchModifier(.shift)
        XCTAssertEqual(model.status.switchModifier, .shift)
        try await model.setSwitchModifier(SwitchModifier.none)
        XCTAssertEqual(model.status.switchModifier, SwitchModifier.none)
    }

    func testPreviewSharing() async throws {
        let model = AgentModel(socketPath: directory.appending(path: "a.sock").path)
        model.start()
        try await eventually("a connection") { model.link == .connected }
        XCTAssertTrue(model.status.previewSharing)

        try await model.setPreviewSharing(false)
        XCTAssertFalse(model.status.previewSharing)
        try await model.setPreviewSharing(true)
        XCTAssertTrue(model.status.previewSharing)
    }

    /// The mock service's lock screen takes no input from other
    /// computers, so the login window cannot either: turning it on is
    /// refused, and turning it off works.
    func testLoginWindowAccess() async throws {
        let model = AgentModel(socketPath: directory.appending(path: "a.sock").path)
        model.start()
        try await eventually("a connection") { model.link == .connected }
        XCTAssertFalse(model.status.lockScreenInput)
        XCTAssertFalse(model.status.loginWindowAccess)
        do {
            try await model.setLoginWindowAccess(true)
            XCTFail("turned on without lock screen input")
        } catch let refusal as AgentRefusal {
            XCTAssertEqual(refusal.code, "unsupported")
        }
        try await model.setLoginWindowAccess(false)
        XCTAssertFalse(model.status.loginWindowAccess)
    }

    /// Relative mode: the hotkey row and
    /// the automatic switch, and turning it on refused while the cursor
    /// is on this computer.
    func testRelativeMode() async throws {
        let model = AgentModel(socketPath: directory.appending(path: "a.sock").path)
        model.start()
        try await eventually("a connection") { model.link == .connected }
        XCTAssertTrue(model.relativeSupported)
        XCTAssertEqual(model.relativeShortcut, "⌃⌥⇧G")
        XCTAssertTrue(model.status.relativeAuto)
        XCTAssertEqual(model.status.relativeMouse, "")

        try await model.setRelativeAuto(false)
        XCTAssertFalse(model.status.relativeAuto)
        try await model.setRelativeAuto(true)
        XCTAssertTrue(model.status.relativeAuto)

        var hotkeys = model.hotkeys
        hotkeys[.relative] = "control+alt+KeyR"
        try await model.setHotkeys(hotkeys)
        XCTAssertEqual(model.relativeShortcut, "⌃⌥R")
        var clash = model.hotkeys
        clash.lock = "control+alt+KeyR"
        do {
            try await model.setHotkeys(clash)
            XCTFail("took a clash with the relative mode hotkey")
        } catch let refusal as AgentRefusal {
            XCTAssertEqual(refusal.code, "duplicate_relative")
        }
        hotkeys[.relative] = ""
        try await model.setHotkeys(hotkeys)
        XCTAssertTrue(model.relativeSupported)
        XCTAssertNil(model.relativeShortcut)
        try await model.setHotkeys(ShortcutText.defaults)
        XCTAssertTrue(ShortcutText.isDefault(model.hotkeys, relative: true, doublePress: true))
    }

    /// Single keys for taking control back and the lock, and the double
    /// press of Control, with the service's own answers.
    func testSingleKeysAndTheDoublePress() async throws {
        let model = AgentModel(socketPath: directory.appending(path: "a.sock").path)
        model.start()
        try await eventually("a connection") { model.link == .connected }
        XCTAssertTrue(model.doublePressSupported)
        XCTAssertFalse(model.doublePressOn)

        var hotkeys = model.hotkeys
        hotkeys.takeBack = "F13"
        hotkeys.lock = "F14"
        hotkeys.takeBackDouble = ShortcutText.doubleControl
        try await model.setHotkeys(hotkeys)
        XCTAssertEqual(model.status.hotkeys, hotkeys)
        XCTAssertEqual(model.takeBackShortcut, "F13")
        XCTAssertTrue(model.doublePressOn)

        func refusal(_ hotkeys: Hotkeys) async -> String? {
            do {
                try await model.setHotkeys(hotkeys)
                return nil
            } catch {
                return (error as? AgentRefusal)?.code
            }
        }
        var plain = hotkeys
        plain.lock = "KeyK"
        let needsModifier = await refusal(plain)
        XCTAssertEqual(needsModifier, "needs_modifier")
        var relativeAlone = hotkeys
        relativeAlone.relative = "F14"
        let relativeNeedsModifier = await refusal(relativeAlone)
        XCTAssertEqual(relativeNeedsModifier, "needs_modifier")
        // A Mac never reports Scroll Lock or Pause: refused, as a key that
        // can't be used.
        for key in ["ScrollLock", "Pause"] {
            var unreported = hotkeys
            unreported.lock = key
            let code = await refusal(unreported)
            XCTAssertEqual(code, "unknown_key", key)
            XCTAssertEqual(
                ShortcutText.refusal(code ?? "", changing: .lock, to: unreported, from: hotkeys),
                "This key can't be used in a shortcut.")
        }
        var shadowed = hotkeys
        shadowed.lock = "F13"
        let duplicate = await refusal(shadowed)
        XCTAssertEqual(duplicate, "duplicate")
        // An app from before the double press leaves it out: it stays.
        var older = hotkeys
        older.clearTakeBackDouble()
        older.lock = "F15"
        try await model.setHotkeys(older)
        XCTAssertEqual(model.status.hotkeys.lock, "F15")
        XCTAssertTrue(model.doublePressOn)
        // Reset turns it off too.
        try await model.setHotkeys(ShortcutText.defaults)
        XCTAssertFalse(model.doublePressOn)
        XCTAssertTrue(ShortcutText.isDefault(model.hotkeys, relative: true, doublePress: true))
    }

    /// Waking the computer the configuration file trusts, which never said
    /// how to wake it: refused ("no_wake_data"), and Wake is offered again
    /// once the service answers. Pushing the cursor to wake goes on and
    /// off.
    func testWaking() async throws {
        let model = AgentModel(socketPath: directory.appending(path: "a.sock").path)
        model.start()
        try await eventually("a connection") { model.link == .connected }
        try await eventually("the trusted computer") { model.device("pc") != nil }
        XCTAssertEqual(model.wakeOffer("pc"), .hidden)

        let wake = Task { try await model.wakeDevice("pc") }
        await Task.yield()
        // On its way until the service answers.
        XCTAssertTrue(model.wakeRequests.contains("pc"))
        let refused: String?
        do {
            try await wake.value
            refused = nil
        } catch {
            refused = (error as? AgentRefusal)?.code
        }
        XCTAssertEqual(refused, "no_wake_data")
        XCTAssertFalse(model.wakeRequests.contains("pc"))

        // Forgotten with a status no longer listing it, before the answer.
        let again = Task { try? await model.wakeDevice("pc") }
        await Task.yield()
        XCTAssertTrue(model.wakeRequests.contains("pc"))
        var unlisted = model.status
        unlisted.devices = []
        model.handle(.with { $0.event = .with { $0.statusChanged = unlisted } })
        XCTAssertFalse(model.wakeRequests.contains("pc"))
        await again.value

        XCTAssertTrue(model.wakeSupported)
        try await model.setWakeOnPush(true)
        XCTAssertTrue(model.status.wakeOnPush)
        try await model.setWakeOnPush(false)
        XCTAssertFalse(model.status.wakeOnPush)
    }

    /// Taking the computer the configuration file trusts offline for now
    /// and bringing it back, then this one: while it is, every other
    /// computer is offline, and only this one can be brought back from
    /// here.
    func testOfflineForNow() async throws {
        let model = AgentModel(socketPath: directory.appending(path: "a.sock").path)
        model.start()
        try await eventually("a connection") { model.link == .connected }
        try await eventually("the trusted computer") { model.device("pc") != nil }
        XCTAssertTrue(model.offlineSupported)
        XCTAssertEqual(model.offlineOffer("pc"), .takeOffline)
        let counted = model.status.offlineChanges

        try await model.setOfflineForNow("pc", true)
        let pc = try XCTUnwrap(model.device("pc"))
        XCTAssertTrue(pc.offlineForNow)
        XCTAssertEqual(pc.offlineByDeviceID, "mac")
        XCTAssertEqual(pc.offlineByName, "Test Mac")
        XCTAssertEqual(pc.state, .offline)
        XCTAssertTrue(model.showsOfflineForNow("pc"))
        XCTAssertEqual(model.offlineOffer("pc"), .bringBack)
        XCTAssertEqual(model.status.offlineChanges, counted + 1)
        XCTAssertEqual(model.status.lastOfflineChange.deviceID, "pc")
        XCTAssertTrue(model.status.lastOfflineChange.offline)
        XCTAssertEqual(model.status.lastOfflineChange.byDeviceID, "mac")
        XCTAssertEqual(model.unreachableDevices.map(\.id), [])
        try await model.setOfflineForNow("pc", false)
        XCTAssertEqual(model.device("pc")?.offlineForNow, false)
        XCTAssertEqual(model.offlineOffer("pc"), .takeOffline)

        try await model.setOfflineForNow("mac", true)
        XCTAssertTrue(model.offlineHere)
        XCTAssertEqual(model.status.offlineByDeviceID, "mac")
        XCTAssertTrue(model.status.devices.allSatisfy { $0.state == .offline && !$0.offlineForNow })
        XCTAssertEqual(model.offlineOffer("pc"), .hidden)
        XCTAssertFalse(model.canToggleLock)
        var refused: String?
        do {
            try await model.setOfflineForNow("pc", true)
        } catch {
            refused = (error as? AgentRefusal)?.code
        }
        XCTAssertEqual(refused, "offline_for_now")
        try await model.setOfflineForNow("mac", false)
        XCTAssertFalse(model.offlineHere)

        refused = nil
        do {
            try await model.setOfflineForNow("ghost", true)
        } catch {
            refused = (error as? AgentRefusal)?.code
        }
        XCTAssertEqual(refused, "unknown_device")
    }

    /// Setup has the background service list the folders macOS protects,
    /// here in the test's own folder, where macOS asks nothing.
    func testFolderAccess() async throws {
        for name in ["Downloads", "Desktop"] {
            try FileManager.default.createDirectory(at: directory.appending(path: name), withIntermediateDirectories: true)
        }
        let model = AgentModel(socketPath: directory.appending(path: "a.sock").path)
        model.start()
        try await eventually("a connection") { model.link == .connected }
        XCTAssertEqual(model.status.folderAccess.map(\.folder), [.downloads, .desktop, .documents])
        XCTAssertEqual(model.status.folderAccess.map(\.state), [.unknown, .unknown, .unknown])
        XCTAssertFalse(model.status.folderAccessAsked)
        XCTAssertTrue(model.foldersToAsk)

        let round = try await model.requestFolderAccess()
        XCTAssertEqual(round, 1)
        XCTAssertGreaterThanOrEqual(model.status.folderAccessRound, round)
        // As setup waits: on the model's changes, not every so often.
        let waited = WaitEnded()
        Task {
            await model.wait { !model.listingFolders(in: round) }
            waited.ended = true
        }
        try await eventually("the folders asked about") { !model.listingFolders(in: round) }
        try await eventually("the wait to end") { waited.ended }
        XCTAssertTrue(model.status.folderAccessAsked)
        XCTAssertEqual(model.status.folderAccess.map(\.state), [.granted, .granted, .missing])
        XCTAssertFalse(model.foldersToAsk)

        // Remembered, and not listed again by a new service: Documents,
        // made since, stays missing until asked again.
        agent?.terminate()
        agent?.waitUntilExit()
        try await eventually("losing the connection") { model.link == .connecting }
        try FileManager.default.createDirectory(at: directory.appending(path: "Documents"), withIntermediateDirectories: true)
        try await setUpAgentAgain()
        try await eventually("a new connection") { model.link == .connected }
        XCTAssertTrue(model.status.folderAccessAsked)
        XCTAssertEqual(model.status.folderAccess.map(\.state), [.granted, .granted, .missing])
        let again = try await model.requestFolderAccess()
        try await eventually("the folders checked again") { !model.listingFolders(in: again) }
        XCTAssertEqual(model.status.folderAccess.map(\.state), [.granted, .granted, .granted])
    }

    private func setUpAgentAgain() async throws {
        let process = Process()
        process.executableURL = agent?.executableURL
        process.arguments = agent?.arguments
        process.environment = agent?.environment
        process.standardError = FileHandle.nullDevice
        try process.run()
        agent = process
    }
}

@MainActor
private final class WaitEnded {
    var ended = false
}
