import XCTest

/// When the app asks for location access, reads the Wi-Fi name and tells
/// the background service, and when it turns the setting off.
final class WifiNamesTests: XCTestCase {
    private func status(
        on: Bool = true, network: String? = "n-office", supported: Bool = true, held: (network: String, ssid: String)? = nil
    ) -> AgentStatus {
        var status = AgentStatus()
        status.profilesSupported = supported
        status.wifiNames = on
        if let network {
            status.network.id = network
            status.network.kind = .wifi
            status.network.router = "192.168.1.1"
        }
        if let held {
            status.wifiNameHeld.networkID = held.network
            status.wifiNameHeld.ssid = held.ssid
        }
        return status
    }

    private let office = WifiNames.Report(networkID: "n-office", ssid: "Office-5G")
    private let start = Date(timeIntervalSince1970: 1_800_000_000)

    private func report(
        _ status: AgentStatus, reading: WifiNames.Report?, sent: WifiNames.Sent?, after seconds: Double = 0,
        access: WifiNames.Access = .granted
    ) -> WifiNames.Report? {
        WifiNames.report(status, access: access, reading: reading, sent: sent, now: start + seconds)
    }

    func testTurningTheSwitchOnAsksOnlyWhenItMust() {
        XCTAssertEqual(WifiNames.start(.granted), .turnOn)
        XCTAssertEqual(WifiNames.start(.unknown), .ask)
        // macOS does not ask again once the user said no.
        XCTAssertEqual(WifiNames.start(.denied), .refuse)
    }

    func testTheAnswerTurnsItOnOrBackOff() {
        XCTAssertEqual(WifiNames.answered(.granted, serviceOn: false), true)
        // Denied: back off, which the service is told only if it has it on.
        XCTAssertNil(WifiNames.answered(.denied, serviceOn: false))
        XCTAssertEqual(WifiNames.answered(.denied, serviceOn: true), false)
        // Not answered yet.
        XCTAssertNil(WifiNames.answered(.unknown, serviceOn: false))
    }

    private func step(
        on: Bool = true, _ access: WifiNames.Access, asking: Bool = false, asked: Bool = false, supported: Bool = true
    ) -> WifiNames.Step {
        WifiNames.step(status(on: on, supported: supported), access: access, asking: asking, askedThisLaunch: asked)
    }

    func testUndecidedAccessWithTheSwitchOnAsksOncePerLaunch() {
        XCTAssertEqual(step(.unknown), .ask)
        // Asked since the app opened, still undecided: no asking again.
        XCTAssertEqual(step(.unknown, asked: true), .wait)
        // While the user is asked after turning the switch on.
        XCTAssertEqual(step(.unknown, asking: true), .wait)
    }

    func testNeverAsksWhileTheSwitchIsOff() {
        XCTAssertEqual(step(on: false, .unknown), .idle)
        XCTAssertEqual(step(on: false, .denied), .idle)
        XCTAssertEqual(step(on: false, .granted), .idle)
        XCTAssertEqual(step(.unknown, supported: false), .idle)
    }

    func testADenialTurnsTheSwitchOffAndAccessGrantedReads() {
        // Denied once asked since the app opened, or later on.
        XCTAssertEqual(step(.denied, asked: true), .turnOff)
        XCTAssertEqual(step(.denied), .turnOff)
        // Answering the switch's own question turns it off (`answered`).
        XCTAssertEqual(step(.denied, asking: true, asked: true), .wait)
        XCTAssertEqual(step(.granted), .read)
        XCTAssertEqual(step(.granted, asked: true), .read)
    }

    func testReadsOnlyWhileTheSettingIsOnAndItMay() {
        XCTAssertTrue(WifiNames.reads(status(), access: .granted))
        XCTAssertFalse(WifiNames.reads(status(), access: .denied))
        XCTAssertFalse(WifiNames.reads(status(), access: .unknown))
        XCTAssertFalse(WifiNames.reads(status(on: false), access: .granted))
        XCTAssertFalse(WifiNames.reads(status(supported: false), access: .granted))
        XCTAssertEqual(WifiNames.interval, .seconds(5))
    }

    func testAFailedReadIsNotLeavingWiFi() {
        func sighting(powered: Bool = true, associated: Bool = true, _ ssid: String?) -> WifiNames.Sighting {
            WifiNames.Sighting(powered: powered, associated: associated, ssid: ssid)
        }
        XCTAssertEqual(WifiNames.ssid(sighting("Office-5G")), "Office-5G")
        // Not on Wi-Fi: off, or joined to no network.
        XCTAssertEqual(WifiNames.ssid(sighting(powered: false, nil)), "")
        XCTAssertEqual(WifiNames.ssid(sighting(associated: false, nil)), "")
        XCTAssertEqual(WifiNames.ssid(sighting(powered: false, associated: false, "Office-5G")), "")
        // Joined, but no name for now: the read failed.
        XCTAssertNil(WifiNames.ssid(sighting(nil)))
        XCTAssertNil(WifiNames.ssid(sighting("")))
    }

    func testTellsTheServiceAtOnceWhenTheReadingChanges() {
        // The first reading.
        XCTAssertEqual(report(status(), reading: office, sent: nil), office)
        let sent = WifiNames.Sent(report: office, at: start)
        // Another name, or none, at once, even while one is held.
        let none = WifiNames.Report(networkID: "n-office", ssid: "")
        XCTAssertEqual(report(status(held: ("n-office", "Office-5G")), reading: none, sent: sent, after: 1), none)
        // Another network, with the same name: at once too.
        let annex = WifiNames.Report(networkID: "n-annex", ssid: "Office-5G")
        XCTAssertEqual(report(status(network: "n-annex"), reading: annex, sent: sent, after: 1), annex)
        // A reading for a network the service no longer says this Mac is on,
        // or none yet: nothing.
        XCTAssertNil(report(status(network: "n-annex"), reading: office, sent: nil))
        XCTAssertNil(report(status(), reading: nil, sent: nil))
        // On no network, off, or without access: nothing.
        XCTAssertNil(report(status(network: nil), reading: office, sent: nil))
        XCTAssertNil(report(status(on: false), reading: office, sent: nil))
        XCTAssertNil(report(status(), reading: office, sent: nil, access: .denied))
        XCTAssertNil(report(status(), reading: office, sent: nil, access: .unknown))
    }

    func testAReportLostWhileTheServiceWaitedIsSentAgainAfterAWhile() {
        let sent = WifiNames.Sent(report: office, at: start)
        // The service answers holding nothing: not again at once, so no loop.
        XCTAssertNil(report(status(), reading: office, sent: sent))
        XCTAssertNil(report(status(), reading: office, sent: sent, after: 4.9))
        XCTAssertEqual(report(status(), reading: office, sent: sent, after: 5), office)
        // Sent again then; the next time only after as long again.
        let again = WifiNames.Sent(report: office, at: start + 5)
        XCTAssertNil(report(status(), reading: office, sent: again, after: 5.1))
        XCTAssertEqual(report(status(), reading: office, sent: again, after: 10), office)
        // Holding a name for another network counts as holding none here.
        XCTAssertEqual(report(status(held: ("n-home", "Home")), reading: office, sent: sent, after: 5), office)
    }

    func testNothingIsSentAgainWhileTheServiceHoldsAName() {
        let sent = WifiNames.Sent(report: office, at: start)
        XCTAssertNil(report(status(held: ("n-office", "Office-5G")), reading: office, sent: sent, after: 60))
        // Even another name than this app's reading, as when it refused an
        // empty one: nothing until the reading changes.
        let none = WifiNames.Report(networkID: "n-office", ssid: "")
        let sentNone = WifiNames.Sent(report: none, at: start)
        XCTAssertNil(report(status(held: ("n-office", "Office-5G")), reading: none, sent: sentNone, after: 60))
        XCTAssertEqual(report(status(held: ("n-office", "Office-5G")), reading: office, sent: sentNone, after: 61), office)
    }

    func testTurnedOffAndOnAgainItTellsTheServiceAgain() {
        var sent: WifiNames.Sent? = WifiNames.Sent(report: office, at: start)
        sent = WifiNames.kept(sent, status: status(), connected: true)
        XCTAssertNil(report(status(), reading: office, sent: sent, after: 1))
        // Off, by this app or anyone: forgotten.
        sent = WifiNames.kept(sent, status: status(on: false), connected: true)
        XCTAssertNil(sent)
        // On again on the same network: told again at once.
        sent = WifiNames.kept(sent, status: status(), connected: true)
        XCTAssertEqual(report(status(), reading: office, sent: sent, after: 1), office)
        // So after connecting again, and from a service from before.
        let kept = WifiNames.Sent(report: office, at: start)
        XCTAssertNil(WifiNames.kept(kept, status: status(), connected: false))
        XCTAssertNil(WifiNames.kept(kept, status: status(supported: false), connected: true))
        XCTAssertEqual(WifiNames.kept(kept, status: status(), connected: true), kept)
    }

    func testWhatTheServiceHoldsCountsOnlyForTheNetworkItIsOn() {
        XCTAssertFalse(WifiNames.held(status()))
        XCTAssertTrue(WifiNames.held(status(held: ("n-office", ""))))
        XCTAssertFalse(WifiNames.held(status(held: ("n-home", "Home"))))
    }
}
