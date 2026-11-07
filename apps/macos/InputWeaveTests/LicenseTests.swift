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
    var applied: [String] = []
    var refuseApply: String?
    /// Every RevokeLicense, as device and activation.
    var revokes: [Revoke] = []
    var refuseRevoke: String?
    /// No answer to RevokeLicense.
    var failRevoke = false
    var unpaired: [String] = []
    /// Every LicenseActivationFreed.
    var freed: [String] = []
    /// No answer to LicenseActivationFreed.
    var failFreed = false

    struct Revoke: Equatable {
        var device: String
        var activation: String
        var deactivated = false
    }

    func setLicenseKey(_ key: String) async throws -> LicenseRecord {
        keysSet.append(key)
        guard key.count >= 20 else { throw AgentRefusal(code: "invalid_license_key", detail: "") }
        if license.state == .licensed && license.key == key { return license }
        if license.state == .revoked && license.key == key {
            sameKeyAgain()
            return license
        }
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

    /// Every RemoveLicense, as key and activation.
    var removals: [[String]] = []
    /// The next so many RemoveLicense find the license changed meanwhile,
    /// to `afterStale` when given.
    var staleRemovals = 0
    var afterStale: LicenseRecord?
    /// Devices connected, or taken offline for now.
    var connected: Set<String> = []
    /// The next RemoveLicense finds the record changed meanwhile: the same
    /// key came here again, so the revoked activation is to be freed and
    /// the key waits to be activated anew.
    var keyAgainBeforeRemove = false

    func removeLicense(key: String, activationID: String) async throws {
        removeAttempts += 1
        removals.append([key, activationID])
        if failRemove { throw URLError(.networkConnectionLost) }
        if let refuseRemove { throw AgentRefusal(code: refuseRemove, detail: "") }
        if staleRemovals > 0 {
            staleRemovals -= 1
            if let afterStale { license = afterStale }
            throw AgentRefusal(code: "license_stale", detail: "")
        }
        if keyAgainBeforeRemove {
            keyAgainBeforeRemove = false
            sameKeyAgain()
        }
        if !key.isEmpty, !license.key.isEmpty, license.key != key || license.activationID != activationID {
            throw AgentRefusal(code: "license_stale", detail: "")
        }
        removed += 1
        license = LicenseRecord()
        license.optedOut = true
    }

    /// As the service does when the key revoked here comes again: the
    /// revoked activation is to be freed, never licensed again.
    func sameKeyAgain() {
        let revoked = license.activationID
        license.state = .pendingActivation
        license.activationID = ""
        listToFree(license.key, revoked)
    }

    func applyLicense(deviceID: String) async throws {
        applied.append(deviceID)
        if let refuseApply { throw AgentRefusal(code: refuseApply, detail: "") }
        license.devices.removeAll { $0.id == deviceID }
        var device = LicensedDevice()
        device.id = deviceID
        device.use = .activating
        license.devices.append(device)
    }

    /// Revokes and unpairs, in the order they came.
    var order: [String] = []

    func revokeLicense(deviceID: String, activationID: String, deactivated: Bool) async throws {
        revokes.append(Revoke(device: deviceID, activation: activationID, deactivated: deactivated))
        order.append("revoke \(deviceID)")
        if failRevoke { throw URLError(.networkConnectionLost) }
        if let refuseRevoke { throw AgentRefusal(code: refuseRevoke, detail: "") }
        license.devices.removeAll { $0.id == deviceID }
    }

    func isConnected(_ deviceID: String) -> Bool {
        connected.contains(deviceID)
    }

    func unpair(_ deviceID: String) async throws {
        unpaired.append(deviceID)
        order.append("unpair \(deviceID)")
    }

    func licenseActivationFreed(_ activationID: String) async throws {
        freed.append(activationID)
        if failFreed { throw URLError(.networkConnectionLost) }
        license.toFree.removeAll { $0.activationID == activationID }
    }

    /// Lists an activation of an earlier key to be freed.
    func listToFree(_ key: String, _ activationID: String) {
        var activation = Inputweave_App_V1_LicenseActivation()
        activation.key = key
        activation.activationID = activationID
        license.toFree.append(activation)
    }

    /// Lists a device on this computer's license.
    func list(_ id: String, _ use: LicenseUse, activation: String = "", legacy: Bool = false, canTakeBack: Bool = false) {
        var device = LicensedDevice()
        device.id = id
        device.canTakeBack = canTakeBack
        device.name = id
        device.use = use
        device.activationID = activation
        device.legacy = legacy
        license.devices.append(device)
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
        XCTAssertEqual(agent.removals, [[key, "act-1"]], "the license as it was when the user confirmed")
    }

    func testARemovalKeepsTheFreeOfARevokedActivation() async throws {
        // The key revoked here came again and is licensed anew; the
        // revoked activation's free waits for its next try.
        store.outbox = LicenseOutbox(ops: [
            .init(kind: .free, key: key, activationID: "act-old", revoked: true, tries: 1,
                  notBefore: clock.now.addingTimeInterval(30)),
            .init(kind: .remove, key: key, activationID: "act-old", revoked: true),
        ])
        licensed(confirmedDaysAgo: 1)
        controller = makeController()
        await controller.settle()
        await polar.queue("deactivate", try answer(nil, 204))
        await polar.queue("deactivate", try answer(nil, 204))
        controller.remove()
        XCTAssertTrue(controller.outbox.ops.contains { $0.kind == .free && $0.activationID == "act-old" }, "kept")
        XCTAssertFalse(controller.outbox.ops.contains { $0.kind == .remove && $0.revoked }, "this removal removes the license")
        await controller.settle()
        XCTAssertEqual(agent.removals, [[key, "act-1"]])
        later()
        controller.update()
        await controller.settle()
        let freed = await polar.sent.filter { $0.endpoint == "deactivate" }.compactMap { $0.body["activation_id"] }
        XCTAssertEqual(Set(freed), ["act-1", "act-old"], "both places freed")
        XCTAssertEqual(freed.count, 2)
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    /// The license on record is now another, as another computer applied
    /// one here meanwhile.
    private func anotherLicense() -> LicenseRecord {
        var other = LicenseRecord()
        other.key = "INPUTWEAVE-OTHER-1111-2222-3333"
        other.state = .licensed
        other.activationID = "act-9"
        other.validatedAt = UInt64(clock.now.timeIntervalSince1970)
        return other
    }

    func testARemovalThatMetANewerLicenseRemovesThatOneToo() async throws {
        licensed(confirmedDaysAgo: 1)
        await controller.settle()
        await polar.queue("deactivate", try answer(nil, 204))
        await polar.queue("deactivate", try answer(nil, 204))
        controller.surfaceAppeared()
        controller.remove()
        // Before the removal reaches the service.
        agent.license = anotherLicense()
        await controller.settle()
        XCTAssertEqual(agent.removals, [[key, "act-1"], ["INPUTWEAVE-OTHER-1111-2222-3333", "act-9"]],
                       "the license named, then the one on record")
        XCTAssertEqual(agent.removed, 1)
        XCTAssertFalse(controller.removalPending)
        XCTAssertFalse(controller.removalRefused)
        let freed = await polar.sent.filter { $0.endpoint == "deactivate" }.compactMap { $0.body["activation_id"] }
        XCTAssertEqual(Set(freed), ["act-1", "act-9"], "both seats freed")
        XCTAssertEqual(freed.count, 2)
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    func testARemovalThatKeepsMeetingANewerLicenseGivesUpAndSaysSo() async throws {
        licensed(confirmedDaysAgo: 1)
        await controller.settle()
        agent.staleRemovals = 3
        agent.afterStale = anotherLicense()
        await polar.queue("deactivate", try answer(nil, 204))
        controller.surfaceAppeared()
        controller.remove()
        await controller.settle()
        XCTAssertEqual(agent.removals, [[key, "act-1"], ["INPUTWEAVE-OTHER-1111-2222-3333", "act-9"],
                                        ["INPUTWEAVE-OTHER-1111-2222-3333", "act-9"]], "three times in all")
        XCTAssertTrue(controller.removalRefused, "as for any other refusal")
        XCTAssertFalse(controller.removalPending)
        XCTAssertTrue(controller.isLicensed, "the license on record, and its seat, stay")
        let freed = await polar.sent.filter { $0.endpoint == "deactivate" }.compactMap { $0.body["activation_id"] }
        XCTAssertEqual(freed, ["act-1"], "only the one no longer on record")
        later(3_600)
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.removeAttempts, 3, "not again")
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    func testARemovalThatFindsNoLicenseLeftIsDone() async throws {
        licensed(confirmedDaysAgo: 1)
        await controller.settle()
        agent.staleRemovals = 1
        agent.afterStale = LicenseRecord()
        await polar.queue("deactivate", try answer(nil, 204))
        controller.surfaceAppeared()
        controller.remove()
        await controller.settle()
        XCTAssertEqual(agent.removals, [[key, "act-1"]])
        XCTAssertFalse(controller.removalRefused)
        XCTAssertFalse(controller.removalPending)
        let freed = await polar.sent.filter { $0.endpoint == "deactivate" }.compactMap { $0.body["activation_id"] }
        XCTAssertEqual(freed, ["act-1"], "its seat is still freed")
        XCTAssertTrue(store.outbox.ops.isEmpty)
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

    // MARK: Applying and revoking on other devices

    func testApplyTellsTheServiceAndNeverPolar() async throws {
        licensed(confirmedDaysAgo: 1)
        await controller.settle()
        await controller.apply("pc")
        XCTAssertEqual(agent.applied, ["pc"])
        XCTAssertEqual(agent.license.devices.first?.use, .activating, "the row shows what the service says")
        XCTAssertTrue(controller.applying.isEmpty)

        agent.refuseApply = "license_full"
        await controller.apply("mini")
        XCTAssertEqual(agent.applied, ["pc", "mini"], "a refusal is only logged")
        XCTAssertTrue(controller.applying.isEmpty)
        let all = await endpoints()
        XCTAssertEqual(all, [])
        XCTAssertTrue(store.outbox.ops.isEmpty, "nothing queued")
    }

    func testDontApplyTakesItBackAtOnceWithoutPolar() async throws {
        licensed(confirmedDaysAgo: 1)
        agent.list("pc", .waiting)
        agent.list("mini", .activating)
        await controller.settle()
        await controller.dontApply("pc")
        await controller.dontApply("mini")
        XCTAssertEqual(agent.revokes, [.init(device: "pc", activation: ""), .init(device: "mini", activation: "")])
        let all = await endpoints()
        XCTAssertEqual(all, [])
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    /// Revoke License on the PC, licensed as `act-pc`, with Polar answering
    /// `answer` (nil: no network); what the service was told.
    private func revokeThePC(_ answer: PolarAnswer?) async throws -> FakeAgent.Revoke? {
        agent = FakeAgent()
        polar = ScriptedPolar()
        store = MemoryLicenseOutboxStore()
        controller = makeController()
        licensed(confirmedDaysAgo: 1)
        agent.list("pc", .licensed, activation: "act-pc")
        await controller.settle()
        if let answer { await polar.queue("deactivate", answer) }
        await controller.revoke("pc", activationID: "act-pc")
        await controller.settle()
        return agent.revokes.last
    }

    func testRevokeAsksPolarOnceAndTellsTheServiceWhetherItDeactivated() async throws {
        var told = try await revokeThePC(try answer(nil, 204))
        XCTAssertEqual(told, .init(device: "pc", activation: "act-pc", deactivated: true))
        let sent = await polar.sent
        XCTAssertEqual(sent.map(\.endpoint), ["deactivate"])
        XCTAssertEqual(sent.first?.body["activation_id"], "act-pc")
        XCTAssertEqual(sent.first?.body["key"], key, "with this computer's key")
        XCTAssertTrue(store.outbox.ops.isEmpty)
        XCTAssertTrue(controller.isLicensed, "this computer keeps its license")
        XCTAssertFalse(controller.seatNotFreedYet)
        XCTAssertFalse(controller.removing)

        told = try await revokeThePC(try answer("404-not-found", 404))
        XCTAssertEqual(told, .init(device: "pc", activation: "act-pc", deactivated: true), "404: already gone")

        let failures: [(String, PolarAnswer?)] = [
            ("unreachable", nil), ("refused", try answer("activate-403-revoked", 403)),
            ("unreadable", try answer("422-validation", 422)),
            ("rate limited", try answer("429-rate-limited", 429, retryAfter: "300")),
        ]
        for (what, failure) in failures {
            told = try await revokeThePC(failure)
            XCTAssertEqual(told, .init(device: "pc", activation: "act-pc", deactivated: false), what)
            let count = await polar.sent.count
            XCTAssertEqual(count, 1, "asked once: \(what)")
            later(3_600)
            controller.update()
            await controller.settle()
            let again = await polar.sent.count
            XCTAssertEqual(again, 1, "never asked again; the PC frees it itself: \(what)")
            XCTAssertTrue(store.outbox.ops.isEmpty, what)
        }
    }

    func testRevokeAsksNothingOfPolarWhileItAskedToWait() async throws {
        licensed(confirmedDaysAgo: 1)
        agent.list("pc", .licensed, activation: "act-pc")
        agent.list("mini", .licensed, activation: "act-mini")
        await controller.settle()
        await polar.queue("deactivate", try answer("429-rate-limited", 429, retryAfter: "300"))
        await controller.revoke("pc", activationID: "act-pc")
        await controller.settle()
        XCTAssertNotNil(store.outbox.polarNotBefore, "kept across launches")
        controller = makeController()
        await controller.settle()
        later(120)
        await controller.revoke("mini", activationID: "act-mini")
        await controller.settle()
        let all = await endpoints()
        XCTAssertEqual(all, ["deactivate"], "Polar said when")
        XCTAssertEqual(agent.revokes.last, .init(device: "mini", activation: "act-mini", deactivated: false))

        later(200)
        await polar.queue("deactivate", try answer(nil, 204))
        agent.list("studio", .licensed, activation: "act-studio")
        await controller.revoke("studio", activationID: "act-studio")
        await controller.settle()
        XCTAssertEqual(agent.revokes.last, .init(device: "studio", activation: "act-studio", deactivated: true))
    }

    func testARevokeIsKeptAcrossARestartWithWhatPolarSaid() async throws {
        licensed(confirmedDaysAgo: 1)
        agent.list("pc", .licensed, activation: "act-pc")
        await controller.settle()
        agent.licenseLinked = false
        await polar.queue("deactivate", try answer(nil, 204))
        await controller.revoke("pc", activationID: "act-pc")
        await controller.revoke("pc", activationID: "act-pc")
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.revoke], "kept once, however often asked")
        XCTAssertEqual(store.outbox.ops.first?.deviceID, "pc")
        XCTAssertEqual(store.outbox.ops.first?.activationID, "act-pc")
        XCTAssertEqual(store.outbox.ops.first?.deactivated, true)
        XCTAssertTrue(controller.revoking("pc"))
        XCTAssertTrue(agent.revokes.isEmpty)
        XCTAssertFalse(controller.seatNotFreedYet, "not this computer's place")
        XCTAssertFalse(controller.removalPending)

        controller = makeController()
        agent.licenseLinked = true
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.revokes, [.init(device: "pc", activation: "act-pc", deactivated: true)])
        let all = await endpoints()
        XCTAssertEqual(all, ["deactivate"], "Polar was asked before it was kept, and only then")
        XCTAssertTrue(store.outbox.ops.isEmpty)
        XCTAssertFalse(controller.revoking("pc"))
    }

    func testARevokeTheServiceRefusesForGoodIsDroppedAndATemporaryOneKept() async throws {
        licensed(confirmedDaysAgo: 1)
        agent.list("pc", .licensed, activation: "act-pc")
        await controller.settle()
        await polar.queue("deactivate", try answer(nil, 204))
        agent.refuseRevoke = "license_unavailable"
        await controller.revoke("pc", activationID: "act-pc")
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.revoke], "only for now: tried again")

        agent.refuseRevoke = "license_stale"
        later()
        controller.update()
        await controller.settle()
        XCTAssertTrue(store.outbox.ops.isEmpty, "dropped: the PC's activation changed meanwhile")
        XCTAssertEqual(agent.revokes.count, 2)
        let all = await endpoints()
        XCTAssertEqual(all, ["deactivate"])
    }

    func testARevokeMadeBeforeTheOutboxCouldBeReadIsKeptWithIt() async throws {
        licensed(confirmedDaysAgo: 1)
        store.outbox = LicenseOutbox(ops: [.init(kind: .free, key: "INPUTWEAVE-OTHER-0000-0000", activationID: "old", tries: 1,
                                                 notBefore: clock.now.addingTimeInterval(600))])
        store.unreadable = true
        agent.licenseLinked = false
        controller = makeController()
        await controller.settle()
        XCTAssertEqual(controller.loading, .unreadable)
        store.unreadable = false
        await polar.queue("deactivate", try answer(nil, 204))
        await controller.revoke("pc", activationID: "act-pc")
        await controller.settle()
        XCTAssertEqual(controller.loading, .loaded, "read again")
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.free, .revoke], "kept with what was kept before")
        XCTAssertEqual(store.outbox.ops.last?.deactivated, true)
    }

    // MARK: A license revoked here

    private func revokedHere() {
        agent.license.key = key
        agent.license.state = .revoked
        agent.license.activationID = "act-1"
        agent.license.sourceDeviceID = "pc"
    }

    func testARevokedLicenseIsFreedFirstThenRemovedWithoutAWord() async throws {
        await controller.settle()
        revokedHere()
        agent.failRemove = true
        await polar.queue("deactivate", try answer(nil, 204))
        controller.update()
        XCTAssertEqual(controller.outbox.ops.map(\.kind), [.free, .remove])
        XCTAssertFalse(controller.removing, "the user here did nothing")
        XCTAssertFalse(controller.removalPending)
        XCTAssertFalse(controller.isLicensed)
        await controller.settle()
        let sent = await polar.sent
        XCTAssertEqual(sent.map(\.endpoint), ["deactivate"])
        XCTAssertEqual(sent.last?.body["activation_id"], "act-1")
        XCTAssertEqual(agent.removeAttempts, 1, "asked once the activation is freed")
        XCTAssertEqual(agent.removals.first, [key, "act-1"], "the license revoked here, and only it")
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.remove])
        XCTAssertFalse(controller.removalPending, "still nothing said")

        agent.failRemove = false
        later()
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.removed, 1)
        XCTAssertTrue(store.outbox.ops.isEmpty)
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.removeAttempts, 2)
    }

    func testARevokedLicenseIsNotRemovedBeforePolarFreedItsActivation() async throws {
        await controller.settle()
        revokedHere()
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.removeAttempts, 0, "Polar could not be reached")
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.free, .remove], "kept, and so the record stays revoked")
        XCTAssertFalse(controller.seatNotFreedYet, "no notice for it")
        controller.update()
        agent.license.devices = [LicensedDevice()]
        controller.update()
        XCTAssertEqual(controller.outbox.ops.filter { $0.kind == .remove }.count, 1, "not again while it is queued")

        // After a restart the free goes on, then the removal.
        controller = makeController()
        later()
        await polar.queue("deactivate", try answer("404-not-found", 404))
        controller.update()
        await controller.settle()
        let all = await endpoints()
        XCTAssertEqual(all, ["deactivate", "deactivate"])
        XCTAssertEqual(agent.removed, 1, "404: freed already")
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    func testARevokedActivationIsFreedEvenBeforeTheServiceAnswersAfterALaunch() async throws {
        store.outbox = LicenseOutbox(ops: [
            .init(kind: .free, key: key, activationID: "act-1", revoked: true),
            .init(kind: .remove, key: key, activationID: "act-1", revoked: true),
        ])
        agent.licenseLinked = false
        await polar.queue("deactivate", try answer(nil, 204))
        controller = makeController()
        controller.update()
        await controller.settle()
        let all = await endpoints()
        XCTAssertEqual(all, ["deactivate"], "it is never licensed again")
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.remove], "the removal waits for the service")

        revokedHere()
        agent.licenseLinked = true
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.removals, [[key, "act-1"]])
        XCTAssertEqual(agent.removed, 1)
    }

    func testTheSameKeyAgainFreesTheRevokedActivationOnceAndActivatesAnew() async throws {
        await controller.settle()
        revokedHere()
        controller.update()
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.free, .remove], "Polar could not be reached")
        await polar.queue("activate", try answer("activate-200", 200))
        controller.beginEntering()
        controller.submit(key)
        await controller.settle()
        XCTAssertEqual(agent.keysSet, [key], "the key does not wait behind the removal")
        XCTAssertEqual(agent.license.state, .licensed)
        XCTAssertEqual(agent.license.activationID, activationID, "activated anew")
        XCTAssertNil(controller.keyError)
        XCTAssertFalse(controller.entering)

        later()
        await polar.queue("deactivate", try answer(nil, 204))
        controller.update()
        await controller.settle()
        let sent = await polar.sent
        XCTAssertEqual(sent.filter { $0.endpoint == "deactivate" }.map { $0.body["activation_id"] }, ["act-1", "act-1"],
                       "the try from before, then the one that freed it")
        XCTAssertEqual(agent.freed, ["act-1"], "said once")
        XCTAssertEqual(agent.removeAttempts, 0)
        XCTAssertTrue(store.outbox.ops.isEmpty)
        XCTAssertTrue(controller.isLicensed)
        controller.update()
        await controller.settle()
        let again = await polar.sent.count
        XCTAssertEqual(again, sent.count, "never freed twice")
    }

    func testAStaleRemovalOfARevokedLicenseSaysItsActivationWasFreed() async throws {
        await controller.settle()
        revokedHere()
        agent.keyAgainBeforeRemove = true
        await polar.queue("deactivate", try answer(nil, 204))
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.removals, [[key, "act-1"]])
        XCTAssertEqual(agent.removed, 0)
        XCTAssertEqual(agent.freed, ["act-1"], "once")
        XCTAssertTrue(agent.license.toFree.isEmpty)
        let all = await endpoints()
        XCTAssertEqual(all.filter { $0 == "deactivate" }.count, 1, "never freed twice")
        XCTAssertTrue(store.outbox.ops.filter { $0.kind != .activate }.isEmpty)
        XCTAssertFalse(controller.removalPending)
        XCTAssertFalse(controller.removalRefused)
    }

    func testARevokedActivationIsFreedOnceWhenAnotherKeyIsEnteredAndTheServiceIsToldSo() async throws {
        await controller.settle()
        revokedHere()
        await polar.queue("deactivate", try answer(nil, 204))
        await polar.hold("deactivate")
        controller.update()
        while await polar.sent.isEmpty { await Task.yield() }
        // Another key, licensed meanwhile; the service lists the activation
        // revoked here to be freed.
        agent.license = LicenseRecord()
        agent.license.key = "INPUTWEAVE-OTHER-1111-2222-3333"
        agent.license.state = .licensed
        agent.license.activationID = "act-9"
        agent.license.validatedAt = UInt64(clock.now.timeIntervalSince1970)
        agent.listToFree(key, "act-1")
        controller.update()
        XCTAssertEqual(controller.outbox.ops.filter { $0.kind == .free }.count, 1, "the free under way is the one")
        await polar.release()
        await controller.settle()
        let all = await endpoints()
        XCTAssertEqual(all, ["deactivate"])
        XCTAssertEqual(agent.freed, ["act-1"])
        XCTAssertEqual(agent.removeAttempts, 0, "the other key stays")
        XCTAssertTrue(store.outbox.ops.isEmpty)
        XCTAssertTrue(controller.isLicensed)
    }

    func testARevokedActivationFreedAfterTheRecordMovedOnIsLeftAtThat() async throws {
        await controller.settle()
        revokedHere()
        await polar.queue("deactivate", try answer(nil, 204))
        await polar.hold("deactivate")
        controller.update()
        while await polar.sent.isEmpty { await Task.yield() }
        agent.license = LicenseRecord()
        controller.update()
        await polar.release()
        await controller.settle()
        XCTAssertEqual(agent.removeAttempts, 0)
        XCTAssertEqual(agent.freed, [], "not listed to be freed: nothing more")
        XCTAssertTrue(store.outbox.ops.isEmpty)
    }

    // MARK: Activations of an earlier key

    func testFreesAnEarlierActivationAcrossARestartAndTellsTheService() async throws {
        await controller.settle()
        licensed(confirmedDaysAgo: 1)
        agent.listToFree("INPUTWEAVE-OLD-0000-0000-0000", "act-old")
        controller.update()
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.free], "Polar could not be reached; kept")
        XCTAssertEqual(store.outbox.ops.first?.toFree, true)
        controller.update()
        XCTAssertEqual(controller.outbox.ops.count, 1, "queued once")
        let sent = await polar.sent
        XCTAssertEqual(sent.first?.body["key"], "INPUTWEAVE-OLD-0000-0000-0000", "with its own key")
        XCTAssertEqual(sent.first?.body["activation_id"], "act-old")

        // Whatever the license is now.
        agent.license.state = .seatsFull
        controller = makeController()
        later()
        await polar.queue("deactivate", try answer("404-not-found", 404))
        controller.update()
        await controller.settle()
        let all = await endpoints()
        XCTAssertEqual(all, ["deactivate", "deactivate"])
        XCTAssertEqual(agent.freed, ["act-old"], "404: gone already")
        XCTAssertTrue(store.outbox.ops.isEmpty)
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.freed.count, 1, "the service forgot it")
    }

    func testTellsTheServiceOfAFreedActivationOnceItIsBack() async throws {
        await controller.settle()
        agent.listToFree("INPUTWEAVE-OLD-0000-0000-0000", "act-old")
        agent.failFreed = true
        await polar.queue("deactivate", try answer(nil, 204))
        controller.update()
        await controller.settle()
        XCTAssertEqual(store.outbox.ops.map(\.kind), [.freed], "freed, and kept until the service hears of it")
        agent.failFreed = false
        later()
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.freed, ["act-old", "act-old"])
        XCTAssertTrue(store.outbox.ops.isEmpty)
        let all = await endpoints()
        XCTAssertEqual(all, ["deactivate"], "Polar is not asked again")
    }

    func testAnEarlierActivationWaitsForPolarsRetryAfter() async throws {
        store.outbox = LicenseOutbox(polarNotBefore: clock.now.addingTimeInterval(300))
        controller = makeController()
        await controller.settle()
        agent.listToFree("INPUTWEAVE-OLD-0000-0000-0000", "act-old")
        controller.update()
        await controller.settle()
        var all = await endpoints()
        XCTAssertEqual(all, [])
        later(301)
        await polar.queue("deactivate", try answer(nil, 204))
        controller.update()
        await controller.settle()
        all = await endpoints()
        XCTAssertEqual(all, ["deactivate"])
        XCTAssertEqual(agent.freed, ["act-old"])
    }

    func testARevokedLicenseTheServiceRefusesToRemoveIsNotAskedAgainForTheSameRecord() async throws {
        revokedHere()
        agent.refuseRemove = "license_unsupported"
        await polar.queue("deactivate", try answer(nil, 204))
        controller.surfaceAppeared()
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.removeAttempts, 1)
        XCTAssertFalse(controller.removalRefused, "said of the user's own Remove License only")
        XCTAssertTrue(store.outbox.ops.isEmpty)
        controller = makeController()
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.removeAttempts, 1, "not again, even after a restart")

        agent.refuseRemove = nil
        agent.license.activationID = "act-2"
        await polar.queue("deactivate", try answer(nil, 204))
        controller.update()
        await controller.settle()
        XCTAssertEqual(agent.removed, 1, "a record that changed is removed")
    }

    // MARK: Removing a device

    /// Removes the PC, licensed as `act-pc` (none when `activation` is
    /// empty), with Polar answering `answer` (nil: no network); the notice
    /// it shows.
    private func removeThePC(_ answer: PolarAnswer?, activation: String = "act-pc") async throws -> LicenseText.RemoveNotice? {
        agent = FakeAgent()
        polar = ScriptedPolar()
        controller = makeController()
        licensed(confirmedDaysAgo: 1)
        agent.list("pc", .licensed, activation: activation)
        if let answer { await polar.queue("deactivate", answer) }
        return await controller.removeDevice("pc", revoking: true)
    }

    func testRemovingADeviceAsksPolarOnceAndTellsTheServiceWhetherItDeactivated() async throws {
        var notice = try await removeThePC(try answer(nil, 204))
        XCTAssertNil(notice)
        XCTAssertEqual(agent.revokes, [.init(device: "pc", activation: "act-pc", deactivated: true)])
        XCTAssertEqual(agent.unpaired, ["pc"])
        let sent = await polar.sent
        XCTAssertEqual(sent.map(\.endpoint), ["deactivate"])
        XCTAssertEqual(sent.first?.body["activation_id"], "act-pc")
        XCTAssertEqual(sent.first?.timeout, PolarClient.timeout)
        XCTAssertTrue(store.outbox.ops.isEmpty, "asked once, never queued")

        notice = try await removeThePC(try answer("404-not-found", 404))
        XCTAssertNil(notice, "404: already gone")
        XCTAssertEqual(agent.revokes, [.init(device: "pc", activation: "act-pc", deactivated: true)])
    }

    func testRemovingADeviceWhosePlaceIsNotFreedStillRemovesItAndSaysSo() async throws {
        let failures: [(String, PolarAnswer?)] = [
            ("unreachable", nil), ("refused", try answer("activate-403-revoked", 403)),
            ("rate limited", try answer("429-rate-limited", 429, retryAfter: "5")),
        ]
        for (what, failure) in failures {
            let notice = try await removeThePC(failure)
            XCTAssertEqual(notice, .notDeactivated, what)
            XCTAssertEqual(agent.revokes, [.init(device: "pc", activation: "act-pc", deactivated: false)],
                           "the PC frees it itself: \(what)")
            XCTAssertEqual(agent.unpaired, ["pc"], what)
            let count = await polar.sent.count
            XCTAssertEqual(count, 1, "once: \(what)")
        }

        // Licensed without an activation id: nothing to free with.
        let notice = try await removeThePC(nil, activation: "")
        XCTAssertEqual(notice, .notDeactivated)
        XCTAssertEqual(agent.revokes, [.init(device: "pc", activation: "")])
        let all = await endpoints()
        XCTAssertEqual(all, [])
    }

    func testRemovingADeviceWhilePolarAskedToWaitAsksNothingOfIt() async throws {
        store.outbox = LicenseOutbox(polarNotBefore: clock.now.addingTimeInterval(300))
        controller = makeController()
        licensed(confirmedDaysAgo: 1)
        agent.list("pc", .licensed, activation: "act-pc")
        await controller.settle()
        let notice = await controller.removeDevice("pc", revoking: true)
        XCTAssertEqual(notice, .notDeactivated)
        XCTAssertEqual(agent.revokes, [.init(device: "pc", activation: "act-pc", deactivated: false)])
        XCTAssertEqual(agent.unpaired, ["pc"])
        let all = await endpoints()
        XCTAssertEqual(all, [])
    }

    func testRemovingADeviceActivatingOrWaitingTakesTheKeyBack() async throws {
        licensed(confirmedDaysAgo: 1)
        agent.list("pc", .activating)
        agent.list("mini", .waiting)
        agent.list("studio", .waiting)
        agent.license.devices[2].keyOutOfReach = true
        var notice = await controller.removeDevice("pc", revoking: true)
        XCTAssertNil(notice)
        notice = await controller.removeDevice("mini", revoking: true)
        XCTAssertNil(notice, "the key never reached it")
        notice = await controller.removeDevice("studio", revoking: true)
        XCTAssertEqual(notice, .outOfReach, "it may hold the key, and is gone before it hears it is taken back")
        XCTAssertEqual(agent.revokes, [
            .init(device: "pc", activation: ""), .init(device: "mini", activation: ""), .init(device: "studio", activation: ""),
        ])
        XCTAssertEqual(agent.unpaired, ["pc", "mini", "studio"])
        let all = await endpoints()
        XCTAssertEqual(all, [])
    }

    func testRemovingADeviceWithoutRevokingOnlyUnpairsIt() async throws {
        licensed(confirmedDaysAgo: 1)
        agent.list("pc", .licensed, activation: "act-pc")
        let notice = await controller.removeDevice("pc", revoking: false)
        XCTAssertNil(notice)
        XCTAssertTrue(agent.revokes.isEmpty)
        XCTAssertEqual(agent.unpaired, ["pc"])
        let all = await endpoints()
        XCTAssertEqual(all, [])
    }

    func testRemovingADeviceSaysTheLicenseWasNotRevokedWhenTheServiceDidNotTakeIt() async throws {
        let failures: [(String, String?)] = [
            ("no answer", nil), ("unavailable", "license_unavailable"), ("stale", "license_stale"),
            ("1.0", "license_legacy"), ("other", "a_code_from_a_newer_service"),
        ]
        for (what, code) in failures {
            agent = FakeAgent()
            polar = ScriptedPolar()
            controller = makeController()
            licensed(confirmedDaysAgo: 1)
            agent.list("pc", .licensed, activation: "act-pc")
            agent.list("laptop", .licensed, activation: "act-laptop")
            agent.list("mini", .waiting, canTakeBack: true)
            agent.list("studio", .waiting, canTakeBack: true)
            agent.license.devices[3].keyOutOfReach = true
            agent.failRevoke = code == nil
            agent.refuseRevoke = code
            await polar.queue("deactivate", try answer(nil, 204))
            var notice = await controller.removeDevice("pc", revoking: true)
            XCTAssertEqual(notice, .notTold, "its place is free, but it was not told: \(what)")
            // Polar can't be reached for the laptop.
            notice = await controller.removeDevice("laptop", revoking: true)
            XCTAssertEqual(notice, .notDeactivated, "neither freed nor told: \(what)")
            notice = await controller.removeDevice("mini", revoking: true)
            XCTAssertEqual(notice, .notDeactivated, "not taken back: \(what)")
            notice = await controller.removeDevice("studio", revoking: true)
            XCTAssertEqual(notice, .outOfReach, what)
            XCTAssertEqual(agent.order, [
                "revoke pc", "unpair pc", "revoke laptop", "unpair laptop", "revoke mini", "unpair mini", "revoke studio",
                "unpair studio",
            ], "told first, then removed whatever it answered: \(what)")
        }
    }

    func testRemovingADeviceThatNeverSaidItsActivationIsRevokedWhileItIsConnected() async throws {
        licensed(confirmedDaysAgo: 1)
        agent.list("pc", .licensed)
        agent.list("mini", .licensed)
        agent.list("studio", .licensed)
        agent.connected = ["pc", "studio"]
        var notice = await controller.removeDevice("pc", revoking: true)
        XCTAssertNil(notice, "the service tells it while it is connected")
        notice = await controller.removeDevice("mini", revoking: true)
        XCTAssertEqual(notice, .notDeactivated, "not connected: it may never hear it")
        agent.refuseRevoke = "license_unavailable"
        notice = await controller.removeDevice("studio", revoking: true)
        XCTAssertEqual(notice, .notDeactivated, "refused")
        XCTAssertEqual(agent.revokes, [
            .init(device: "pc", activation: ""), .init(device: "mini", activation: ""), .init(device: "studio", activation: ""),
        ])
        let all = await endpoints()
        XCTAssertEqual(all, [], "nothing to deactivate with")
    }

    func testRemovingADeviceTheServiceTookTheRevokeOfKeepsItsNotice() async throws {
        licensed(confirmedDaysAgo: 1)
        agent.list("pc", .licensed, activation: "act-pc")
        agent.list("mini", .waiting, canTakeBack: true)
        await polar.queue("deactivate", try answer(nil, 204))
        var notice = await controller.removeDevice("pc", revoking: true)
        XCTAssertNil(notice, "its place is free")
        notice = await controller.removeDevice("mini", revoking: true)
        XCTAssertNil(notice)
        XCTAssertEqual(agent.order, ["revoke pc", "unpair pc", "revoke mini", "unpair mini"])
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

    func testKeepsARevokeAQuietRemovalAndPolarsWait() throws {
        let outbox = LicenseOutbox(
            ops: [
                .init(kind: .revoke, key: key, activationID: "act-pc", deviceID: "pc", deactivated: true),
                .init(kind: .free, key: key, activationID: "act-1", revoked: true),
                .init(kind: .remove, key: key, activationID: "act-1", revoked: true),
                .init(kind: .free, key: "INPUTWEAVE-OLD-0000-0000-0000", activationID: "act-old", toFree: true),
                .init(kind: .freed, key: "INPUTWEAVE-OLD-0000-0000-0000", activationID: "act-older"),
            ],
            polarNotBefore: Date(timeIntervalSince1970: 1_793_800_300))
        let read = try JSONDecoder().decode(LicenseOutbox.self, from: JSONEncoder().encode(outbox))
        XCTAssertEqual(read, outbox)
        XCTAssertEqual(read.kept, outbox)
        XCTAssertTrue(read.ops[0].isService)
        XCTAssertFalse(read.ops[0].needsKeeping, "Polar was asked already; nothing waits on the keychain")
        XCTAssertTrue(read.ops[4].isService)
        XCTAssertFalse(read.ops[3].isService)
    }

    func testReadsAnOutboxLeniently() throws {
        let json = #"{"ops":[{"kind":"teleport","key":"k"},{"kind":"free","key":"k","activationID":"a"}],"later":1}"#
        let outbox = try JSONDecoder().decode(LicenseOutbox.self, from: Data(json.utf8))
        XCTAssertEqual(outbox.ops.map(\.kind), [.free], "a kind from a newer build is skipped")
        XCTAssertEqual(outbox.ops.first?.activationID, "a")
        XCTAssertEqual(outbox.ops.first?.tries, 0)
        XCTAssertEqual(outbox.ops.first?.deviceID, "")
        XCTAssertEqual(outbox.ops.first?.deactivated, false)
        XCTAssertEqual(outbox.ops.first?.revoked, false, "a removal from before was the user's")
        XCTAssertEqual(outbox.ops.first?.toFree, false)
        XCTAssertEqual(outbox.stalled, [:])
        XCTAssertNil(outbox.polarNotBefore)
    }
}

/// What "Computers on this license", the trial page and Remove Device show.
final class LicenseTextTests: XCTestCase {
    private func device(_ id: String, connected: Bool = true, offlineForNow: Bool = false) -> DeviceStatus {
        var device = DeviceStatus()
        device.id = id
        device.name = id.uppercased()
        device.os = "windows"
        device.paired = true
        device.state = connected ? .connected : .offline
        device.offlineForNow = offlineForNow
        return device
    }

    private func entry(
        _ id: String, _ use: LicenseUse, activation: String = "", legacy: Bool = false, canTakeBack: Bool = false,
        refused: Bool = false, waitingToApply: Bool = false
    ) -> LicensedDevice {
        var entry = LicensedDevice()
        entry.id = id
        entry.name = id.uppercased()
        entry.use = use
        entry.activationID = activation
        entry.legacy = legacy
        entry.canTakeBack = canTakeBack
        entry.legacyRefused = refused
        entry.waitingToApply = waitingToApply
        return entry
    }

    private func status(_ devices: [DeviceStatus], _ entries: [LicensedDevice]) -> AgentStatus {
        var status = AgentStatus()
        status.thisDeviceID = "mac"
        status.thisDeviceName = "MacBook Pro"
        status.offlineSupported = true
        status.devices = devices
        status.license.state = .licensed
        status.license.applySupported = true
        status.license.devices = entries
        return status
    }

    func testEachRowSaysHowTheComputerStandsAndWhatCanBeDone() {
        let rows = LicenseText.rows(status(
            [
                device("pc"), device("mini", connected: false), device("laptop"), device("studio", connected: false),
                device("imac"), device("desk"), device("old"), device("lounge", connected: false, offlineForNow: true),
                device("odd"),
            ],
            [
                entry("pc", .licensed, activation: "act-pc"), entry("laptop", .activating),
                entry("studio", .waiting, canTakeBack: true),
                entry("imac", .noPlaces), entry("desk", .otherLicense), entry("old", .licensed, legacy: true),
                entry("lounge", .licensed, activation: "act-lounge"), entry("odd", .UNRECOGNIZED(9)),
                // Not trusted: not listed.
                entry("gone", .licensed, activation: "act-gone"),
            ]), thisName: "MacBook Pro")
        XCTAssertEqual(rows.map(\.id), ["mac", "pc", "mini", "laptop", "studio", "imac", "desk", "old", "lounge", "odd"])
        XCTAssertEqual(rows.map(\.standing), [
            .thisComputer, .licensed, .trial, .activating, .waiting, .noPlaces, .otherLicense, .licensed, .licensed, .trial,
        ])
        XCTAssertEqual(rows.map(\.action), [.none, .revoke, .apply, .none, .dontApply, .apply, .none, .none, .revoke, .apply],
                       "a computer on InputWeave 1.0 can't be told of a revoke")
        XCTAssertEqual(rows.map(\.reach), [
            .thisComputer, .connected, .offline, .connected, .offline, .connected, .connected, .connected, .offlineForNow,
            .connected,
        ])
        XCTAssertEqual(rows.map(\.showsLegacyNote), [false, false, false, false, false, false, false, true, false, false])
        XCTAssertEqual(rows[1].activationID, "act-pc")
        XCTAssertEqual(rows[0].name, "MacBook Pro")
        XCTAssertEqual(rows[0].os, "macos")
        // This computer, the PC, the old MacBook and the lounge PC are
        // licensed; the laptop and the studio PC are promised a place too.
        XCTAssertEqual(LicenseText.licensedCount(rows), 4)
        XCTAssertTrue(LicenseText.allPlacesUsed(rows))
        XCTAssertTrue(LicenseText.noPlaces(rows))
    }

    func testDontApplyTakesBackOnlyWhatCanBeTakenBack() {
        let rows = LicenseText.rows(status(
            ["a", "b", "c", "d"].map { device($0) },
            [entry("a", .activating, canTakeBack: true), entry("b", .activating), entry("c", .waiting, canTakeBack: true),
             entry("d", .waiting)]), thisName: "Mac")
        XCTAssertEqual(rows.dropFirst().map(\.action), [.dontApply, .none, .dontApply, .none],
                       "an activating key the user entered there stays")
        XCTAssertEqual(rows.dropFirst().map(\.standing), [.activating, .activating, .waiting, .waiting])
    }

    func testAComputerOn10ThatHeardTheKeyCantBeTakenBack() {
        let rows = LicenseText.rows(status(
            ["licensed", "activating", "waiting", "untold", "refused", "new"].map { device($0) },
            [entry("licensed", .licensed, legacy: true), entry("activating", .activating, legacy: true),
             entry("waiting", .waiting, legacy: true), entry("untold", .waiting, legacy: true, canTakeBack: true),
             entry("refused", .none, legacy: true, refused: true), entry("new", .activating, canTakeBack: true)]),
            thisName: "Mac").dropFirst()
        XCTAssertEqual(rows.map(\.showsLegacyNote), [true, true, true, false, false, false])
        XCTAssertEqual(rows.map(\.action), [.none, .none, .none, .dontApply, .apply, .dontApply],
                       "the key never went to the untold one: it can still be taken back")
        XCTAssertEqual(rows.map(\.showsLegacyRefused), [false, false, false, false, true, false])
        XCTAssertEqual(rows.map(\.standing)[4], .trial, "Trial and Apply License, as any other")
        XCTAssertEqual(LicenseText.removeOffer("activating", license: status([], [entry("activating", .activating, legacy: true)]).license,
                                               licensedHere: true), .legacy)
    }

    func testListsWhatWaitsToBeAppliedWhileThisComputerIsNotLicensed() {
        var waiting = status(
            [device("pc"), device("mini", connected: false), device("old"), device("holder", connected: false),
             device("laptop")],
            [entry("pc", .waiting, canTakeBack: true, waitingToApply: true),
             entry("mini", .waiting, canTakeBack: true, waitingToApply: true),
             entry("old", .waiting, legacy: true, waitingToApply: true),
             // Offline with a key it holds already: never waiting to apply.
             entry("holder", .waiting), entry("laptop", .licensed, activation: "a")])
        // Licensed: while this computer removes its own license.
        for state in [Inputweave_App_V1_LicenseState.seatsFull, .invalid, .none, .revoked, .licensed] {
            waiting.license.state = state
            let rows = LicenseText.waitingToApply(waiting)
            XCTAssertEqual(rows.map(\.id), ["pc", "mini", "old"], "\(state)")
            XCTAssertEqual(rows.map(\.reach), [.connected, .offline, .connected])
            XCTAssertEqual(rows.map(\.action), [.dontApply, .dontApply, .none], "1.0 heard the key: no button")
            XCTAssertEqual(rows.map(\.showsLegacyNote), [false, false, true])
        }
        waiting.license.devices[1].canTakeBack = false
        XCTAssertEqual(LicenseText.waitingToApply(waiting)[1].action, .none, "no button where it can't be done")
        waiting.license.state = .unavailable
        XCTAssertEqual(LicenseText.waitingToApply(waiting), [], "the license can't be read")
        XCTAssertEqual(LicenseText.waitingToApply(status([device("pc")], [entry("pc", .activating, canTakeBack: true)])), [])
    }

    func testCountsThePlacesTakenOrPromised() {
        func rows(_ entries: [LicensedDevice]) -> [LicenseText.Row] {
            LicenseText.rows(status(entries.map { device($0.id) }, entries), thisName: "Mac")
        }
        let four = rows([entry("a", .licensed), entry("b", .activating), entry("c", .waiting), entry("d", .noPlaces),
                         entry("e", .otherLicense), entry("f", .none)])
        XCTAssertEqual(LicenseText.licensedCount(four), 2)
        XCTAssertFalse(LicenseText.allPlacesUsed(four), "this one and three: one place left")
        let five = rows([entry("a", .licensed), entry("b", .activating), entry("c", .waiting), entry("d", .licensed)])
        XCTAssertTrue(LicenseText.allPlacesUsed(five))
        XCTAssertFalse(LicenseText.noPlaces(five))
        XCTAssertEqual(LicenseText.licensedCount(rows([])), 1, "this computer alone")
    }

    func testWhileThisComputerIsOfflineForNowOthersArePlainlyOffline() {
        var here = status([device("pc", connected: false, offlineForNow: true)], [entry("pc", .licensed)])
        here.offlineForNow = true
        XCTAssertEqual(LicenseText.rows(here, thisName: "Mac")[1].reach, .offline)
    }

    func testOnTrialNamesAComputerToApplyTheLicenseFromNeverOneOn10() {
        var license = LicenseRecord()
        license.applySupported = true
        license.devices = [entry("old", .licensed, legacy: true), entry("trial", .none), entry("pc", .otherLicense),
                           entry("mini", .licensed)]
        XCTAssertEqual(LicenseText.licensedElsewhere(license), "PC")
        license.devices = [entry("old", .otherLicense, legacy: true), entry("trial", .activating)]
        XCTAssertNil(LicenseText.licensedElsewhere(license))
        license.devices = [entry("mini", .licensed)]
        XCTAssertEqual(LicenseText.licensedElsewhere(license), "MINI")
        license.applySupported = false
        XCTAssertNil(LicenseText.licensedElsewhere(license), "a service that does not apply licenses")
    }

    func testWarnsOfEveryPlaceTakenOnlyForAKeyEnteredHere() {
        var license = LicenseRecord()
        license.state = .seatsFull
        XCTAssertTrue(LicenseText.seatsFullHere(license))
        license.sourceDeviceID = "pc"
        XCTAssertFalse(LicenseText.seatsFullHere(license), "applied from the PC: plainly on trial")
        license.sourceDeviceID = ""
        license.state = .revoked
        XCTAssertFalse(LicenseText.seatsFullHere(license))
    }

    func testRemovingAComputerOffersToRevokeOnlyWhereThereIsALicenseToRevoke() {
        var license = LicenseRecord()
        license.state = .licensed
        license.devices = [entry("pc", .licensed, activation: "act-pc"), entry("laptop", .activating),
                           entry("studio", .waiting), entry("old", .licensed, legacy: true), entry("imac", .noPlaces),
                           entry("desk", .otherLicense), entry("trial", .none), entry("old2", .waiting, legacy: true)]
        let offers = ["pc", "laptop", "studio", "old", "imac", "desk", "trial", "old2", "unlisted"].map {
            LicenseText.removeOffer($0, license: license, licensedHere: true)
        }
        XCTAssertEqual(offers, [.revoke, .revoke, .revoke, .legacy, .none, .none, .none, .legacy, .none])
        XCTAssertEqual(LicenseText.removeOffer("pc", license: license, licensedHere: false), .none, "on trial here")
    }

    @MainActor
    func testAComputerTakenOfflineForNowStillHearsWhatTheServiceTellsIt() {
        let model = AgentModel.sample(status: status(
            [device("pc"), device("mini", connected: false), device("lounge", connected: false, offlineForNow: true)], []))
        XCTAssertTrue(model.isConnected("pc"))
        XCTAssertFalse(model.isConnected("mini"))
        XCTAssertTrue(model.isConnected("lounge"), "it keeps its connection")
        XCTAssertFalse(model.isConnected("gone"))
    }

    func testARevokeWithoutAnActivationReachesAConnectedComputer() {
        let unnamed = entry("pc", .licensed)
        XCTAssertNil(LicenseText.removeNotice(unnamed, deactivated: false, connected: true))
        XCTAssertEqual(LicenseText.removeNotice(unnamed, deactivated: false, connected: false), .notDeactivated)
        XCTAssertEqual(LicenseText.removeNotice(unnamed, deactivated: false, revoked: false, connected: true), .notDeactivated)
        XCTAssertEqual(
            LicenseText.removeNotice(entry("pc", .licensed, activation: "a"), deactivated: false, connected: true), .notDeactivated,
            "Polar did not free the activation it named")
        var outOfReach = entry("studio", .waiting, canTakeBack: true)
        outOfReach.keyOutOfReach = true
        XCTAssertEqual(LicenseText.removeNotice(outOfReach, deactivated: false, connected: false), .outOfReach)
    }

    func testARevokeTheServiceDidNotTakeLeavesTheLicenseNotRevoked() {
        var outOfReach = entry("studio", .waiting, canTakeBack: true)
        outOfReach.keyOutOfReach = true
        XCTAssertEqual(LicenseText.removeNotice(entry("pc", .licensed, activation: "a"), deactivated: true, revoked: false),
                       .notTold, "freed, but not told")
        XCTAssertEqual(LicenseText.removeNotice(entry("pc", .licensed, activation: "a"), deactivated: false, revoked: false),
                       .notDeactivated)
        XCTAssertNil(LicenseText.removeNotice(entry("pc", .licensed, activation: "a"), deactivated: true), "accepted")
        XCTAssertTrue(LicenseText.RemoveNotice.notDeactivated.offersPortal)
        XCTAssertTrue(LicenseText.RemoveNotice.outOfReach.offersPortal)
        XCTAssertFalse(LicenseText.RemoveNotice.notTold.offersPortal, "its place is free already")
        XCTAssertEqual(LicenseText.removeNotice(entry("mini", .waiting, canTakeBack: true), deactivated: false, revoked: false),
                       .notDeactivated)
        XCTAssertEqual(LicenseText.removeNotice(entry("laptop", .activating, canTakeBack: true), deactivated: false, revoked: false),
                       .notDeactivated)
        XCTAssertEqual(LicenseText.removeNotice(outOfReach, deactivated: false, revoked: false), .outOfReach)
    }

    func testSaysWhyTheLicenseOnARemovedComputerCouldNotBeRevoked() {
        var outOfReach = entry("studio", .waiting, canTakeBack: true)
        outOfReach.keyOutOfReach = true
        XCTAssertEqual(LicenseText.removeNotice(entry("pc", .licensed, activation: "a"), deactivated: false), .notDeactivated)
        XCTAssertNil(LicenseText.removeNotice(entry("pc", .licensed, activation: "a"), deactivated: true))
        XCTAssertEqual(LicenseText.removeNotice(outOfReach, deactivated: false), .outOfReach)
        XCTAssertNil(LicenseText.removeNotice(entry("mini", .waiting, canTakeBack: true), deactivated: false),
                     "the key never reached it")
        XCTAssertNil(LicenseText.removeNotice(entry("laptop", .activating, canTakeBack: true), deactivated: false))
    }

    func testTheWordsInBothLanguages() throws {
        func language(_ code: String) throws -> Bundle {
            let folder = try XCTUnwrap(
                Bundle(for: LicenseTextTests.self).path(forResource: code, ofType: "lproj"), "no \(code) translations")
            return try XCTUnwrap(Bundle(path: folder))
        }
        let en = try language("en")
        let zh = try language("zh-Hant")
        XCTAssertEqual(LicenseText.revokeTitle("Windows Desktop", bundle: en), "Revoke the license on Windows Desktop?")
        XCTAssertEqual(LicenseText.revokeTitle("Windows 桌機", bundle: zh), "要取消「Windows 桌機」的授權嗎？")
        XCTAssertEqual(
            LicenseText.revokeMessage("Windows 桌機", bundle: zh),
            "「Windows 桌機」會回到試用中，所有功能照常可用，並空出 5 台中的一個名額。之後可以再套用。")
        XCTAssertEqual(
            LicenseText.licensedElsewhereNote("MacBook Pro", bundle: en),
            "MacBook Pro is licensed. To use the license here too, apply it to this computer from MacBook Pro’s License page.")
        XCTAssertEqual(
            LicenseText.licensedElsewhereNote("MacBook Pro", bundle: zh),
            "「MacBook Pro」已授權。要讓這台也使用這份授權，請在「MacBook Pro」的授權頁套用到這台。")
        XCTAssertEqual(
            LicenseText.legacyRemoveNote("Mac mini", bundle: zh),
            "「Mac mini」是 InputWeave 1.0：移除後它的授權仍佔著名額；要空出名額，請到 Polar 的顧客入口停用它。")
        XCTAssertEqual(LicenseText.notRevokedTitle(.notDeactivated, name: "Mac mini", bundle: zh), "已移除「Mac mini」，但沒能取消它的授權。")
        XCTAssertEqual(LicenseText.notRevokedTitle(.outOfReach, name: "Mac mini", bundle: zh), "已移除「Mac mini」，但沒能取消它的授權。")
        XCTAssertEqual(
            LicenseText.notRevokedTitle(.notTold, name: "Mac mini", bundle: en), "Mac mini was removed, and its place on the license is free.")
        XCTAssertEqual(LicenseText.notRevokedTitle(.notTold, name: "Mac mini", bundle: zh), "已移除「Mac mini」，它在授權上的名額也已空出。")
        XCTAssertEqual(
            LicenseText.notRevokedMessage(.notTold, bundle: en),
            "It couldn’t be told, so it may still show as licensed until it next checks its license with Polar.")
        XCTAssertEqual(
            LicenseText.notRevokedMessage(.notTold, bundle: zh), "沒能通知它，所以在它下次向 Polar 確認授權之前，可能仍顯示為已授權。")
        XCTAssertEqual(
            LicenseText.notRevokedTitle(.notDeactivated, name: "Mac mini", bundle: en),
            "Mac mini was removed, but its license couldn’t be revoked.")
        XCTAssertEqual(LicenseText.notRevokedMessage(.notDeactivated, bundle: zh), "到 Polar 的顧客入口停用它，名額才會空出來。")
        XCTAssertEqual(
            LicenseText.notRevokedMessage(.outOfReach, bundle: en),
            "It was offline. If it activates the license later, free its place in Polar’s customer portal.")
        XCTAssertEqual(
            LicenseText.notRevokedMessage(.outOfReach, bundle: zh), "它當時沒有連線。之後如果它啟用了授權，請到 Polar 的顧客入口空出它的名額。")
        // The words the views and the alert use as they are.
        let words: [(String, String)] = [
            ("Computers on this license", "使用這份授權的電腦"), ("This computer", "這台電腦"), ("Offline", "離線"),
            ("Licensed", "已授權"), ("Revoke License…", "取消授權…"), ("Trial", "試用中"), ("Apply License", "套用授權"),
            ("Activating…", "正在啟用…"), ("Offline. Applies when it connects", "離線，連上後套用"), ("Don’t Apply", "不套用"),
            ("No places left", "名額已滿"), ("Another license", "另一份授權"),
            ("This computer uses a different license.", "這台使用另一份授權。"),
            ("Revoke License", "取消授權"), ("Keep", "保留"), ("Also revoke its license", "同時取消它的授權"),
            ("Frees one of your 5 places; it goes back to trial.", "空出 5 台中的一個名額；它會回到試用中。"),
            ("Deactivate it in Polar’s customer portal to free its place.", "到 Polar 的顧客入口停用它，名額才會空出來。"),
            ("All 5 are in use. To apply the license to another computer, revoke it on one first.",
             "5 台都用了。要套用到其他電腦，先取消其中一台的授權。"),
            ("On that computer’s License page, choose the other computers that use it, up to 5 in all.",
             "在那台的授權頁選擇其他要使用授權的電腦，最多 5 台。"),
            ("This frees one of your 5 places. Your other computers keep their license. You can enter the key again later, or apply it to this computer from a licensed one.",
             "這會空出 5 台中的一個名額，其他電腦的授權不受影響。之後可以再輸入授權碼，或從已授權的電腦再套用到這台。"),
            ("Manage Computers…", "管理電腦…"), ("OK", "好"), ("Waiting to Apply", "等待套用"),
            ("Applies once this computer is licensed again", "這台電腦恢復授權後套用"),
            ("InputWeave 1.0 didn’t take the license. Update it to 1.1, then apply it again.",
             "InputWeave 1.0 沒有接受這份授權；把它更新到 1.1 後再套用。"),
        ]
        for (english, chinese) in words {
            XCTAssertEqual(zh.localizedString(forKey: english, value: "MISSING", table: nil), chinese, english)
        }
        XCTAssertEqual(String(format: zh.localizedString(forKey: "%lld of %lld", value: nil, table: nil), 3, 5), "3／5 台")
    }
}
