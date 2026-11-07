import AppKit
import CoreServices
import Foundation
import SwiftProtobuf
import XCTest

final class FrameTests: XCTestCase {
    func testRoundTripsAndWaitsForWholeFrames() throws {
        var message = Inputweave_App_V1_ClientMessage()
        message.requestID = 7
        message.command = .getStatus(.init())
        let frame = try Frame.encode(message)
        XCTAssertEqual(Array(frame.prefix(4)), [0, 0, 0, UInt8(frame.count - 4)])

        var buffer = frame.prefix(frame.count - 1)
        XCTAssertNil(try Frame.decode(Inputweave_App_V1_ClientMessage.self, from: &buffer))
        buffer.append(frame.last!)
        buffer.append(frame)
        XCTAssertEqual(try Frame.decode(Inputweave_App_V1_ClientMessage.self, from: &buffer), message)
        XCTAssertEqual(try Frame.decode(Inputweave_App_V1_ClientMessage.self, from: &buffer), message)
        XCTAssertTrue(buffer.isEmpty)
    }

    func testRefusesOversizedFramesFromTheHeader() {
        var buffer = Data([0x7f, 0xff, 0xff, 0xff])
        XCTAssertThrowsError(try Frame.decode(Inputweave_App_V1_AgentMessage.self, from: &buffer))
    }
}

final class OSLabelTests: XCTestCase {
    func testNamesSystems() {
        XCTAssertEqual(OSLabel(os: "linux").name, "Linux")
        XCTAssertEqual(OSLabel(os: "windows").name, "Windows")
    }
}

final class CountdownTests: XCTestCase {
    func testFormatsMinutesAndSecondsAndStopsAtZero() {
        let now = Date(timeIntervalSince1970: 1000)
        XCTAssertEqual(countdown(until: 1_000_000 + 118_000, now: now), "1:58")
        XCTAssertEqual(countdown(until: 999_000, now: now), "0:00")
        XCTAssertEqual(groupedCode("382914"), "382 914")
    }
}

final class TransferTextTests: XCTestCase {
    private func transfer(
        _ state: Inputweave_App_V1_TransferState, id: String = "t", outgoing: Bool = true,
        reason: String = "", done: UInt64 = 0, rate: UInt64 = 0
    ) -> FileTransfer {
        var transfer = FileTransfer()
        transfer.transferID = id
        transfer.name = "Report.pdf"
        transfer.size = 100_000_000
        transfer.state = state
        transfer.outgoing = outgoing
        transfer.reason = reason
        transfer.doneBytes = done
        transfer.bytesPerSecond = rate
        return transfer
    }

    func testDescribesProgressAndWaiting() {
        let going = transfer(.transferring, done: 64_000_000, rate: 12_000_000)
        XCTAssertEqual(TransferText.title(going, device: "PC"), "Report.pdf → PC")
        XCTAssertEqual(TransferText.title(transfer(.done, outgoing: false), device: "Mac"), "Report.pdf ← Mac")
        XCTAssertEqual(TransferText.fraction(going), 0.64)
        let detail = TransferText.detail(going, device: "PC")
        XCTAssertTrue(detail.hasPrefix("64%"), detail)
        XCTAssertTrue(detail.contains("12 MB/s"), detail)

        XCTAssertNil(TransferText.fraction(transfer(.preparing)))
        // Cut off midway, the bar stays where the file will resume.
        XCTAssertEqual(TransferText.fraction(transfer(.waitingForConnection, done: 50_000_000)), 0.5)
        XCTAssertNil(TransferText.fraction(transfer(.waitingForConnection)))
        XCTAssertEqual(TransferText.detail(transfer(.waitingForConnection), device: "PC"), "Waiting for connection…")
    }

    func testQueuedFilesWaitToBeSentAndCanBeCancelled() {
        let queued = transfer(.queued)
        XCTAssertEqual(TransferText.detail(queued, device: "PC"), "Waiting to send…")
        XCTAssertNil(TransferText.fraction(queued))
        // Still going: listed in the panel, where it can be cancelled.
        XCTAssertFalse(queued.state.isFinished)
        XCTAssertFalse(TransferText.isProblem(queued))
        XCTAssertEqual(TransferText.panelRows([queued, transfer(.done, id: "d")], dismissed: []).map(\.transferID), ["t"])
    }

    func testExplainsDeclinesFromEitherSide() {
        XCTAssertEqual(TransferText.detail(transfer(.declined, reason: "rejected"), device: "PC"), "PC declined the file.")
        XCTAssertEqual(TransferText.detail(transfer(.declined, outgoing: false, reason: "rejected"), device: "Mac"), "Declined")
        XCTAssertFalse(TransferText.isProblem(transfer(.declined, outgoing: false, reason: "rejected")))
        XCTAssertTrue(TransferText.isProblem(transfer(.declined, reason: "no_space")))
        XCTAssertEqual(TransferText.detail(transfer(.failed, reason: "corrupt"), device: "PC"), "The file arrived damaged.")
    }

    func testPanelListsWhatIsGoingAndProblemsUntilDismissed() {
        let all = [transfer(.transferring, id: "a"), transfer(.done, id: "b"), transfer(.failed, id: "c", reason: "corrupt"), transfer(.cancelled, id: "d")]
        XCTAssertEqual(TransferText.panelRows(all, dismissed: []).map(\.transferID), ["a", "c"])
        XCTAssertEqual(TransferText.panelRows(all, dismissed: ["c"]).map(\.transferID), ["a"])
    }

    func testSummarizesReceivedFiles() {
        let one = TransferText.received([transfer(.done, outgoing: false)], device: "Mac")
        XCTAssertTrue(one.hasPrefix("Received Report.pdf ("), one)
        XCTAssertTrue(one.hasSuffix(") from Mac"), one)
        let two = TransferText.received([transfer(.done, id: "a"), transfer(.done, id: "b")], device: "Mac")
        XCTAssertTrue(two.hasPrefix("Received 2 files ("), two)
    }

    private func folder(
        _ state: Inputweave_App_V1_TransferState, id: String = "f", outgoing: Bool = true,
        files: UInt32 = 312, done: UInt32 = 0, failed: UInt32 = 0
    ) -> FileTransfer {
        var folder = transfer(state, id: id, outgoing: outgoing)
        folder.name = "Photos"
        folder.folder = true
        folder.size = 1_200_000_000
        folder.files = files
        folder.filesDone = done
        folder.filesFailed = failed
        return folder
    }

    func testAFolderRowSaysWhatItHoldsAndHowFarItGot() {
        XCTAssertEqual(TransferText.folderSummary(folder(.queued)), "312 files · 1.2 GB")
        XCTAssertEqual(TransferText.folderSummary(folder(.queued, files: 1)), "1 file · 1.2 GB")
        var going = folder(.transferring, done: 80)
        going.doneBytes = 300_000_000
        going.bytesPerSecond = 40_000_000
        XCTAssertEqual(TransferText.fraction(going), 0.25)
        let detail = TransferText.detail(going, device: "PC")
        XCTAssertTrue(detail.hasPrefix("80 of 312 files · 25% · 40 MB/s"), detail)
        XCTAssertEqual(TransferText.detail(folder(.queued), device: "PC"), "Waiting to send…")
        XCTAssertEqual(TransferText.detail(folder(.done, done: 312), device: "PC"), "Sent")
        XCTAssertFalse(TransferText.isProblem(folder(.done, done: 312)))
    }

    func testAFolderWithFailuresIsAProblemWithDetails() {
        let sent = folder(.done, done: 309, failed: 3)
        XCTAssertEqual(TransferText.detail(sent, device: "PC"), "309 sent, 3 failed")
        XCTAssertEqual(TransferText.detail(folder(.done, outgoing: false, done: 8, failed: 2), device: "Mac"), "8 received, 2 failed")
        // None arrived: failed, and still counted.
        XCTAssertEqual(TransferText.detail(folder(.failed, done: 0, failed: 312), device: "PC"), "0 sent, 312 failed")
        XCTAssertTrue(TransferText.isProblem(sent))
        XCTAssertEqual(TransferText.panelRows([sent], dismissed: []).map(\.transferID), ["f"])

        var listed = sent
        var failure = Inputweave_App_V1_FailedFile()
        failure.relativePath = "Photos/raw/IMG_0001.dng"
        failure.reason = "unreadable"
        listed.failures = [failure]
        XCTAssertEqual(TransferText.moreFailures(listed), 2)
    }

    func testExplainsTheNewReasons() {
        XCTAssertEqual(TransferText.detail(transfer(.failed, reason: "symlink"), device: "PC"), "This is a shortcut (symbolic link) and is not sent.")
        XCTAssertEqual(TransferText.detail(transfer(.failed, reason: "not_a_file"), device: "PC"), "This is not a file or a folder.")
        XCTAssertEqual(TransferText.detail(transfer(.declined, reason: "too_large"), device: "PC"), "The other computer does not take this much at once.")
        XCTAssertEqual(TransferText.detail(transfer(.declined, reason: "invalid_path"), device: "PC"), "The other computer cannot save it under this name.")
        XCTAssertEqual(TransferText.detail(transfer(.declined, outgoing: false, reason: "invalid_path"), device: "Mac"), "This computer cannot save it under this name.")
        XCTAssertEqual(TransferText.reason("unreadable", outgoing: true), "The file could not be read.")
        XCTAssertEqual(TransferText.reason("busy", outgoing: false), "Too many files at once.")
    }

    func testAsksOnceForTheWholeBatch() {
        var offered = transfer(.waitingForAnswer, outgoing: false)
        offered.batchItems = 3
        offered.batchFiles = 120
        offered.batchBytes = 3_000_000_000
        XCTAssertEqual(TransferText.detail(offered, device: "Mac"), "Mac wants to send you 3 items (120 files, 3 GB).")
        XCTAssertEqual(TransferText.offerTitle(offered, device: "Mac"), "Mac wants to send you files")
        XCTAssertEqual(TransferText.offerBody(offered), "3 items (120 files, 3 GB)")
        let lone = folder(.waitingForAnswer, outgoing: false)
        XCTAssertEqual(TransferText.offerBody(lone), "Photos (312 files, 1.2 GB)")
        XCTAssertEqual(TransferText.offerTitle(transfer(.waitingForAnswer, outgoing: false), device: "Mac"), "Mac wants to send you a file")
    }

    func testSummarizesReceivedFoldersAndWhatDidNotCome() {
        XCTAssertEqual(
            TransferText.received([folder(.done, outgoing: false, done: 312)], device: "Mac"),
            "Received 1 folder (312 files, 1.2 GB) from Mac")
        XCTAssertEqual(
            TransferText.received([folder(.done, outgoing: false, done: 309, failed: 3)], device: "Mac"),
            "Received 1 folder (309 files, 1.2 GB) from Mac, 3 files could not be sent")
        let mixed = TransferText.received(
            [folder(.done, outgoing: false, done: 10), transfer(.done, id: "a", outgoing: false)], device: "Mac", failed: 1)
        XCTAssertTrue(mixed.hasPrefix("Received 2 items (11 files, "), mixed)
        XCTAssertTrue(mixed.hasSuffix(") from Mac, 1 file could not be sent"), mixed)
        let folders = TransferText.received(
            [folder(.done, id: "1", outgoing: false, done: 2), folder(.done, id: "2", outgoing: false, done: 3)], device: "Mac")
        XCTAssertTrue(folders.hasPrefix("Received 2 folders (5 files, "), folders)
    }

    func testDismissedProblemsStayDismissedWhileListed() {
        let listed = [
            transfer(.failed, id: "old", reason: "corrupt"), transfer(.failed, id: "kept", reason: "corrupt"),
            transfer(.transferring, id: "going"), folder(.done, id: "partly", done: 8, failed: 2),
        ]
        // What was there when the app started is not news.
        XCTAssertEqual(TransferText.dismissed([], listed: listed, firstLook: true), ["old", "kept", "partly"])
        // Ids no longer listed are dropped; the rest stay.
        XCTAssertEqual(TransferText.dismissed(["kept", "gone"], listed: listed, firstLook: false), ["kept"])
        // A problem that happens later shows until dismissed.
        let later = listed + [transfer(.failed, id: "new", reason: "corrupt")]
        let dismissed = TransferText.dismissed(["old", "kept", "partly"], listed: later, firstLook: false)
        XCTAssertEqual(TransferText.panelRows(later, dismissed: dismissed).map(\.transferID), ["going", "new"])
    }
}

@MainActor
final class StatusIconStateTests: XCTestCase {
    private func model(
        link: AgentModel.Link = .connected, unreachableFor: TimeInterval = 0, sharing: Bool = true,
        missingPermission: Bool = false, active: String = "mac", controlledBy: String = ""
    ) -> AgentModel {
        var status = AgentStatus()
        status.thisDeviceID = "mac"
        status.sharing = sharing
        status.activeDeviceID = active
        status.controlledByDeviceID = controlledBy
        var permission = Inputweave_App_V1_PermissionStatus()
        permission.permission = .accessibility
        permission.granted = !missingPermission
        status.permissions = [permission]
        return AgentModel.sample(status: status, link: link, unreachableSince: .now - unreachableFor)
    }

    private func state(_ model: AgentModel, serviceEnabled: Bool = true) -> StatusIcon.State {
        StatusIcon.State(model, serviceEnabled: serviceEnabled, now: .now)
    }

    func testShowsWhatIsGoingOn() {
        XCTAssertEqual(state(model()), .normal)
        XCTAssertEqual(state(model(sharing: false)), .paused)
        XCTAssertEqual(state(model(active: "pc")), .controlling)
        XCTAssertEqual(state(model(controlledBy: "pc")), .controlled)
    }

    func testAsksForAttentionOnlyForWhatTheUserHasToFix() {
        XCTAssertEqual(state(model(), serviceEnabled: false), .attention)
        XCTAssertEqual(state(model(missingPermission: true)), .attention)
        XCTAssertEqual(state(model(link: .connecting, unreachableFor: 11)), .attention)
    }

    func testStaysQuietWhileTheServiceStartsOrUpdates() {
        // A fresh launch: no status yet, so not "paused" either.
        let starting = AgentModel.sample(status: AgentStatus(), link: .connecting)
        XCTAssertEqual(state(starting), .normal)
        XCTAssertEqual(state(model(link: .connecting, unreachableFor: 9)), .normal)
        XCTAssertEqual(state(model(link: .incompatible, unreachableFor: 60)), .normal)
        XCTAssertEqual(
            StatusIcon.State(model(link: .connecting), serviceEnabled: true, now: .now + 11), .attention)
    }
}

@MainActor
final class PanelTextTests: XCTestCase {
    private func model(
        sharing: Bool = true, locked: Bool = false, active: String = "mac", controlledBy: String = ""
    ) -> AgentModel {
        var status = AgentStatus()
        status.thisDeviceID = "mac"
        status.thisDeviceName = "MacBook Pro"
        status.sharing = sharing
        status.locked = locked
        status.activeDeviceID = active
        status.controlledByDeviceID = controlledBy
        var pc = DeviceStatus()
        pc.id = "pc"
        pc.name = "Windows Desktop"
        pc.state = .connected
        status.devices = [pc]
        return AgentModel.sample(status: status)
    }

    func testNamesTheComputerTheCursorIsLockedTo() {
        XCTAssertEqual(PanelText.title(model(locked: true)), "Locked to MacBook Pro")
        XCTAssertEqual(PanelText.subtitle(model(locked: true)), "Press ⌃⌥⇧L to unlock.")
        XCTAssertEqual(PanelText.title(model(locked: true, active: "pc")), "Locked to Windows Desktop")
        XCTAssertEqual(PanelText.subtitle(model(locked: true, active: "pc")), "Press ⌃⌥⇧L to unlock.")
        // Before the service names the active device, the cursor is here.
        XCTAssertEqual(PanelText.title(model(locked: true, active: "")), "Locked to MacBook Pro")
    }

    func testPausingAndBeingControlledComeFirst() {
        XCTAssertEqual(PanelText.title(model(sharing: false, locked: true)), "Sharing paused")
        XCTAssertNil(PanelText.subtitle(model(sharing: false, locked: true)))
        XCTAssertEqual(PanelText.title(model(locked: true, controlledBy: "pc")), "Controlled by Windows Desktop")
        XCTAssertEqual(
            PanelText.subtitle(model(locked: true, controlledBy: "pc")), "Move this computer's mouse to take it back.")
    }

    func testRelativeModeShowsInThePanel() {
        var status = AgentStatus()
        status.thisDeviceID = "mac"
        status.thisDeviceName = "MacBook Pro"
        status.sharing = true
        status.activeDeviceID = "pc"
        status.relativeMouse = "pc"
        status.relativeSupported = true
        status.hotkeys = ShortcutText.defaults
        var pc = DeviceStatus()
        pc.id = "pc"
        pc.name = "Windows Desktop"
        pc.state = .connected
        status.devices = [pc]
        XCTAssertEqual(PanelText.title(AgentModel.sample(status: status)), "Relative mouse on Windows Desktop")
        XCTAssertEqual(PanelText.subtitle(AgentModel.sample(status: status)), "Press ⌃⌥⇧G to end relative mouse.")
        // Locked too, relative mode says more.
        status.locked = true
        XCTAssertEqual(PanelText.title(AgentModel.sample(status: status)), "Relative mouse on Windows Desktop")
        status.hotkeys.relative = ""
        XCTAssertEqual(PanelText.subtitle(AgentModel.sample(status: status)), "Press ⌃⌥⇧⎋ to take back control.")
    }

    func testUnlockedTextIsUnchanged() {
        XCTAssertEqual(PanelText.title(model()), "Sharing keyboard and mouse")
        XCTAssertEqual(PanelText.subtitle(model()), "Cursor on MacBook Pro")
        XCTAssertEqual(PanelText.title(model(active: "pc")), "Controlling Windows Desktop")
        XCTAssertEqual(PanelText.subtitle(model(active: "pc")), "Press ⌃⌥⇧⎋ to take back control.")
    }
}

@MainActor
final class RelativeTextTests: XCTestCase {
    private typealias State = RelativeText.State
    private typealias Note = RelativeText.Note

    func testSaysWhenRelativeModeStartsAndEnds() {
        let off = State()
        let manual = State(device: "pc")
        let automatic = State(device: "pc", automatic: true)
        let change = RelativeText.change
        XCTAssertEqual(change(off, manual), .entered(device: "pc", automatic: false))
        XCTAssertEqual(change(off, automatic), .entered(device: "pc", automatic: true))
        // Why it ended is the background service's to say.
        XCTAssertEqual(
            change(automatic, State(ends: 1, endDevice: "pc", endReason: .released)),
            .ended(device: "pc", reason: .released))
        XCTAssertEqual(
            change(manual, State(ends: 1, endDevice: "pc", endReason: .turnedOff)),
            .ended(device: "pc", reason: .turnedOff))
        // Ended and began again in between: it began.
        XCTAssertEqual(
            change(automatic, State(device: "pc", automatic: true, ends: 1, endDevice: "pc", endReason: .released)),
            .entered(device: "pc", automatic: true))
        // Jumped from one computer to another that holds the pointer.
        XCTAssertEqual(
            change(manual, State(device: "laptop", automatic: true, ends: 1, endDevice: "pc", endReason: .jumped)),
            .entered(device: "laptop", automatic: true))
        XCTAssertNil(change(off, off))
        XCTAssertNil(change(manual, manual))
        XCTAssertNil(change(manual, automatic))
        // Fewer ends than before: a restarted service, only remembered.
        XCTAssertNil(change(State(ends: 5, endDevice: "pc", endReason: .turnedOff), State(device: "pc")))
        XCTAssertNil(change(State(device: "pc", ends: 5), State(ends: 1, endDevice: "pc", endReason: .turnedOff)))
    }

    func testTellsWhatTheUserDidAndWhatAGameDid() {
        let told = { (reason: RelativeText.EndReason) in Note.ended(device: "pc", reason: reason).told }
        XCTAssertTrue(told(.turnedOff))
        XCTAssertTrue(told(.autoOff))
        XCTAssertTrue(told(.jumped))
        XCTAssertTrue(told(.released))
        // Why the keyboard and mouse came back says it.
        XCTAssertFalse(told(.controlReturned))
        XCTAssertFalse(told(.deviceLost))
        XCTAssertFalse(told(.sharingPaused))
        XCTAssertTrue(Note.entered(device: "pc", automatic: false).told)
        XCTAssertTrue(Note.ended(device: "pc", reason: .released).automatic)
        XCTAssertFalse(Note.ended(device: "pc", reason: .turnedOff).automatic)
    }

    func testWhatAGameDoesIsToldNowAndThen() {
        // For each computer the first of each kind,
        // then at most one a minute; what the user does, always.
        var quieting = RelativeText.Quieting()
        let start = Date(timeIntervalSince1970: 1_000_000)
        let entered = Note.entered(device: "pc", automatic: true)
        let released = Note.ended(device: "pc", reason: .released)
        XCTAssertTrue(quieting.allows(entered, at: start))
        XCTAssertTrue(quieting.allows(released, at: start + 5))
        XCTAssertFalse(quieting.allows(entered, at: start + 10))
        XCTAssertFalse(quieting.allows(released, at: start + 20))
        XCTAssertTrue(quieting.allows(Note.entered(device: "pc", automatic: false), at: start + 21))
        XCTAssertTrue(quieting.allows(Note.ended(device: "pc", reason: .turnedOff), at: start + 22))
        // Another computer has its own.
        XCTAssertTrue(quieting.allows(Note.entered(device: "laptop", automatic: true), at: start + 23))
        XCTAssertFalse(quieting.allows(entered, at: start + 64))
        XCTAssertTrue(quieting.allows(entered, at: start + 65))
        XCTAssertFalse(quieting.allows(released, at: start + 100))
        XCTAssertTrue(quieting.allows(released, at: start + 125))
    }

    func testOnlyChangesAfterConnectingAreNews() {
        var news = RelativeText.News()
        let now = Date()
        let on = State(device: "pc", automatic: true, ends: 4)
        // The first status is only remembered.
        XCTAssertNil(news.update(on, at: now))
        let ended = State(ends: 5, endDevice: "pc", endReason: .turnedOff)
        XCTAssertEqual(news.update(ended, at: now), .ended(device: "pc", reason: .turnedOff))
        // Taken back: the keyboard and mouse coming back says why.
        XCTAssertNil(news.update(State(ends: 6, endDevice: "pc", endReason: .controlReturned), at: now))
        // The service restarts, and counts from 0 again: nothing to tell.
        XCTAssertNil(news.update(nil, at: now))
        XCTAssertNil(news.update(State(device: "pc", automatic: true), at: now))
        XCTAssertEqual(
            news.update(State(ends: 1, endDevice: "pc", endReason: .turnedOff), at: now),
            .ended(device: "pc", reason: .turnedOff))
    }

    func testWhatIsNotShownIsNotCountedAsTold() {
        var news = RelativeText.News()
        let now = Date()
        XCTAssertNil(news.update(State(), at: now))
        // Something else is shown for this status: not told...
        XCTAssertNil(news.update(State(device: "pc", automatic: true), at: now, preempted: true))
        XCTAssertNil(news.update(State(ends: 1, endDevice: "pc", endReason: .released), at: now + 1, preempted: true))
        // ...so the first of each kind is still to come.
        XCTAssertEqual(
            news.update(State(device: "pc", automatic: true, ends: 1), at: now + 2),
            .entered(device: "pc", automatic: true))
        XCTAssertEqual(
            news.update(State(ends: 2, endDevice: "pc", endReason: .released), at: now + 3),
            .ended(device: "pc", reason: .released))
        XCTAssertNil(news.update(State(device: "pc", automatic: true, ends: 2), at: now + 4))
    }

    func testWordsForTheHUD() {
        var status = AgentStatus()
        status.thisDeviceID = "mac"
        status.thisDeviceName = "MacBook Pro"
        status.relativeSupported = true
        status.hotkeys = ShortcutText.defaults
        var pc = DeviceStatus()
        pc.id = "pc"
        pc.name = "Windows Desktop"
        status.devices = [pc]
        let model = AgentModel.sample(status: status)
        XCTAssertEqual(
            RelativeText.message(.entered(device: "pc", automatic: false), model: model),
            "Relative mouse: the cursor stays on “Windows Desktop”. Press ⌃⌥⇧G to end it.")
        XCTAssertEqual(
            RelativeText.message(.ended(device: "pc", reason: .turnedOff), model: model), "Relative mouse ended.")
        XCTAssertEqual(
            RelativeText.message(.ended(device: "pc", reason: .released), model: model),
            "“Windows Desktop” let go of the mouse, so it moves as usual again.")
        XCTAssertEqual(
            RelativeText.unsupported("Windows Desktop"),
            "InputWeave on “Windows Desktop” doesn't support relative mouse.")
        // With its hotkey off, the take-back hotkey ends it.
        status.hotkeys.relative = ""
        XCTAssertEqual(
            RelativeText.message(.entered(device: "pc", automatic: true), model: AgentModel.sample(status: status)),
            "Relative mouse: the cursor stays on “Windows Desktop”. Press ⌃⌥⇧⎋ to take back control.")
    }
}

/// What the HUD says, apart from showing it.
@MainActor
final class HUDControllerTests: XCTestCase {
    private var defaults: UserDefaults!
    private var suite = ""

    override func setUp() async throws {
        suite = "HUDControllerTests-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
    }

    override func tearDown() async throws {
        defaults.removePersistentDomain(forName: suite)
    }

    private func model(
        active: String = "mac", relative: String = "", automatic: Bool = false, ends: UInt64 = 0,
        endReason: RelativeText.EndReason = .unspecified, refusals: UInt64 = 0
    ) -> AgentModel {
        var status = AgentStatus()
        status.thisDeviceID = "mac"
        status.thisDeviceName = "MacBook Pro"
        status.sharing = true
        status.activeDeviceID = active
        status.relativeSupported = true
        status.hotkeys = ShortcutText.defaults
        status.relativeMouse = relative
        status.relativeMouseAutomatic = automatic
        status.relativeEnds = ends
        status.lastRelativeEnd.deviceID = ends == 0 ? "" : "pc"
        status.lastRelativeEnd.reason = endReason
        var pc = DeviceStatus()
        pc.id = "pc"
        pc.name = "Windows Desktop"
        pc.state = .connected
        status.devices = [pc]
        let refusal = (0..<refusals).reduce(nil as RelativeRefusal?) { previous, _ in .after(previous, deviceID: "pc") }
        return AgentModel.sample(status: status, relativeRefusal: refusal)
    }

    func testTheFirstCrossingIntoRelativeModeSaysHowToTakeBackControlToo() {
        let hud = HUDController(model: model(), defaults: defaults)
        let now = Date()
        XCTAssertNil(hud.message(for: model(), at: now))
        XCTAssertEqual(
            hud.message(for: model(active: "pc", relative: "pc", automatic: true), at: now),
            "Press ⌃⌥⇧⎋ to take back control at any time.\nRelative mouse: the cursor stays on “Windows Desktop”. Press ⌃⌥⇧G to end it.")
        XCTAssertEqual(defaults.integer(forKey: Preferences.takeBackHints), 1)
        // Later, only relative mode.
        XCTAssertEqual(
            hud.message(for: model(active: "pc", ends: 1, endReason: .turnedOff), at: now),
            "Relative mouse ended.")
        XCTAssertEqual(
            hud.message(for: model(active: "pc", relative: "pc", ends: 1), at: now),
            "Relative mouse: the cursor stays on “Windows Desktop”. Press ⌃⌥⇧G to end it.")
    }

    func testSaysWhyTheKeyboardAndMouseCameBack() {
        func returned(_ reason: Inputweave_App_V1_ReturnReason, count: UInt64) -> AgentModel {
            let base = model()
            var status = base.status
            status.controlReturns = count
            status.lastReturn = reason
            status.lastReturnDeviceID = "pc"
            return AgentModel.sample(status: status)
        }
        let hud = HUDController(model: model(), defaults: defaults)
        let now = Date()
        XCTAssertNil(hud.message(for: returned(.unspecified, count: 0), at: now))
        XCTAssertEqual(
            hud.message(for: returned(.inputUnavailable, count: 1), at: now),
            "“Windows Desktop” can't take input from here right now, so the keyboard and mouse are back on this computer.")
        XCTAssertEqual(
            hud.message(for: returned(.peerPaused, count: 2), at: now),
            "“Windows Desktop” paused sharing, so the keyboard and mouse are back on this computer.")
        XCTAssertEqual(
            hud.message(for: returned(.captureEnded, count: 3), at: now),
            "The desktop took the keyboard and mouse back to this computer.")
    }

    func testTellsOfAComputerBeingWokenAndWakingUp() {
        func mini(
            _ wake: WakeState, connected: Bool = false, supported: Bool = true, link: AgentModel.Link = .connected
        ) -> AgentModel {
            var status = model().status
            status.wakeSupported = supported
            var mini = DeviceStatus()
            mini.id = "mini"
            mini.name = "Mac mini"
            mini.state = connected ? .connected : .offline
            mini.canWake = !connected
            mini.wake = wake
            status.devices.append(mini)
            return AgentModel.sample(status: status, link: link)
        }
        let hud = HUDController(model: model(), defaults: defaults)
        let now = Date()
        // Being woken as the app connects: only remembered.
        XCTAssertNil(hud.message(for: mini(.waking), at: now))
        XCTAssertEqual(hud.message(for: mini(WakeState.none, connected: true), at: now), "Mac mini is awake.")
        XCTAssertNil(hud.message(for: mini(WakeState.none), at: now))
        XCTAssertEqual(hud.message(for: mini(.waking), at: now), "Waking Mac mini…")
        XCTAssertNil(hud.message(for: mini(.notWoken), at: now))
        // The service out of reach and back: its first status is only
        // remembered again.
        XCTAssertNil(hud.message(for: mini(WakeState.none, link: .connecting), at: now))
        XCTAssertNil(hud.message(for: mini(.waking), at: now))
        XCTAssertEqual(hud.message(for: mini(WakeState.none, connected: true), at: now), "Mac mini is awake.")
        // Locked at the same moment: that is told instead.
        var locked = mini(.waking).status
        locked.locked = true
        XCTAssertEqual(
            hud.message(for: AgentModel.sample(status: locked), at: now), "Locked to this computer. Press ⌃⌥⇧L to unlock.")
        // A service from before waking tells nothing of it.
        let older = HUDController(model: model(), defaults: defaults)
        XCTAssertNil(older.message(for: mini(WakeState.none, supported: false), at: now))
        XCTAssertNil(older.message(for: mini(.waking, supported: false), at: now))
    }

    func testARefusalShownFirstLeavesWhatAGameDidUntold() {
        let hud = HUDController(model: model(), defaults: defaults)
        defaults.set(3, forKey: Preferences.takeBackHints)
        let now = Date()
        XCTAssertNil(hud.message(for: model(active: "pc"), at: now))
        // The game takes the pointer as the hotkey is refused elsewhere.
        XCTAssertEqual(
            hud.message(for: model(active: "pc", relative: "pc", automatic: true, refusals: 1), at: now),
            "InputWeave on “Windows Desktop” doesn't support relative mouse.")
        XCTAssertEqual(
            hud.message(for: model(active: "pc", ends: 1, endReason: .released, refusals: 1), at: now + 1),
            "“Windows Desktop” let go of the mouse, so it moves as usual again.")
        // The game taking it was never told, so its first time is now.
        XCTAssertEqual(
            hud.message(for: model(active: "pc", relative: "pc", automatic: true, ends: 1, refusals: 1), at: now + 2),
            "Relative mouse: the cursor stays on “Windows Desktop”. Press ⌃⌥⇧G to end it.")
    }
}

@MainActor
final class AgentModelStateTests: XCTestCase {
    private func model(
        link: AgentModel.Link = .connected, sharing: Bool = true, locked: Bool = false, active: String = "mac",
        dwellMs: UInt32 = 0, pcScreenLocked: Bool = false, pcLockScreenInput: Bool = false,
        pcInputUnavailable: Bool = false, pcSharingPaused: Bool = false
    ) -> AgentModel {
        var status = AgentStatus()
        status.thisDeviceID = "mac"
        status.sharing = sharing
        status.locked = locked
        status.activeDeviceID = active
        status.edgeDwellMs = dwellMs
        status.defaultDwellMs = 150
        var pc = DeviceStatus()
        pc.id = "pc"
        pc.state = .connected
        pc.screenLocked = pcScreenLocked
        pc.lockScreenInput = pcLockScreenInput
        pc.inputUnavailable = pcInputUnavailable
        pc.sharingPaused = pcSharingPaused
        var laptop = DeviceStatus()
        laptop.id = "laptop"
        laptop.state = .offline
        status.devices = [pc, laptop]
        return AgentModel.sample(status: status, link: link)
    }

    func testTheCursorIsHereUntilTheServiceSaysOtherwise() {
        XCTAssertEqual(model(active: "").cursorDeviceID, "mac")
        XCTAssertEqual(model(active: "pc").cursorDeviceID, "pc")
    }

    func testOffersToMoveTheCursorOnlyWhereItCanGo() {
        XCTAssertTrue(model().canJump(to: "pc"))
        XCTAssertTrue(model(active: "pc").canJump(to: "mac"))
        // Not where it already is, which would only warp the pointer.
        XCTAssertFalse(model().canJump(to: "mac"))
        XCTAssertFalse(model(active: "").canJump(to: "mac"))
        XCTAssertFalse(model(active: "pc").canJump(to: "pc"))
        XCTAssertFalse(model().canJump(to: "laptop"))
        XCTAssertFalse(model().canJump(to: "ghost"))
        XCTAssertFalse(model(sharing: false).canJump(to: "pc"))
        XCTAssertFalse(model(locked: true).canJump(to: "pc"))
        XCTAssertFalse(model(link: .connecting).canJump(to: "pc"))
        // Not onto a locked screen input cannot reach; a locked one that
        // takes input is fine.
        XCTAssertFalse(model(pcScreenLocked: true).canJump(to: "pc"))
        XCTAssertTrue(model(pcScreenLocked: true).outOfReach("pc"))
        XCTAssertTrue(model(pcScreenLocked: true, pcLockScreenInput: true).canJump(to: "pc"))
        XCTAssertFalse(model(pcLockScreenInput: false).outOfReach("pc"))
        // Not onto one that takes no input now, locked or not.
        XCTAssertFalse(model(pcInputUnavailable: true).canJump(to: "pc"))
        XCTAssertTrue(model(pcInputUnavailable: true).outOfReach("pc"))
        // Nor onto one whose user paused sharing.
        XCTAssertFalse(model(pcSharingPaused: true).canJump(to: "pc"))
        XCTAssertTrue(model(pcSharingPaused: true).outOfReach("pc"))
    }

    func testTurnsTheSwitchDelayOnWithTheLastOneInEffect() {
        XCTAssertEqual(model().switchDelayMs, 150)
        XCTAssertEqual(model(dwellMs: 400).switchDelayMs, 400)
    }
}

final class LaunchRevealTests: XCTestCase {
    func testOpeningTheAppBringsBackAHiddenIcon() {
        XCTAssertEqual(
            LaunchReveal.decide(launch: .opened, iconShown: false, finishedSetup: true),
            LaunchReveal(showsIcon: true, showsMainWindow: true))
        // Setup opens its own window.
        XCTAssertEqual(
            LaunchReveal.decide(launch: .opened, iconShown: false, finishedSetup: false),
            LaunchReveal(showsIcon: true, showsMainWindow: false))
        XCTAssertEqual(LaunchReveal.decide(launch: .opened, iconShown: true, finishedSetup: true), LaunchReveal())
        XCTAssertEqual(LaunchReveal.decide(launch: .atLogin, iconShown: false, finishedSetup: true), LaunchReveal())
        // Opened for Send with InputWeave: only its panel.
        XCTAssertEqual(LaunchReveal.decide(launch: .forService, iconShown: false, finishedSetup: true), LaunchReveal())
    }

    private func event(_ id: AEEventID, launch: OSType?) -> NSAppleEventDescriptor {
        let event = NSAppleEventDescriptor(
            eventClass: kCoreEventClass, eventID: id, targetDescriptor: nil,
            returnID: AEReturnID(kAutoGenerateReturnID), transactionID: AETransactionID(kAnyTransactionID))
        if let launch {
            event.setParam(NSAppleEventDescriptor(enumCode: launch), forKeyword: keyAEPropData)
        }
        return event
    }

    func testTellsHowTheAppWasLaunched() {
        XCTAssertEqual(LaunchReveal.launch(event(kAEOpenApplication, launch: keyAELaunchedAsLogInItem), isDefault: true), .atLogin)
        // Opened from the Finder, the Dock or `open`.
        XCTAssertEqual(LaunchReveal.launch(event(kAEOpenApplication, launch: nil), isDefault: true), .opened)
        XCTAssertEqual(LaunchReveal.launch(nil, isDefault: true), .opened)
        XCTAssertEqual(LaunchReveal.launch(event(kAEReopenApplication, launch: keyAELaunchedAsLogInItem), isDefault: true), .opened)
        // For a service: as the event says, or as the launch is not the default one.
        XCTAssertEqual(LaunchReveal.launch(event(kAEOpenApplication, launch: keyAELaunchedAsServiceItem), isDefault: true), .forService)
        XCTAssertEqual(LaunchReveal.launch(event(kAEOpenApplication, launch: nil), isDefault: false), .forService)
        XCTAssertEqual(LaunchReveal.launch(nil, isDefault: false), .forService)
        XCTAssertEqual(
            LaunchReveal.launch(event(kAEOpenApplication, launch: keyAELaunchedAsLogInItem), isDefault: false), .atLogin)
    }

    func testTellsMacOSAboutTheServicesOncePerBuild() {
        XCTAssertTrue(ServicesRefresh.needed(recorded: nil, build: "10100"), "the first run")
        XCTAssertTrue(ServicesRefresh.needed(recorded: "10001", build: "10100"), "a new build")
        XCTAssertFalse(ServicesRefresh.needed(recorded: "10100", build: "10100"))
    }
}

final class EarlierPreferencesTests: XCTestCase {
    func testCopiesTheSettingsInUseThatAreNotSetYet() {
        let earlier: [String: Any] = [
            Preferences.finishedSetup: true,
            Preferences.dismissedTransfers: ["a", "b"],
            Preferences.showsMenuBarIcon: false,
            "SUEnableAutomaticChecks": false,
            "NSWindow Frame Main": "0 0 100 100",
        ]
        let copied = EarlierPreferences.toCopy(earlier: earlier, isSet: { $0 == Preferences.showsMenuBarIcon })
        // Setup runs again, for the permissions the new service needs.
        XCTAssertEqual(Set(copied.keys), [Preferences.dismissedTransfers, "SUEnableAutomaticChecks"])
        XCTAssertEqual(copied[Preferences.dismissedTransfers] as? [String], ["a", "b"])
    }

    func testCopiesOnceIntoTheNewDomainAndLeavesTheOldOne() throws {
        let name = "cc.ctsite.inputweave.tests.\(UUID().uuidString)"
        let earlierName = "\(name).earlier"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: name))
        defer {
            defaults.removePersistentDomain(forName: name)
            defaults.removePersistentDomain(forName: earlierName)
        }
        defaults.setPersistentDomain(
            [Preferences.finishedSetup: true, Preferences.takeBackHints: 2, Preferences.showsMenuBarIcon: false],
            forName: earlierName)

        EarlierPreferences.copy(into: defaults, from: earlierName)
        XCTAssertEqual(defaults.integer(forKey: Preferences.takeBackHints), 2)
        XCTAssertEqual(defaults.object(forKey: Preferences.showsMenuBarIcon) as? Bool, false)
        XCTAssertNil(defaults.object(forKey: Preferences.finishedSetup))
        XCTAssertNotNil(defaults.persistentDomain(forName: earlierName))

        // Once copied, never again: a later change in the old domain stays
        // there.
        defaults.set(0, forKey: Preferences.takeBackHints)
        defaults.setPersistentDomain([Preferences.takeBackHints: 3], forName: earlierName)
        EarlierPreferences.copy(into: defaults, from: earlierName)
        XCTAssertEqual(defaults.integer(forKey: Preferences.takeBackHints), 0)
    }
}

final class OutdatedServiceTests: XCTestCase {
    private let bundled = String(repeating: "ab", count: 32)
    private let other = String(repeating: "cd", count: 32)

    private func outdated(
        _ link: AgentModel.Link = .connected, version: String = "0.1.0", digest: String, bundled: String? = nil
    ) -> Bool {
        OutdatedService.isOutdated(
            link: link, agentVersion: version, appVersion: "0.1.0",
            agentDigest: digest, bundledDigest: bundled ?? self.bundled)
    }

    func testAnotherBuildOfTheSameVersionIsOutdated() {
        XCTAssertTrue(outdated(digest: other))
        XCTAssertFalse(outdated(digest: bundled))
        // shasum and the agent both print lowercase; case alone is no difference.
        XCTAssertFalse(outdated(digest: bundled.uppercased()))
    }

    func testAnotherVersionOrIPCVersionIsOutdated() {
        XCTAssertTrue(outdated(version: "0.0.9", digest: bundled))
        XCTAssertTrue(outdated(version: "0.0.9", digest: ""))
        XCTAssertTrue(outdated(.incompatible, version: "", digest: ""))
    }

    func testAServiceOrAppWithoutADigestIsJudgedByVersion() {
        // A service from before agents reported their digest.
        XCTAssertFalse(outdated(digest: ""))
        // An app whose plist lacks the digest.
        XCTAssertFalse(outdated(digest: other, bundled: ""))
        // A service that did not report its version.
        XCTAssertFalse(outdated(version: "", digest: bundled))
        XCTAssertTrue(outdated(version: "", digest: other))
    }

    func testNothingIsDecidedBeforeTheServiceAnswers() {
        XCTAssertFalse(outdated(.connecting, version: "0.0.9", digest: other))
    }

    func testReadsTheDigestFromTheLaunchAgentPlist() throws {
        let directory = FileManager.default.temporaryDirectory.appending(path: "iw-plist-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let plist = directory.appending(path: "agent.plist")
        let contents: NSDictionary = ["Label": "cc.ctsite.inputweave.agent", OutdatedService.digestKey: bundled]
        try contents.write(to: plist)
        XCTAssertEqual(OutdatedService.bundledDigest(plist: plist), bundled)

        try (["Label": "cc.ctsite.inputweave.agent"] as NSDictionary).write(to: plist)
        XCTAssertEqual(OutdatedService.bundledDigest(plist: plist), "")
        XCTAssertEqual(OutdatedService.bundledDigest(plist: directory.appending(path: "missing.plist")), "")
    }
}

final class ConnectionTextTests: XCTestCase {
    func testSaysWhenItsUserPausedSharing() {
        XCTAssertEqual(ConnectionText.connected(rttMs: 0, sharingPaused: true), "Paused sharing")
        XCTAssertEqual(ConnectionText.connected(rttMs: 12, sharingPaused: true), "Paused sharing · 12 ms")
        // Before the other reasons the cursor stays off it.
        XCTAssertEqual(
            ConnectionText.connected(rttMs: 12, screenLocked: true, inputUnavailable: true, sharingPaused: true),
            "Paused sharing · 12 ms")
    }

    func testTheSymbolFollowsTheSamePriority() {
        XCTAssertEqual(ConnectionText.symbol(screenLocked: false, sharingPaused: false), "circle.fill")
        XCTAssertEqual(ConnectionText.symbol(screenLocked: true, sharingPaused: false), "lock.fill")
        XCTAssertEqual(ConnectionText.symbol(screenLocked: false, sharingPaused: true), "pause.circle")
        XCTAssertEqual(ConnectionText.symbol(screenLocked: true, sharingPaused: true), "pause.circle")
    }

    func testShowsTheRoundTripOnceMeasured() {
        XCTAssertEqual(ConnectionText.connected(rttMs: 0), "Connected")
        XCTAssertEqual(ConnectionText.connected(rttMs: 12), "Connected · 12 ms")
        XCTAssertEqual(ConnectionText.connected(rttMs: 1), "Connected · 1 ms")
        XCTAssertEqual(ConnectionText.connected(rttMs: 999), "Connected · 999 ms")
    }

    func testSaysWhenTheScreenIsLocked() {
        XCTAssertEqual(ConnectionText.connected(rttMs: 0, screenLocked: true), "Screen locked")
        XCTAssertEqual(ConnectionText.connected(rttMs: 12, screenLocked: true), "Screen locked · 12 ms")
        XCTAssertEqual(ConnectionText.connected(rttMs: 12, screenLocked: false), "Connected · 12 ms")
        // Input from here cannot reach it while it is locked.
        XCTAssertEqual(
            ConnectionText.connected(rttMs: 0, screenLocked: true, lockScreenInput: false),
            "Screen locked, can't take input")
        XCTAssertEqual(
            ConnectionText.connected(rttMs: 12, screenLocked: true, lockScreenInput: false),
            "Screen locked, can't take input · 12 ms")
        XCTAssertEqual(
            ConnectionText.connected(rttMs: 12, screenLocked: false, lockScreenInput: false),
            "Connected · 12 ms")
    }

    func testSaysWhenNoInputReachesIt() {
        XCTAssertEqual(ConnectionText.connected(rttMs: 0, inputUnavailable: true), "Can't take input")
        XCTAssertEqual(ConnectionText.connected(rttMs: 12, inputUnavailable: true), "Can't take input · 12 ms")
        // Locked too, it says both.
        XCTAssertEqual(
            ConnectionText.connected(rttMs: 12, screenLocked: true, lockScreenInput: true, inputUnavailable: true),
            "Screen locked, can't take input · 12 ms")
    }
}

final class LoginWindowTextTests: XCTestCase {
    /// Each refusal says why, and anything else that the setting did not
    /// change.
    func testEachRefusalHasItsReason() {
        let codes = ["not_installed", "custom_paths", "no_lock_screen_input", "unsupported"]
        let texts = codes.map(LoginWindowText.refusal)
        XCTAssertEqual(Set(texts).count, codes.count)
        let fallback = LoginWindowText.refusal("timeout")
        XCTAssertEqual(fallback, LoginWindowText.refusal(""))
        XCTAssertFalse(texts.contains(fallback))
        XCTAssertTrue(LoginWindowText.refusal("not_installed").contains("installer package"))
    }
}

@MainActor
final class FolderAccessTests: XCTestCase {
    private func model(_ states: [Inputweave_App_V1_FolderAccessState], asked: Bool) -> AgentModel {
        var status = AgentStatus()
        status.folderAccess = zip([Inputweave_App_V1_UserFolder.downloads, .desktop, .documents], states).map { folder, state in
            var access = Inputweave_App_V1_FolderAccess()
            access.folder = folder
            access.state = state
            return access
        }
        status.folderAccessAsked = asked
        return AgentModel.sample(status: status)
    }

    func testSaysWhatMacOSSaidOfEachFolder() {
        XCTAssertEqual(FolderAccessText.state(.granted).symbol, "checkmark.circle.fill")
        XCTAssertEqual(FolderAccessText.state(.denied).symbol, "xmark.circle.fill")
        XCTAssertEqual(FolderAccessText.state(.asking).symbol, "clock")
        XCTAssertEqual(FolderAccessText.state(.unknown).text, String(localized: "Not asked yet"))
        XCTAssertEqual(FolderAccessText.state(.missing).text, String(localized: "Not on this Mac"))
        XCTAssertEqual(FolderAccessText.color(.granted), .systemGreen)
        XCTAssertEqual(FolderAccessText.color(.denied), .systemRed)
        XCTAssertEqual(FolderAccessText.name(.downloads), String(localized: "Downloads"))
    }

    func testFollowsTheRoundItStarted() {
        var status = AgentStatus()
        var access = Inputweave_App_V1_FolderAccess()
        access.folder = .downloads
        access.state = .asking
        status.folderAccess = [access]
        status.folderAccessRound = 2
        XCTAssertTrue(AgentModel.sample(status: status).listingFolders(in: 2))
        // Another round began, or the service started over.
        XCTAssertFalse(AgentModel.sample(status: status).listingFolders(in: 1))
        status.folderAccessRound = 0
        XCTAssertFalse(AgentModel.sample(status: status).listingFolders(in: 2))
        status.folderAccessRound = 2
        status.folderAccess[0].state = .granted
        XCTAssertFalse(AgentModel.sample(status: status).listingFolders(in: 2))
    }

    /// Setup and the notifications ask macOS in one place, once, and read
    /// the answer again whenever the app becomes active.
    func testNotificationPermissionAsksOnceAndRereadsOnActivation() async throws {
        let system = FakeNotificationSettings()
        let permission = NotificationPermission(
            read: { system.state },
            ask: {
                system.asked += 1
                system.state = .denied
            })
        var allowed = await permission.allowed()
        XCTAssertFalse(allowed)
        XCTAssertEqual(system.asked, 1)
        allowed = await permission.allowed()
        XCTAssertFalse(allowed)
        XCTAssertEqual(system.asked, 1)
        XCTAssertEqual(permission.state, .denied)

        // Turned on in System Settings, then back to the app.
        system.state = .allowed
        NotificationCenter.default.post(name: NSApplication.didBecomeActiveNotification, object: nil)
        let deadline = Date.now.addingTimeInterval(5)
        while permission.state != .allowed && Date.now < deadline {
            try await Task.sleep(for: .milliseconds(20))
        }
        XCTAssertEqual(permission.state, .allowed)
        allowed = await permission.allowed()
        XCTAssertTrue(allowed)
        XCTAssertEqual(system.asked, 1)
    }

    func testAsksUntilEveryFolderWasAskedAbout() {
        // A service before folder access, or one with none to ask for.
        XCTAssertFalse(model([], asked: false).foldersToAsk)
        XCTAssertFalse(model([], asked: false).asksForFolders)
        XCTAssertTrue(model([.unknown, .unknown, .unknown], asked: false).foldersToAsk)
        // Asked on this Mac before, not yet listed since the service started.
        XCTAssertTrue(model([.unknown, .unknown, .unknown], asked: true).foldersToAsk)
        let asking = model([.granted, .asking, .asking], asked: false)
        XCTAssertTrue(asking.askingForFolders)
        XCTAssertTrue(asking.foldersToAsk)
        let answered = model([.granted, .denied, .missing], asked: true)
        XCTAssertFalse(answered.foldersToAsk)
        XCTAssertFalse(answered.askingForFolders)
    }
}

/// What macOS would say of notifications, for `NotificationPermission`.
@MainActor
private final class FakeNotificationSettings {
    var state = NotificationPermission.State.notAsked
    var asked = 0
}
