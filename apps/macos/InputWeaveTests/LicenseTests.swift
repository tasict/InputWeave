import Foundation
import XCTest

/// The fixtures the three apps share (testdata/polar).
private let fixtures = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
    .appending(path: "testdata/polar")

private func answer(_ fixture: String?, _ status: Int, date: String? = nil, retryAfter: String? = nil) throws -> PolarAnswer {
    let body = try fixture.map { try Data(contentsOf: fixtures.appending(path: "\($0).json")) } ?? Data()
    return PolarAnswer(status: status, body: body, dateHeader: date, retryAfterHeader: retryAfter)
}

/// Polar's answers in order, per endpoint; nil stands for no network.
private actor ScriptedPolar: PolarTransport {
    struct Sent {
        var endpoint: String
        var body: [String: String]
        var userAgent: String?
        var timeout: TimeInterval
    }

    private var answers: [String: [PolarAnswer?]] = [:]
    private(set) var sent: [Sent] = []
    private var holding: String?
    private var held: CheckedContinuation<Void, Never>?

    func queue(_ endpoint: String, _ answer: PolarAnswer?) {
        answers[endpoint, default: []].append(answer)
    }

    /// The next request to `endpoint` waits for `release()`.
    func hold(_ endpoint: String) {
        holding = endpoint
    }

    func release() {
        holding = nil
        held?.resume()
        held = nil
    }

    func send(_ request: URLRequest) async throws -> PolarAnswer {
        let endpoint = request.url?.lastPathComponent ?? ""
        let body = (try? JSONSerialization.jsonObject(with: request.httpBody ?? Data())) as? [String: String] ?? [:]
        sent.append(Sent(
            endpoint: endpoint, body: body, userAgent: request.value(forHTTPHeaderField: "User-Agent"),
            timeout: request.timeoutInterval))
        if holding == endpoint {
            await withCheckedContinuation { held = $0 }
        }
        guard var list = answers[endpoint], !list.isEmpty else { throw URLError(.notConnectedToInternet) }
        let next = list.removeFirst()
        answers[endpoint] = list
        guard let next else { throw URLError(.notConnectedToInternet) }
        return next
    }
}

/// The service's side, as far as the license goes.
@MainActor
private final class FakeAgent: LicenseAgent {
    var license = LicenseRecord()
    var licenseLinked = true
    var thisComputerName = "MacBook Pro"
    var recorded: [Inputweave_App_V1_RecordLicenseResult] = []
    var keysSet: [String] = []
    var removed = 0
    var removeAttempts = 0
    /// A refusal code for every RecordLicenseResult.
    var refuseRecords: String?
    /// No answer to RecordLicenseResult or RemoveLicense: the link failed.
    var failRecords = false
    var failRemove = false
    /// A refusal code for RemoveLicense.
    var refuseRemove: String?

    func setLicenseKey(_ key: String) async throws -> LicenseRecord {
        keysSet.append(key)
        guard key.count >= 20 else { throw AgentRefusal(code: "invalid_license_key", detail: "") }
        if license.state == .licensed && license.key == key { return license }
        license = LicenseRecord()
        license.key = key
        license.state = .pendingActivation
        return license
    }

    func recordLicense(_ result: Inputweave_App_V1_RecordLicenseResult) async throws {
        recorded.append(result)
        if failRecords { throw URLError(.networkConnectionLost) }
        if let refuseRecords { throw AgentRefusal(code: refuseRecords, detail: "") }
        switch result.result {
        case .activated(let activated):
            license.state = .licensed
            license.activationID = activated.activationID
            license.customerEmail = activated.customerEmail
            license.validatedAt = activated.validatedAt
        case .validated(let validated):
            license.validatedAt = validated.validatedAt
        case .refused(let refusal):
            license.state = refusal == .seatsFull ? .seatsFull : .invalid
            license.activationID = ""
        case nil:
            break
        }
    }

    func removeLicense() async throws {
        removeAttempts += 1
        if failRemove { throw URLError(.networkConnectionLost) }
        if let refuseRemove { throw AgentRefusal(code: refuseRemove, detail: "") }
        removed += 1
        license = LicenseRecord()
        license.optedOut = true
    }
}

/// Resumes the controller's waits only when the test says so.
private final class ManualSleeper: @unchecked Sendable {
    private let lock = NSLock()
    private var waiting: [CheckedContinuation<Void, Never>] = []
    private var asked: [Duration] = []

    /// Every wait asked for so far.
    var requested: [Duration] {
        lock.lock()
        defer { lock.unlock() }
        return asked
    }

    func sleep(_ duration: Duration) async {
        await withCheckedContinuation { continuation in
            lock.lock()
            asked.append(duration)
            waiting.append(continuation)
            lock.unlock()
        }
    }

    func wake() {
        lock.lock()
        let woken = waiting
        waiting = []
        lock.unlock()
        woken.forEach { $0.resume() }
    }
}

@MainActor
private final class Clock {
    var now = Date(timeIntervalSince1970: 1_793_800_000)
}

private let key = "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42"
private let activationID = "6b1e3f9a-2c4d-4e8f-9a1b-3c5d7e9f1a2b"
private let day: TimeInterval = 86_400

@MainActor
final class PolarClientTests: XCTestCase {
    private func client(_ polar: ScriptedPolar) -> PolarClient {
        PolarClient(config: .init(server: URL(string: "https://api.polar.sh")!, organizationID: "org", version: "1.2.3"), transport: polar)
    }

    func testAsksAsPolarExpects() async throws {
        let polar = ScriptedPolar()
        await polar.queue("activate", try answer("activate-200", 200))
        _ = await client(polar).activate(key: key, label: "MacBook Pro")
        let request = client(polar).request("activate", ["key": key])
        XCTAssertEqual(request.url?.absoluteString, "https://api.polar.sh/v1/customer-portal/license-keys/activate")
        XCTAssertEqual(request.httpMethod, "POST")
        let sentList = await polar.sent
        let sent = try XCTUnwrap(sentList.first)
        XCTAssertEqual(sent.userAgent, "InputWeave/1.2.3")
        XCTAssertEqual(sent.body, ["key": key, "organization_id": "org", "label": "MacBook Pro"])
        XCTAssertEqual(sent.timeout, 60, "an activation waits longer for its answer")
        _ = await client(polar).validate(key: key, activationID: nil)
        let validate = await polar.sent.last
        XCTAssertEqual(validate?.timeout, 20)
    }

    func testSendsTheComputersNameOnOneShortLine() async throws {
        XCTAssertEqual(PolarClient.label("  Tasha’s\n MacBook\tPro  "), "Tasha’s MacBook Pro")
        XCTAssertEqual(PolarClient.label(String(repeating: "a", count: 100)), String(repeating: "a", count: 64))
        XCTAssertEqual(PolarClient.label(String(repeating: "a", count: 63) + " b"), String(repeating: "a", count: 63))
        XCTAssertEqual(PolarClient.label(" \n\u{7} "), "Mac")
        let polar = ScriptedPolar()
        _ = await client(polar).activate(key: key, label: "Office\r\nMac")
        let sent = await polar.sent.first
        XCTAssertEqual(sent?.body["label"], "Office Mac")
    }

    func testReadsItsConfigFromInfoPlistValues() {
        XCTAssertNil(PolarClient.Config.fromInfo([:], version: "1"))
        let config = PolarClient.Config.fromInfo(
            ["InputWeavePolarServer": "https://api.polar.sh", "InputWeavePolarOrganizationID": "org"], version: "1.0.0")
        XCTAssertEqual(config?.server.absoluteString, "https://api.polar.sh")
        XCTAssertEqual(config?.organizationID, "org")
        let links = LicenseLinks.fromInfo(["InputWeaveCheckoutURL": "https://buy.example/x", "InputWeaveCustomerPortalURL": ""])
        XCTAssertEqual(links.checkout?.absoluteString, "https://buy.example/x")
        XCTAssertNil(links.customerPortal)
    }

    func testReadsActivateAnswers() async throws {
        let cases: [(String?, Int, String?, PolarClient.Activation)] = [
            ("activate-200", 200, "Wed, 04 Nov 2026 13:46:40 GMT",
             .activated(id: activationID, email: "someone@example.com", at: Date(timeIntervalSince1970: 1_793_800_000))),
            ("activate-403-limit", 403, nil, .notPermitted),
            ("activate-403-revoked", 403, nil, .notPermitted),
            ("403-cloudflare", 403, nil, .unreachable),
            ("404-not-found", 404, nil, .invalid),
            ("422-validation", 422, nil, .rejected),
            (nil, 503, nil, .unreachable),
        ]
        for (fixture, status, date, expected) in cases {
            let polar = ScriptedPolar()
            await polar.queue("activate", try answer(fixture, status, date: date))
            let got = await client(polar).activate(key: key, label: "Mac")
            XCTAssertEqual(got, expected, fixture ?? "\(status)")
        }
        let limited = ScriptedPolar()
        await limited.queue("activate", try answer("429-rate-limited", 429, retryAfter: "7"))
        let rate = await client(limited).activate(key: key, label: "Mac")
        XCTAssertEqual(rate, .rateLimited(retryAfter: .seconds(7)))
        let none = await client(ScriptedPolar()).activate(key: key, label: "Mac")
        XCTAssertEqual(none, .unreachable)
        let unnamed = ScriptedPolar()
        await unnamed.queue("activate", PolarAnswer(status: 200, body: Data(#"{"license_key":{}}"#.utf8)))
        let noID = await client(unnamed).activate(key: key, label: "Mac")
        XCTAssertEqual(noID, .activatedUnnamed, "a seat was taken, though it cannot be named")
        let aboutTheKey = ScriptedPolar()
        await aboutTheKey.queue("activate", unreadable("key"))
        let invalid = await client(aboutTheKey).activate(key: key, label: "Mac")
        XCTAssertEqual(invalid, .invalid, "only a 422 about the key is a refusal")
    }

    /// A 422 about one field of the request.
    private func unreadable(_ field: String) -> PolarAnswer {
        let body = #"{"error":"RequestValidationError","detail":[{"type":"value_error","loc":["body","\#(field)"],"msg":"x"}]}"#
        return PolarAnswer(status: 422, body: Data(body.utf8))
    }

    func testReadsValidateAndDeactivateAnswers() async throws {
        let cases: [(String?, Int, PolarClient.Validation)] = [
            ("validate-200", 200, .valid(email: "someone@example.com", at: nil)),
            ("validate-200-key-only", 200, .valid(email: "someone@example.com", at: nil)),
            ("404-not-found", 404, .notFound),
            ("404-no-longer-active", 404, .notFound),
            ("422-validation", 422, .notFound),
            ("403-cloudflare", 403, .unreachable),
        ]
        for (fixture, status, expected) in cases {
            let polar = ScriptedPolar()
            await polar.queue("validate", try answer(fixture, status))
            let got = await client(polar).validate(key: key, activationID: "a1")
            XCTAssertEqual(got, expected, fixture ?? "\(status)")
            let sentList = await polar.sent
            let sent = try XCTUnwrap(sentList.first)
            XCTAssertEqual(sent.body["activation_id"], "a1")
        }
        let organization = ScriptedPolar()
        await organization.queue("validate", unreadable("organization_id"))
        let unread = await client(organization).validate(key: key, activationID: "a1")
        XCTAssertEqual(unread, .unreachable, "not about the key or the activation: changes nothing")

        let keyOnly = ScriptedPolar()
        await keyOnly.queue("validate", try answer("validate-200-key-only", 200))
        _ = await client(keyOnly).validate(key: key, activationID: nil)
        let sentList = await keyOnly.sent
        let sent = try XCTUnwrap(sentList.first)
        XCTAssertNil(sent.body["activation_id"])

        let deactivations: [(String?, Int, PolarClient.Deactivation)] = [
            (nil, 204, .done), ("404-not-found", 404, .done), ("activate-403-revoked", 403, .refused),
            ("422-validation", 422, .refused), ("403-cloudflare", 403, .unreachable), (nil, 500, .unreachable),
        ]
        let limited = ScriptedPolar()
        await limited.queue("deactivate", try answer("429-rate-limited", 429, retryAfter: "7"))
        let wait = await client(limited).deactivate(key: key, activationID: "a1")
        XCTAssertEqual(wait, .rateLimited(retryAfter: .seconds(7)))
        let organization2 = ScriptedPolar()
        await organization2.queue("deactivate", unreadable("organization_id"))
        let kept = await client(organization2).deactivate(key: key, activationID: "a1")
        XCTAssertEqual(kept, .unreachable)
        for (fixture, status, expected) in deactivations {
            let polar = ScriptedPolar()
            await polar.queue("deactivate", try answer(fixture, status))
            let got = await client(polar).deactivate(key: key, activationID: "a1")
            XCTAssertEqual(got, expected, fixture ?? "\(status)")
        }
    }
}

@MainActor
final class LicenseControllerTests: XCTestCase {
    private var agent: FakeAgent!
    private var polar: ScriptedPolar!
    private var clock: Clock!
    private var sleeper: ManualSleeper!
    private var store: MemoryLicenseOutboxStore!
    private var controller: LicenseController!

    override func setUp() async throws {
        agent = FakeAgent()
        polar = ScriptedPolar()
        clock = Clock()
        sleeper = ManualSleeper()
        store = MemoryLicenseOutboxStore()
        controller = makeController()
    }

    /// A controller over the same service, Polar and kept outbox: a new one
    /// stands for the app launched again.
    private func makeController() -> LicenseController {
        let clock = clock!
        let sleeper = sleeper!
        return LicenseController(
            agent: agent,
            polar: PolarClient(config: .init(server: URL(string: "https://polar.test")!, organizationID: "org", version: "1.0.0"), transport: polar),
            links: LicenseLinks(checkout: URL(string: "https://buy.example/checkout"), customerPortal: URL(string: "https://polar.example/portal")),
            store: store,
            now: { clock.now },
            sleep: { await sleeper.sleep($0) })
    }

    private func pending() {
        agent.license.key = key
        agent.license.state = .pendingActivation
    }

    private func licensed(confirmedDaysAgo days: Double) {
        agent.license.key = key
        agent.license.state = .licensed
        agent.license.activationID = "act-1"
        agent.license.validatedAt = UInt64(clock.now.addingTimeInterval(-days * day).timeIntervalSince1970)
    }

    private func endpoints() async -> [String] {
        await polar.sent.map(\.endpoint)
    }

    private func later(_ seconds: TimeInterval = 31) {
        clock.now = clock.now.addingTimeInterval(seconds)
    }

    /// Gives a wake the controller set the time to ask for its wait.
    private func letWakesStart() async throws {
        for _ in 0..<20 {
            await Task.yield()
            try await Task.sleep(for: .milliseconds(2))
        }
    }

    // MARK: Activating

    func testActivatesAKeyWaitingForIt() async throws {
        pending()
        await polar.queue("activate", try answer("activate-200", 200, date: "Wed, 04 Nov 2026 13:46:40 GMT"))
        controller.update()
        await controller.settle()
        let sentList = await polar.sent
        let sent = try XCTUnwrap(sentList.first)
        XCTAssertEqual(sent.body["label"], "MacBook Pro")
        XCTAssertNil(sent.body["conditions"])
        let result = try XCTUnwrap(agent.recorded.first)
        XCTAssertEqual(result.key, key)
        XCTAssertEqual(result.activated.activationID, activationID)
        XCTAssertEqual(result.activated.customerEmail, "someone@example.com")
        XCTAssertEqual(result.activated.validatedAt, 1_793_800_000)
        XCTAssertEqual(agent.license.state, .licensed)
        XCTAssertTrue(controller.isLicensed)
        XCTAssertTrue(store.outbox.ops.isEmpty)
        controller.update()
        await controller.settle()
        let count = await polar.sent.count
        XCTAssertEqual(count, 1)
        XCTAssertFalse(controller.justActivated, "only the user's own Activate sews the label")
    }

    func testTheUsersActivateClosesTheSheetAndSewsOnce() async throws {
        await polar.queue("activate", try answer("activate-200", 200))
        controller.beginEntering()
        controller.submit("  \(key)\n")
        await controller.settle()
        XCTAssertEqual(agent.keysSet, [key])
        XCTAssertFalse(controller.entering)
        XCTAssertFalse(controller.activating)
        XCTAssertNil(controller.keyError)
        XCTAssertTrue(controller.justActivated)
        controller.labelSewn()
        XCTAssertFalse(controller.justActivated)
    }

    func testTellsSeatsFullFromARefundedKey() async throws {
        await polar.queue("activate", try answer("activate-403-limit", 403))
        await polar.queue("validate", try answer("validate-200-key-only", 200))
        controller.beginEntering()
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(controller.keyError, .seatsFull)
        XCTAssertEqual(agent.recorded.last?.refused, .seatsFull)
        let sentList = await polar.sent
        let probe = try XCTUnwrap(sentList.last)
        XCTAssertEqual(probe.endpoint, "validate")
        XCTAssertNil(probe.body["activation_id"])
        XCTAssertTrue(controller.entering)

        await polar.queue("activate", try answer("activate-403-revoked", 403))
        await polar.queue("validate", try answer("404-no-longer-active", 404))
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(controller.keyError, .noLongerValid)
        XCTAssertEqual(agent.recorded.last?.refused, .invalid)
    }

    func testAnUnknownKey() async throws {
        await polar.queue("activate", try answer("404-not-found", 404))
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(controller.keyError, .notAKey)
        XCTAssertEqual(agent.recorded.last?.refused, .invalid)

        // One the service refuses outright never reaches Polar.
        controller.submit("INPUTWEAVE")
        await controller.settle()
        XCTAssertEqual(controller.keyError, .notAKey)
        let count = await polar.sent.count
        XCTAssertEqual(count, 1)
    }

    func testAsksAgainLaterWhenPolarCannotBeReached() async throws {
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(controller.keyError, .polarUnreachable)
        XCTAssertFalse(controller.activating)
        XCTAssertTrue(agent.recorded.isEmpty, "no answer, nothing to report")
        controller.update()
        await controller.settle()
        var count = await polar.sent.count
        XCTAssertEqual(count, 1, "waits before asking again")

        later()
        await polar.queue("activate", try answer("activate-200", 200))
        controller.update()
        await controller.settle()
        count = await polar.sent.count
        XCTAssertEqual(count, 2)
        XCTAssertEqual(agent.license.state, .licensed)
        XCTAssertNil(controller.keyError, "the sheet learns of the late success")
        XCTAssertFalse(controller.entering)
    }

    // MARK: Reports the service refuses or misses

    func testFreesAnActivationTheServiceRefusesAndDoesNotTryAgain() async throws {
        agent.refuseRecords = "invalid_license_result"
        await polar.queue("activate", try answer("activate-200", 200))
        await polar.queue("deactivate", try answer(nil, 204))
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(controller.keyError, .failed)
        controller.update()
        await controller.settle()
        var all = await endpoints()
        XCTAssertEqual(all, ["activate", "deactivate"])
        XCTAssertEqual(all.filter { $0 == "deactivate" }.count, 1)
        XCTAssertTrue(store.outbox.ops.isEmpty)
        XCTAssertEqual(store.outbox.stalled.count, 1)

        // The service's record changes: the key is tried again.
        agent.refuseRecords = nil
        agent.license.sourceDeviceID = "pc"
        await polar.queue("activate", try answer("activate-200", 200))
        controller.update()
        await controller.settle()
        all = await endpoints()
        XCTAssertEqual(all, ["activate", "deactivate", "activate"])
        XCTAssertEqual(agent.license.state, .licensed)
        XCTAssertTrue(store.outbox.stalled.isEmpty, "forgotten once the record changed")
    }

    func testAnAnswerThatCouldNotBeReadOrAStoppingServiceIsTemporary() async throws {
        pending()
        agent.refuseRecords = "unexpected"
        await polar.queue("activate", try answer("activate-200", 200))
        controller.update()
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.report], "kept, not freed")
        XCTAssertTrue(store.outbox.stalled.isEmpty)

        agent.refuseRecords = "agent_stopping"
        later()
        controller.update()
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.report])

        agent.refuseRecords = nil
        later(121)
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.license.state, .licensed)
        let all = await endpoints()
        XCTAssertEqual(all, ["activate"], "never freed")
    }

    func testAnyOtherAnswerTheServiceGivesFreesTheActivation() async throws {
        pending()
        agent.refuseRecords = "a_code_from_a_newer_service"
        await polar.queue("activate", try answer("activate-200", 200))
        await polar.queue("deactivate", try answer(nil, 204))
        controller.update()
        await controller.settle()
        let all = await endpoints()
        XCTAssertEqual(all, ["activate", "deactivate"])
        XCTAssertEqual(store.outbox.stalled.count, 1, "not activated again while the record is the same")
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    func testReportsOnlyOnceTheActivationIsKept() async throws {
        store.failSaves = true
        await polar.queue("activate", try answer("activate-200", 200))
        controller.beginEntering()
        controller.submit(key)
        await controller.settle()
        XCTAssertTrue(agent.recorded.isEmpty, "not reported while a restart could lose it")
        XCTAssertEqual(controller.keyError, .failed)
        XCTAssertEqual(agent.license.state, .pendingActivation)

        store.failSaves = false
        sleeper.wake()
        try await letWakesStart()
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [], "kept, reported, done")
        XCTAssertEqual(agent.recorded.last?.activated.activationID, activationID)
        XCTAssertEqual(agent.license.state, .licensed)
        XCTAssertNil(controller.keyError, "the sheet keeps following")
        XCTAssertFalse(controller.entering)
        let all = await endpoints()
        XCTAssertEqual(all, ["activate"])
    }

    func testRetryAfterIsNeverShortened() async throws {
        await polar.queue("activate", try answer("429-rate-limited", 429, retryAfter: "300"))
        controller.beginEntering()
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(controller.keyError, .polarUnreachable)

        controller.submit(key)
        await controller.settle()
        later()
        controller.update()
        await controller.settle()
        var count = await polar.sent.count
        XCTAssertEqual(count, 1, "Polar said when")

        later(300)
        await polar.queue("activate", try answer("activate-200", 200))
        controller.update()
        await controller.settle()
        count = await polar.sent.count
        XCTAssertEqual(count, 2)
        XCTAssertEqual(agent.license.state, .licensed)
    }

    func testARequestPolarCannotReadIsNoRefusal() async throws {
        await polar.queue("activate", try answer("422-validation", 422))
        controller.beginEntering()
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(controller.keyError, .failed)
        XCTAssertTrue(agent.recorded.isEmpty, "nothing refused")
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.activate], "asked again later")
    }

    func testAlreadyLicensedWithAnotherActivationFreesOurs() async throws {
        pending()
        agent.refuseRecords = "already_licensed"
        await polar.queue("activate", try answer("activate-200", 200))
        await polar.queue("deactivate", try answer(nil, 204))
        controller.update()
        await controller.settle()
        let sent = await polar.sent
        XCTAssertEqual(sent.map(\.endpoint), ["activate", "deactivate"])
        XCTAssertEqual(sent.last?.body["activation_id"], activationID)
    }

    func testATemporaryAnswerIsNeverTakenForGood() async throws {
        pending()
        agent.refuseRecords = "license_unavailable"
        await polar.queue("activate", try answer("activate-200", 200))
        controller.update()
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.report], "kept, not freed")
        XCTAssertTrue(store.outbox.stalled.isEmpty)
        agent.refuseRecords = nil
        later()
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.license.state, .licensed)
        let all = await endpoints()
        XCTAssertEqual(all, ["activate"])
    }

    func testAnActivationTheServiceMissedIsReportedAfterARestartNotMadeAgain() async throws {
        pending()
        agent.failRecords = true
        await polar.queue("activate", try answer("activate-200", 200))
        controller.update()
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.report])
        XCTAssertEqual(agent.license.state, .pendingActivation)

        agent.failRecords = false
        controller = makeController()
        later()
        controller.update()
        await controller.settle()
        let all = await endpoints()
        XCTAssertEqual(all, ["activate"], "the same activation is reported, not a new one made")
        XCTAssertEqual(agent.license.activationID, activationID)
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    func testARefusalTheServiceMissedIsNotShownAndWaits() async throws {
        agent.failRecords = true
        await polar.queue("activate", try answer("activate-403-limit", 403))
        await polar.queue("validate", try answer("validate-200-key-only", 200))
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(controller.keyError, .failed, "not seats full, which the service did not record")
        controller.update()
        await controller.settle()
        let all = await endpoints()
        XCTAssertEqual(all, ["activate", "validate"], "waits before trying again")
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.refuse])
    }

    func testFreesKeepTryingAcrossARestartAndWhileTheServiceIsAway() async throws {
        pending()
        agent.refuseRecords = "license_stale"
        await polar.queue("activate", try answer("activate-200", 200))
        controller.update()
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.free])

        controller = makeController()
        agent.licenseLinked = false
        later()
        await polar.queue("deactivate", try answer(nil, 204))
        controller.update()
        await controller.settle()
        XCTAssertTrue(store.outbox.ops.isEmpty)
        let all = await endpoints()
        XCTAssertEqual(all, ["activate", "deactivate", "deactivate"])
    }

    func testAFreePolarRefusesForGoodIsDropped() async throws {
        pending()
        agent.refuseRecords = "license_stale"
        await polar.queue("activate", try answer("activate-200", 200))
        await polar.queue("deactivate", try answer("422-validation", 422))
        controller.update()
        await controller.settle()
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    // MARK: Removing

    func testRemovesAndFreesTheSeat() async throws {
        licensed(confirmedDaysAgo: 1)
        await polar.queue("deactivate", try answer(nil, 204))
        controller.remove()
        XCTAssertTrue(controller.removing)
        XCTAssertFalse(controller.isLicensed)
        await controller.settle()
        XCTAssertEqual(agent.removed, 1)
        XCTAssertFalse(controller.removing)
        XCTAssertFalse(controller.removalPending)
        XCTAssertFalse(controller.seatNotFreedYet)
        let sentList = await polar.sent
        XCTAssertEqual(sentList.last?.body["activation_id"], "act-1")
    }

    func testRemovesEvenWhenPolarCannotBeToldAndFreesTheSeatLater() async throws {
        licensed(confirmedDaysAgo: 1)
        controller.remove()
        await controller.settle()
        XCTAssertEqual(agent.removed, 1)
        XCTAssertTrue(controller.seatNotFreedYet)
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.free], "kept across launches")

        controller = makeController()
        await controller.settle()
        XCTAssertTrue(controller.seatNotFreedYet, "the notice survives a relaunch")
        later()
        await polar.queue("deactivate", try answer(nil, 204))
        controller.update()
        await controller.settle()
        XCTAssertFalse(controller.seatNotFreedYet)
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    func testARemovalTheServiceMissesIsAskedAgainAndNeverShowsLicensed() async throws {
        licensed(confirmedDaysAgo: 1)
        agent.failRemove = true
        await polar.queue("deactivate", try answer(nil, 204))
        controller.remove()
        await controller.settle()
        XCTAssertTrue(controller.removalPending)
        XCTAssertFalse(controller.removing)
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.free, .remove], "the seat is freed once the service removed it")
        var all = await endpoints()
        XCTAssertEqual(all, [])
        XCTAssertEqual(agent.license.state, .licensed)
        XCTAssertFalse(controller.isLicensed, "a freed activation never shows as licensed")

        agent.failRemove = false
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.removeAttempts, 1, "waits before asking again")
        later()
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.removed, 1)
        XCTAssertFalse(controller.removalPending)
        XCTAssertTrue(store.outbox.ops.isEmpty)
        all = await endpoints()
        XCTAssertEqual(all, ["deactivate"])
    }

    func testARemovalTheServiceRefusesForGoodIsDroppedAndSaidOnce() async throws {
        licensed(confirmedDaysAgo: 1)
        agent.refuseRemove = "license_unsupported"
        controller.surfaceAppeared()
        controller.remove()
        await controller.settle()
        XCTAssertTrue(controller.removalRefused)
        XCTAssertTrue(controller.isLicensed, "the license, and its seat, stay")
        XCTAssertTrue(store.outbox.ops.isEmpty, "nothing left to block what comes next")
        let all = await endpoints()
        XCTAssertEqual(all, [], "the seat is not freed")
        controller.surfaceDisappeared()
        XCTAssertFalse(controller.removalRefused, "said once")
    }

    func testAKeyWaitingBehindARemovalIsNeverDropped() async throws {
        licensed(confirmedDaysAgo: 1)
        agent.failRemove = true
        await polar.queue("deactivate", try answer(nil, 204))
        controller.remove()
        await controller.settle()

        controller.beginEntering()
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(agent.removeAttempts, 2, "asked again at once for the key")
        XCTAssertEqual(controller.keyError, .failed)
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.free, .remove], "the key waits; it is not kept")
        XCTAssertTrue(agent.keysSet.isEmpty)

        agent.failRemove = false
        await polar.queue("activate", try answer("activate-200", 200))
        later(121)
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.keysSet, [key])
        XCTAssertEqual(agent.license.state, .licensed)
        XCTAssertFalse(controller.entering, "the sheet kept following")
        XCTAssertNil(controller.keyError)
    }

    func testRemovingSaysItKeepsTryingWhileTheServiceIsAway() async throws {
        licensed(confirmedDaysAgo: 1)
        await controller.settle()
        agent.licenseLinked = false
        controller.remove()
        await controller.settle()
        XCTAssertTrue(controller.removing)
        try await letWakesStart()
        XCTAssertEqual(sleeper.requested, [LicenseController.awayAfter])
        sleeper.wake()
        try await letWakesStart()
        XCTAssertTrue(controller.serviceAway)
        XCTAssertFalse(controller.removing)
        XCTAssertTrue(controller.removalPending)
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.free, .remove], "kept, not dropped")
    }

    func testARemovalDropsAnActivationWaitingAndFreesOneTheServiceHasNotTaken() async throws {
        pending()
        controller.update()
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.activate])
        controller.remove()
        await controller.settle()
        later()
        controller.update()
        await controller.settle()
        var all = await endpoints()
        XCTAssertEqual(all, ["activate"], "not asked again after the removal")
        XCTAssertEqual(agent.removed, 1)

        pending()
        agent.failRecords = true
        await polar.queue("activate", try answer("activate-200", 200))
        controller.update()
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.report])
        let reports = agent.recorded.count
        agent.failRecords = false
        await polar.queue("deactivate", try answer(nil, 204))
        controller.remove()
        await controller.settle()
        XCTAssertEqual(agent.recorded.count, reports, "never reported after the removal")
        let sent = await polar.sent
        XCTAssertEqual(sent.map(\.endpoint), ["activate", "activate", "deactivate"])
        XCTAssertEqual(sent.last?.body["activation_id"], activationID)
        XCTAssertEqual(agent.removed, 2)
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    func testAnActivationAnsweredAfterARemovalIsFreed() async throws {
        pending()
        await polar.queue("activate", try answer("activate-200", 200))
        await polar.queue("deactivate", try answer(nil, 204))
        await polar.hold("activate")
        controller.update()
        while await polar.sent.isEmpty { await Task.yield() }
        controller.remove()
        await polar.release()
        await controller.settle()
        let sent = await polar.sent
        XCTAssertEqual(sent.map(\.endpoint), ["activate", "deactivate"])
        XCTAssertEqual(sent.last?.body["activation_id"], activationID)
        XCTAssertTrue(agent.recorded.isEmpty, "never reported")
        XCTAssertEqual(agent.removed, 1)
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    func testAProbeThatFailsAfterARemovalNeverActivatesAgain() async throws {
        pending()
        await polar.queue("activate", try answer("activate-403-limit", 403))
        await polar.hold("validate")
        controller.update()
        while await polar.sent.count < 2 { await Task.yield() }
        controller.remove()
        await polar.release()
        await controller.settle()
        later(3_600)
        controller.update()
        await controller.settle()
        let all = await endpoints()
        XCTAssertEqual(all, ["activate", "validate"])
        XCTAssertTrue(agent.recorded.isEmpty)
        XCTAssertEqual(agent.removed, 1)
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    func testARemovalStillOwedGoesBeforeANewKey() async throws {
        licensed(confirmedDaysAgo: 1)
        agent.failRemove = true
        await polar.queue("deactivate", try answer(nil, 204))
        controller.remove()
        await controller.settle()

        agent.failRemove = false
        await polar.queue("activate", try answer("activate-200", 200))
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(agent.removed, 1, "removed first, without waiting")
        XCTAssertEqual(agent.keysSet, [key])
        XCTAssertEqual(agent.license.state, .licensed)
        XCTAssertEqual(agent.license.activationID, activationID, "a new activation, not the freed one")
        XCTAssertTrue(controller.isLicensed)
    }

    // MARK: The sheet and the worker

    func testAnOutcomeFromBeforeTheKeyWasEnteredAgainNeverReachesTheSheet() async throws {
        agent.failRecords = true
        await polar.queue("activate", try answer("activate-403-limit", 403))
        await polar.queue("validate", try answer("validate-200-key-only", 200))
        controller.beginEntering()
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.refuse])

        // A seat was freed meanwhile; the user enters the key again.
        agent.failRecords = false
        let before = agent.recorded.count
        await polar.queue("activate", try answer("activate-200", 200))
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(agent.recorded.count, before + 1, "the refusal from before is never sent")
        XCTAssertEqual(agent.recorded.last?.activated.activationID, activationID)
        XCTAssertNil(controller.keyError)
        XCTAssertFalse(controller.entering)
        XCTAssertEqual(agent.license.state, .licensed)
    }

    func testAnActivationFromBeforeIsReportedAfterTheKeyAndKeepsItsSeat() async throws {
        pending()
        agent.failRecords = true
        await polar.queue("activate", try answer("activate-200", 200))
        controller.update()
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.report])

        agent.failRecords = false
        controller.beginEntering()
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(agent.keysSet, [key], "the key goes without waiting")
        XCTAssertEqual(agent.license.activationID, activationID)
        let all = await endpoints()
        XCTAssertEqual(all, ["activate"], "the seat Polar gave is kept, not a second one taken")
        XCTAssertNil(controller.keyError)
        XCTAssertFalse(controller.entering)
    }

    func testAnActivationIsNeverAskedForAgainAtOnce() async throws {
        controller.beginEntering()
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(controller.keyError, .polarUnreachable)
        later()
        controller.update()
        await controller.settle()
        var count = await polar.sent.count
        XCTAssertEqual(count, 2, "the second try now waits two minutes")

        controller.submit(key)
        await controller.settle()
        count = await polar.sent.count
        XCTAssertEqual(count, 2, "an answer that was lost may have taken a seat")
        XCTAssertTrue(controller.activating)

        later()
        await polar.queue("activate", try answer("activate-200", 200))
        controller.update()
        await controller.settle()
        count = await polar.sent.count
        XCTAssertEqual(count, 3, "the user's Activate brings it forward to half a minute after the last")
        XCTAssertEqual(agent.license.state, .licensed)
        XCTAssertNil(controller.keyError)
        XCTAssertFalse(controller.entering)
    }

    func testTheKeyNeverWaitsBehindAnotherBackoff() async throws {
        store.outbox = LicenseOutbox(ops: [
            .init(kind: .refuse, key: "INPUTWEAVE-OTHER-0000-0000", refusal: 1, tries: 3, notBefore: clock.now.addingTimeInterval(600)),
        ])
        controller = makeController()
        await polar.queue("activate", try answer("activate-200", 200))
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(agent.keysSet, [key])
        XCTAssertEqual(agent.license.state, .licensed)
        XCTAssertNil(controller.keyError)
    }

    func testNeverSpinsWhileNothingCanRun() async throws {
        agent.licenseLinked = false
        store.outbox = LicenseOutbox(ops: [.init(kind: .remove, key: key, tries: 1, notBefore: clock.now.addingTimeInterval(-60))])
        controller = makeController()
        controller.update()
        await controller.settle()
        try await letWakesStart()
        XCTAssertEqual(sleeper.requested, [LicenseController.awayAfter], "only the notice's timer, never a wake")
        for _ in 0..<5 {
            sleeper.wake()
            try await letWakesStart()
        }
        XCTAssertEqual(sleeper.requested, [LicenseController.awayAfter])
        XCTAssertEqual(agent.removeAttempts, 0)
        XCTAssertTrue(controller.removalPending)

        agent.licenseLinked = true
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.removed, 1, "the service coming back runs it")
    }

    func testWakesForTheServicesFirstOperationNotOneBehindIt() async throws {
        store.outbox = LicenseOutbox(ops: [
            .init(kind: .remove, key: key, tries: 1, notBefore: clock.now.addingTimeInterval(600)),
            .init(kind: .refuse, key: key, refusal: 1, tries: 1, notBefore: clock.now.addingTimeInterval(-60)),
        ])
        controller = makeController()
        controller.update()
        await controller.settle()
        try await letWakesStart()
        XCTAssertEqual(sleeper.requested, [.seconds(600)])
        XCTAssertEqual(agent.removeAttempts, 0)
        XCTAssertTrue(agent.recorded.isEmpty)
    }

    func testWritesTheOutboxOnceForEachChange() async throws {
        agent.licenseLinked = false
        await controller.settle()
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(store.writes, 0, "a key the user typed is never kept")

        store.outbox = LicenseOutbox(ops: [
            .init(kind: .remove, key: key, tries: 1, notBefore: clock.now.addingTimeInterval(600)),
            .init(kind: .refuse, key: "INPUTWEAVE-OTHER-0000-0000", refusal: 1, tries: 1, notBefore: clock.now.addingTimeInterval(600)),
        ])
        controller = makeController()
        await controller.settle()
        let writes = store.writes
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(store.writes, writes + 1, "both waits end in one write")
    }

    func testReadsTheOutboxInTheBackground() async throws {
        store.outbox = LicenseOutbox(ops: [.init(kind: .free, key: key, activationID: "a1", fromRemove: true, tries: 1)])
        controller = makeController()
        XCTAssertEqual(controller.loading, .loading)
        XCTAssertFalse(controller.seatNotFreedYet, "the service's record meanwhile")
        await controller.settle()
        XCTAssertEqual(controller.loading, .loaded)
        XCTAssertTrue(controller.seatNotFreedYet)
    }

    func testAnUnreadableOutboxIsLeftAloneAndReadAgainLater() async throws {
        pending()
        store.outbox = LicenseOutbox(ops: [.init(
            kind: .report, key: key, activationID: activationID, validatedAt: UInt64(clock.now.timeIntervalSince1970), tries: 1)])
        store.unreadable = true
        controller = makeController()
        await controller.settle()
        XCTAssertEqual(controller.loading, .unreadable)
        controller.update()
        await controller.settle()
        var all = await endpoints()
        XCTAssertEqual(all, [], "no second activation for a key whose report may be waiting")
        XCTAssertEqual(store.writes, 0, "left as it is")
        XCTAssertTrue(agent.recorded.isEmpty)

        controller.beginEntering()
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(controller.keyError, .failed, "a key entered now cannot be kept")
        XCTAssertTrue(agent.keysSet.isEmpty)

        store.unreadable = false
        controller.surfaceAppeared()
        await controller.settle()
        XCTAssertEqual(controller.loading, .loaded)
        XCTAssertEqual(agent.license.activationID, activationID, "the kept report goes")
        all = await endpoints()
        XCTAssertEqual(all, [])
    }

    func testAnActivationWithoutAnIDIsNeverAskedForAgainByItself() async throws {
        await polar.queue("activate", PolarAnswer(status: 200, body: Data("{}".utf8)))
        controller.beginEntering()
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(controller.keyError, .failed)
        XCTAssertTrue(store.outbox.ops.isEmpty)
        XCTAssertEqual(store.outbox.stalled.count, 1)
        later(3_600)
        controller.update()
        await controller.settle()
        var count = await polar.sent.count
        XCTAssertEqual(count, 1, "another try would take another seat")

        await polar.queue("activate", try answer("activate-200", 200))
        controller.submit(key)
        await controller.settle()
        count = await polar.sent.count
        XCTAssertEqual(count, 2, "the user's own Activate asks again")
        XCTAssertEqual(agent.license.state, .licensed)
    }

    func testTheKeyNeverWaitsForPolar() async throws {
        store.outbox = LicenseOutbox(ops: [.init(kind: .free, key: "INPUTWEAVE-OTHER-0000-0000", activationID: "old")])
        await polar.hold("deactivate")
        controller = makeController()
        while await polar.sent.isEmpty { await Task.yield() }
        controller.submit(key)
        for _ in 0..<500 where agent.keysSet.isEmpty { await Task.yield() }
        XCTAssertEqual(agent.keysSet, [key], "the service's lane runs while Polar's waits")
        await polar.release()
        await controller.settle()
    }

    func testAKeyEnteredWhileTheServiceIsStillAwayHearsSoAtOnce() async throws {
        await controller.settle()
        agent.licenseLinked = false
        controller.beginEntering()
        controller.submit(key)
        try await letWakesStart()
        sleeper.wake()
        try await letWakesStart()
        XCTAssertEqual(controller.keyError, .serviceNotResponding)

        controller.submit(key)
        XCTAssertEqual(controller.keyError, .serviceNotResponding, "not Activating… for good")
        XCTAssertFalse(controller.activating)
    }

    func testWhileTheOutboxCannotBeWrittenARemovalSaysItKeepsTrying() async throws {
        licensed(confirmedDaysAgo: 1)
        await controller.settle()
        store.failSaves = true
        controller.remove()
        await controller.settle()
        XCTAssertFalse(controller.removing, "never Removing… for good")
        XCTAssertTrue(controller.removalPending)
        XCTAssertEqual(agent.removeAttempts, 0, "not sent before its free is kept")

        controller.beginEntering()
        controller.submit(key)
        sleeper.wake()
        try await letWakesStart()
        await controller.settle()
        XCTAssertEqual(controller.keyError, .failed, "the key waits behind the removal")

        store.failSaves = false
        sleeper.wake()
        try await letWakesStart()
        await controller.settle()
        XCTAssertEqual(agent.removed, 1)
        XCTAssertEqual(agent.keysSet, [key])
    }

    func testAFreeWaitsAsLongAsPolarSays() async throws {
        store.outbox = LicenseOutbox(ops: [.init(kind: .free, key: key, activationID: "a1")])
        await polar.queue("deactivate", try answer("429-rate-limited", 429, retryAfter: "300"))
        controller = makeController()
        await controller.settle()
        later()
        controller.update()
        await controller.settle()
        var all = await endpoints()
        XCTAssertEqual(all, ["deactivate"])
        later(300)
        await polar.queue("deactivate", try answer(nil, 204))
        controller.update()
        await controller.settle()
        all = await endpoints()
        XCTAssertEqual(all, ["deactivate", "deactivate"])
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    func testTheSheetSaysWhenTheServiceIsAwayAndCarriesOn() async throws {
        await controller.settle()
        agent.licenseLinked = false
        controller.beginEntering()
        controller.submit(key)
        await controller.settle()
        XCTAssertTrue(controller.activating)
        try await letWakesStart()
        sleeper.wake()
        try await letWakesStart()
        XCTAssertEqual(controller.keyError, .serviceNotResponding)
        XCTAssertFalse(controller.activating)

        agent.licenseLinked = true
        await polar.queue("activate", try answer("activate-200", 200))
        controller.update()
        await controller.settle()
        XCTAssertFalse(controller.serviceAway)
        XCTAssertEqual(agent.license.state, .licensed)
        XCTAssertFalse(controller.entering)
        XCTAssertNil(controller.keyError)
    }

    func testForgetsAStalledKeyWhenTheLicenseIsRemoved() async throws {
        pending()
        agent.refuseRecords = "license_stale"
        await polar.queue("activate", try answer("activate-200", 200))
        await polar.queue("deactivate", try answer(nil, 204))
        controller.update()
        await controller.settle()
        XCTAssertEqual(store.outbox.stalled.count, 1)

        agent.refuseRecords = nil
        controller.remove()
        await controller.settle()
        XCTAssertEqual(agent.removed, 1)
        XCTAssertTrue(store.outbox.stalled.isEmpty)
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    // MARK: Confirming

    func testConfirmsOnlyWhenDueAndAtMostHourly() async throws {
        licensed(confirmedDaysAgo: 10)
        controller.surfaceAppeared()
        await controller.settle()
        var all = await endpoints()
        XCTAssertEqual(all, [])

        licensed(confirmedDaysAgo: 31)
        controller.update()
        await controller.settle()
        all = await endpoints()
        XCTAssertEqual(all, ["validate"], "Polar could not be reached")
        XCTAssertEqual(agent.license.state, .licensed, "an unreachable Polar changes nothing")

        clock.now = clock.now.addingTimeInterval(30 * 60)
        controller.update()
        await controller.settle()
        all = await endpoints()
        XCTAssertEqual(all.count, 1, "not again within the hour")

        clock.now = clock.now.addingTimeInterval(31 * 60)
        await polar.queue("validate", try answer("validate-200", 200))
        controller.update()
        await controller.settle()
        let sent = await polar.sent
        XCTAssertEqual(sent.count, 2)
        XCTAssertEqual(sent.last?.body["activation_id"], "act-1")
        let validated = try XCTUnwrap(agent.recorded.last)
        XCTAssertEqual(validated.activationID, "act-1")
        XCTAssertEqual(validated.validated.validatedAt, UInt64(clock.now.timeIntervalSince1970))
    }

    func testConfirmsWhenTheServiceComesBackWhileThePageIsOpen() async throws {
        licensed(confirmedDaysAgo: 31)
        agent.licenseLinked = false
        controller.surfaceAppeared()
        await controller.settle()
        var all = await endpoints()
        XCTAssertEqual(all, [])

        agent.licenseLinked = true
        await polar.queue("validate", try answer("validate-200", 200))
        controller.update()
        await controller.settle()
        all = await endpoints()
        XCTAssertEqual(all, ["validate"])

        controller.surfaceDisappeared()
        licensed(confirmedDaysAgo: 40)
        clock.now = clock.now.addingTimeInterval(2 * 3_600)
        controller.update()
        await controller.settle()
        all = await endpoints()
        XCTAssertEqual(all.count, 1, "not once the page is closed")
    }

    func testTwoNotFoundsAMinuteApartMakeItInvalid() async throws {
        licensed(confirmedDaysAgo: 40)
        await polar.queue("validate", try answer("404-not-found", 404))
        controller.surfaceAppeared()
        await controller.settle()
        XCTAssertTrue(agent.recorded.isEmpty, "one 404 is not enough")

        clock.now = clock.now.addingTimeInterval(61)
        await polar.queue("validate", try answer("404-no-longer-active", 404))
        sleeper.wake()
        for _ in 0..<20 { await Task.yield() }
        await controller.settle()
        let refused = try XCTUnwrap(agent.recorded.last)
        XCTAssertEqual(refused.refused, .invalid)
        XCTAssertEqual(refused.activationID, "act-1")
        let all = await endpoints()
        XCTAssertEqual(all, ["validate", "validate"])
    }

    // MARK: Small parts

    func testStampsWithThisMacsClockWhenPolarIsFarAhead() {
        let now = UInt64(clock.now.timeIntervalSince1970)
        XCTAssertEqual(controller.stamp(clock.now.addingTimeInterval(3_600)), now + 3_600)
        XCTAssertEqual(controller.stamp(clock.now.addingTimeInterval(2 * day)), now)
        XCTAssertEqual(controller.stamp(nil), now)
        XCTAssertEqual(controller.stamp(Date(timeIntervalSince1970: 1_000)), now)
    }

    func testWhatThePageShows() {
        XCTAssertEqual(LicenseText.maskedEmail("tasha@example.com"), "t•••@example.com")
        XCTAssertEqual(LicenseText.maskedEmail(""), "")
        XCTAssertEqual(LicenseText.maskedEmail("@example.com"), "")

        var license = LicenseRecord()
        license.state = .licensed
        var pc = Inputweave_App_V1_LicensedDevice()
        pc.id = "pc"; pc.name = "Windows Desktop"; pc.licensed = true
        var mini = Inputweave_App_V1_LicensedDevice()
        mini.id = "mini"; mini.name = "Mac mini"; mini.licensed = false
        license.devices = [pc, mini]
        XCTAssertEqual(LicenseText.computers(license, thisName: "MacBook Pro").map(\.name), ["MacBook Pro", "Windows Desktop"])
        license.state = .seatsFull
        XCTAssertEqual(LicenseText.computers(license, thisName: "MacBook Pro"), [])
    }
}

/// The outbox in the keychain, under a service of the test's own.
@MainActor
final class KeychainLicenseOutboxStoreTests: XCTestCase {
    private let service = "cc.ctsite.inputweave.tests.\(UUID().uuidString)"
    private let suite = "cc.ctsite.inputweave.tests.\(UUID().uuidString)"

    override func tearDown() async throws {
        UserDefaults().removePersistentDomain(forName: suite)
        for account in [KeychainLicenseOutboxStore.account, KeychainLicenseOutboxStore.asideAccount] {
            SecItemDelete(item(account) as CFDictionary)
        }
    }

    private func item(_ account: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword, kSecAttrService as String: service, kSecAttrAccount as String: account]
    }

    func testKeepsTheOutboxInTheKeychainAndDeletesTheDefaultsCopy() async throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        defaults.set(Data("{}".utf8), forKey: Preferences.licenseOutbox)
        let store = KeychainLicenseOutboxStore(service: service, defaults: defaults)
        XCTAssertNil(defaults.data(forKey: Preferences.licenseOutbox), "no keys left in defaults")
        var read = await store.load()
        XCTAssertEqual(read, .kept(LicenseOutbox()))

        var outbox = LicenseOutbox()
        outbox.ops = [.init(kind: .free, key: key, activationID: activationID, fromRemove: true, tries: 1)]
        outbox.stalled = [key: "1|x"]
        var written = await store.save(outbox)
        XCTAssertTrue(written)
        read = await KeychainLicenseOutboxStore(service: service, defaults: defaults).load()
        XCTAssertEqual(read, .kept(outbox), "kept across launches")

        written = await store.save(LicenseOutbox())
        XCTAssertTrue(written)
        read = await store.load()
        XCTAssertEqual(read, .kept(LicenseOutbox()))
    }

    func testSetsAsideAnOutboxItCannotDecode() async throws {
        var bad = item(KeychainLicenseOutboxStore.account)
        bad[kSecValueData as String] = Data("not an outbox".utf8)
        XCTAssertEqual(SecItemAdd(bad as CFDictionary, nil), errSecSuccess)
        let store = KeychainLicenseOutboxStore(service: service, defaults: try XCTUnwrap(UserDefaults(suiteName: suite)))
        let read = await store.load()
        XCTAssertEqual(read, .kept(LicenseOutbox()), "starts over")

        var query = item(KeychainLicenseOutboxStore.asideAccount)
        query[kSecReturnData as String] = true
        var found: CFTypeRef?
        XCTAssertEqual(SecItemCopyMatching(query as CFDictionary, &found), errSecSuccess)
        XCTAssertEqual(found as? Data, Data("not an outbox".utf8), "kept aside, never overwritten")
    }

    func testReadsAnOutboxLeniently() throws {
        let json = #"{"ops":[{"kind":"teleport","key":"k"},{"kind":"free","key":"k","activationID":"a"}],"later":1}"#
        let outbox = try JSONDecoder().decode(LicenseOutbox.self, from: Data(json.utf8))
        XCTAssertEqual(outbox.ops.map(\.kind), [.free], "a kind from a newer build is skipped")
        XCTAssertEqual(outbox.ops.first?.activationID, "a")
        XCTAssertEqual(outbox.ops.first?.tries, 0)
        XCTAssertEqual(outbox.stalled, [:])
    }
}
