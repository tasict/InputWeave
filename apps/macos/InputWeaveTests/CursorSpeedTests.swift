import XCTest

/// The inspector's cursor speed row: where it is offered, what it shows, and
/// the speed under the hand giving way to the service's.
final class CursorSpeedTests: XCTestCase {
    private func device(paired: Bool = true, speed: UInt32) -> DeviceStatus {
        var device = DeviceStatus()
        device.id = "pc"
        device.paired = paired
        device.speedPercent = speed
        return device
    }

    func testOfferedForAnotherComputerWhoseServiceKnowsSpeeds() {
        XCTAssertEqual(CursorSpeed.shown(device(speed: 150), thisDevice: false), 150)
        XCTAssertEqual(CursorSpeed.shown(device(speed: 100), thisDevice: false), 100)
        // Offline computers keep theirs: it is this Mac's own choice.
        var offline = device(speed: 200)
        offline.state = .offline
        XCTAssertEqual(CursorSpeed.shown(offline, thisDevice: false), 200)
        XCTAssertNil(CursorSpeed.shown(device(speed: 100), thisDevice: true))
        // One from the configuration file too.
        XCTAssertEqual(CursorSpeed.shown(device(paired: false, speed: 120), thisDevice: false), 120)
        // A service from before cursor speeds says 0.
        XCTAssertNil(CursorSpeed.shown(device(speed: 0), thisDevice: false))
        XCTAssertNil(CursorSpeed.shown(nil, thisDevice: false))
    }

    func testSnapsToStepsInRangeAndSaysThePercent() {
        XCTAssertEqual(CursorSpeed.snapped(154), 150)
        XCTAssertEqual(CursorSpeed.snapped(155), 160)
        XCTAssertEqual(CursorSpeed.snapped(12), 50)
        XCTAssertEqual(CursorSpeed.snapped(999), 300)
        XCTAssertEqual(CursorSpeed.label(150), "150%")
        XCTAssertEqual(CursorSpeed.label(50), "50%")
        XCTAssertTrue(CursorSpeed.showsReset(150))
        XCTAssertFalse(CursorSpeed.showsReset(100))
        XCTAssertEqual(CursorSpeed.ticks.map(CursorSpeed.fraction), [0, 0.2, 0.6, 1])
        XCTAssertTrue(CursorSpeed.description("Windows Desktop").contains("Windows Desktop"))
    }

    func testADragSendsWhereItIsLetGoAndThenShowsTheService() {
        var draft = CursorSpeedDraft()
        XCTAssertEqual(draft.shown(100), 100)
        draft.began()
        draft.moved(to: 131, service: 100)
        XCTAssertEqual(draft.shown(100), 130)
        // Nothing goes while the slider is held.
        XCTAssertNil(draft.pending(service: 100))
        draft.moved(to: 152, service: 100)
        XCTAssertEqual(draft.ended(service: 100), 150)
        let sent = draft.sending(150)
        XCTAssertEqual(draft.shown(100), 150)
        draft.answered(sent)
        XCTAssertEqual(draft.shown(150), 150)
        XCTAssertNil(draft.value)
    }

    func testARefusedSpeedGoesBackToTheService() {
        var draft = CursorSpeedDraft()
        draft.began()
        draft.moved(to: 250, service: 100)
        XCTAssertEqual(draft.ended(service: 100), 250)
        let sent = draft.sending(250)
        // Refused: the service still has 100, and so does the slider.
        draft.answered(sent)
        XCTAssertEqual(draft.shown(100), 100)
    }

    func testLettingGoWhereItStartedSendsNothing() {
        var draft = CursorSpeedDraft()
        draft.began()
        draft.moved(to: 180, service: 150)
        draft.moved(to: 149, service: 150)
        XCTAssertNil(draft.ended(service: 150))
        // And the service's next speed shows, from wherever it comes.
        XCTAssertEqual(draft.shown(200), 200)
    }

    func testTheKeyboardAndResetSendWithoutTheSliderHeld() {
        var draft = CursorSpeedDraft()
        draft.moved(to: 110, service: 100)
        XCTAssertEqual(draft.pending(service: 100), 110)
        draft.moved(to: 100, service: 100)
        XCTAssertNil(draft.pending(service: 100))
        XCTAssertNil(draft.value)
        // Reset, from 150.
        draft.moved(to: Double(CursorSpeed.normal), service: 150)
        XCTAssertEqual(draft.pending(service: 150), 100)
        XCTAssertEqual(draft.shown(150), 100)
        // An answer while a newer choice waits leaves that choice.
        let sent = draft.sending(130)
        draft.answered(sent)
        XCTAssertEqual(draft.shown(150), 100)
        draft.settle(service: 100)
        XCTAssertNil(draft.value)
    }

    func testTheServicesSpeedChosenAgainWhileAnotherIsOnItsWayStillGoes() {
        var draft = CursorSpeedDraft()
        draft.moved(to: 110, service: 100)
        XCTAssertEqual(draft.pending(service: 100), 110)
        let first = draft.sending(110)
        // On its way, it is what shows.
        XCTAssertEqual(draft.shown(100), 110)
        // Back to 100 before the service answered: that is a change too.
        draft.moved(to: 100, service: 100)
        XCTAssertEqual(draft.pending(service: 100), 100)
        let second = draft.sending(100)
        // The answer to the first leaves the second on its way.
        draft.answered(first)
        XCTAssertEqual(draft.shown(110), 100)
        draft.answered(second)
        XCTAssertEqual(draft.shown(100), 100)
        XCTAssertNil(draft.value)
        // Choosing what is on its way again is no change.
        draft.moved(to: 130, service: 100)
        _ = draft.sending(130)
        draft.moved(to: 130, service: 100)
        XCTAssertNil(draft.pending(service: 100))
        // Held, the same goes on letting go.
        draft.began()
        draft.moved(to: 100, service: 100)
        XCTAssertEqual(draft.ended(service: 100), 100)
    }

    func testOnlyTheAnswerToTheLatestSendCounts() {
        var draft = CursorSpeedDraft()
        let sends = [110, 120, 110].map { draft.sending($0) }
        // The answers to the first two change nothing: 110 stays.
        draft.answered(sends[0])
        XCTAssertEqual(draft.shown(100), 110)
        draft.answered(sends[1])
        XCTAssertEqual(draft.shown(100), 110)
        // The latest one's: the service's speed shows, 110 if it took it,
        // 100 if it refused.
        draft.answered(sends[2])
        XCTAssertEqual(draft.shown(110), 110)
        XCTAssertEqual(draft.shown(100), 100)
    }

    func testAnAnswerWhileTheSliderIsHeldLeavesIt() {
        var draft = CursorSpeedDraft()
        draft.moved(to: 120, service: 100)
        let sent = draft.sending(120)
        draft.began()
        draft.moved(to: 140, service: 100)
        draft.answered(sent)
        XCTAssertEqual(draft.shown(100), 140)
    }
}

/// When the cursor speed row sends, and whose slider an answer touches.
@MainActor
final class CursorSpeedEditorTests: XCTestCase {
    /// Records what is sent; while `holding`, the answer waits for
    /// `answer()`.
    @MainActor private final class Service {
        var sent: [String] = []
        var holding = false
        private var waiting: [CheckedContinuation<Void, Never>] = []

        func send(_ id: String, _ percent: Int) async {
            sent.append("\(id) \(percent)")
            if holding { await withCheckedContinuation { waiting.append($0) } }
        }

        func answer() {
            waiting.forEach { $0.resume() }
            waiting = []
        }
    }

    private let delay = Duration.milliseconds(20)

    /// Long enough for anything held back by `delay` to have gone on an
    /// idle machine: only for checking that something did not go.
    private func settle() async throws {
        try await Task.sleep(for: delay * 4)
    }

    /// Waits for what the editor does on its own, however slow the machine
    /// running the tests is.
    private func eventually(_ what: String, _ condition: () -> Bool) async throws {
        let deadline = ContinuousClock.now + .seconds(10)
        while !condition() {
            if ContinuousClock.now > deadline { XCTFail("timed out waiting for \(what)"); return }
            try await Task.sleep(for: .milliseconds(5))
        }
    }

    func testTheKeysSendOnceTheyStopUnlessTheSliderIsBackWhereItWas() async throws {
        let service = Service()
        let editor = CursorSpeedEditor(delay: delay)
        editor.show("pc", send: service.send)
        editor.moved(to: 120, service: 100, on: "pc", send: service.send)
        editor.moved(to: 130, service: 100, on: "pc", send: service.send)
        try await eventually("130 sent") { !service.sent.isEmpty }
        XCTAssertEqual(service.sent, ["pc 130"])
        // Back to the service's speed before the delay: nothing goes.
        editor.moved(to: 140, service: 100, on: "pc", send: service.send)
        editor.moved(to: 100, service: 100, on: "pc", send: service.send)
        try await settle()
        XCTAssertEqual(service.sent, ["pc 130"])
    }

    func testPressingTheSliderHoldsBackWhatTheKeysChose() async throws {
        let service = Service()
        let editor = CursorSpeedEditor(delay: delay)
        editor.moved(to: 120, service: 100, on: "pc", send: service.send)
        editor.began(on: "pc", send: service.send)
        editor.moved(to: 200, service: 100, on: "pc", send: service.send)
        try await settle()
        XCTAssertEqual(service.sent, [], "nothing while it is held")
        editor.ended(service: 100, on: "pc", send: service.send)
        try await eventually("200 sent") { !service.sent.isEmpty }
        XCTAssertEqual(service.sent, ["pc 200"])
    }

    func testAnotherComputerShownSendsWhatWasWaitingForTheFirst() async throws {
        let service = Service()
        let editor = CursorSpeedEditor(delay: .seconds(10))
        editor.moved(to: 150, service: 100, on: "pc", send: service.send)
        editor.show("mini", send: service.send)
        try await eventually("150 sent") { !service.sent.isEmpty }
        XCTAssertEqual(service.sent, ["pc 150"])
        XCTAssertEqual(editor.shown(100, for: "mini"), 100)
        // And when the row goes away.
        editor.moved(to: 60, service: 100, on: "mini", send: service.send)
        editor.leave(listed: ["pc", "mini"], send: service.send)
        try await eventually("60 sent") { service.sent.count > 1 }
        XCTAssertEqual(service.sent, ["pc 150", "mini 60"])
    }

    func testAnAnswerForAnotherComputerLeavesTheOneShown() async throws {
        let service = Service()
        service.holding = true
        let editor = CursorSpeedEditor(delay: .seconds(10))
        editor.began(on: "pc", send: service.send)
        editor.moved(to: 150, service: 100, on: "pc", send: service.send)
        editor.ended(service: 100, on: "pc", send: service.send)
        try await eventually("150 sent") { !service.sent.isEmpty }
        XCTAssertEqual(service.sent, ["pc 150"])
        // The same speed chosen on the mini meanwhile stays when the PC's
        // answer comes.
        editor.show("mini", send: service.send)
        editor.moved(to: 150, service: 100, on: "mini", send: service.send)
        // Back on the PC before its answer: the speed on its way shows.
        XCTAssertEqual(editor.shown(100, for: "pc"), 150)
        service.answer()
        try await eventually("the PC's answer") { editor.shown(120, for: "pc") == 120 }
        XCTAssertEqual(editor.shown(100, for: "mini"), 150)
        XCTAssertEqual(editor.shown(120, for: "pc"), 120)
    }

    func testADragHeldWhenAnotherComputerIsChosenGoesForTheFirst() async throws {
        let service = Service()
        let editor = CursorSpeedEditor(delay: .seconds(10))
        editor.began(on: "pc", send: service.send)
        editor.moved(to: 220, service: 100, on: "pc", send: service.send)
        editor.show("mini", send: service.send)
        try await eventually("220 sent") { !service.sent.isEmpty }
        XCTAssertEqual(service.sent, ["pc 220"])
        XCTAssertFalse(editor.drafts["pc"]?.editing ?? true)
    }

    func testTheServicesSpeedChosenAgainBeforeItAnsweredGoes() async throws {
        let service = Service()
        service.holding = true
        let editor = CursorSpeedEditor(delay: delay)
        editor.moved(to: 110, service: 100, on: "pc", send: service.send)
        try await eventually("110 sent") { !service.sent.isEmpty }
        XCTAssertEqual(service.sent, ["pc 110"])
        editor.moved(to: 100, service: 100, on: "pc", send: service.send)
        try await eventually("100 sent") { service.sent.count > 1 }
        XCTAssertEqual(service.sent, ["pc 110", "pc 100"])
        service.answer()
    }

    func testAComputerNoLongerTrustedLeavesNothingBehind() async throws {
        let service = Service()
        service.holding = true
        let editor = CursorSpeedEditor(delay: delay)
        editor.began(on: "pc", send: service.send)
        editor.moved(to: 150, service: 100, on: "pc", send: service.send)
        editor.ended(service: 100, on: "pc", send: service.send)
        editor.moved(to: 200, service: 100, on: "pc", send: service.send)
        // Unpaired with 150 on its way and 200 waiting: neither shows when
        // it is paired again, and 200 does not go.
        editor.keep(["mini"])
        XCTAssertNil(editor.drafts["pc"])
        XCTAssertEqual(editor.shown(100, for: "pc"), 100)
        try await eventually("150 sent") { !service.sent.isEmpty }
        try await settle()
        XCTAssertEqual(service.sent, ["pc 150"])
        service.answer()
        try await settle()
        XCTAssertNil(editor.drafts["pc"])
    }

    func testAComputerGoneWhenItsRowGoesAwaySendsNothing() async throws {
        let service = Service()
        let editor = CursorSpeedEditor(delay: .seconds(10))
        editor.moved(to: 150, service: 100, on: "pc", send: service.send)
        // Unpaired: the row goes away with the PC no longer listed.
        editor.leave(listed: ["mini"], send: service.send)
        try await Task.sleep(for: delay)
        XCTAssertEqual(service.sent, [])
        XCTAssertNil(editor.drafts["pc"])
        // Nor when another computer is chosen as it goes.
        editor.began(on: "mini", send: service.send)
        editor.moved(to: 60, service: 100, on: "mini", send: service.send)
        editor.show("pc", listed: ["pc"], send: service.send)
        try await Task.sleep(for: delay)
        XCTAssertEqual(service.sent, [])
        XCTAssertNil(editor.drafts["mini"])
    }

    func testResetGoesAtOnceInPlaceOfWhatTheKeysChose() async throws {
        let service = Service()
        let editor = CursorSpeedEditor(delay: delay)
        editor.moved(to: 170, service: 150, on: "pc", send: service.send)
        editor.reset(service: 150, on: "pc", send: service.send)
        try await eventually("the reset answered") { editor.draft.inFlight == nil }
        XCTAssertEqual(service.sent, ["pc 100"])
        // Answered: the slider shows the service's speed again.
        XCTAssertNil(editor.draft.value)
    }
}
