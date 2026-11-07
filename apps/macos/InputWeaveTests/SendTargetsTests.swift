import Foundation
import XCTest

private func language(_ code: String) throws -> Bundle {
    let folder = try XCTUnwrap(
        Bundle(for: SendTargetsTests.self).path(forResource: code, ofType: "lproj"), "no \(code) translations")
    return try XCTUnwrap(Bundle(path: folder))
}

/// Where files from Finder's Services menu go: the reason a computer can't
/// take them, the choice between none, one and several, and the words.
@MainActor
final class SendTargetsTests: XCTestCase {
    private func device(
        _ id: String, _ state: Inputweave_App_V1_ConnectionState = .connected, filesOff: Bool = false, rtt: UInt32 = 0,
        offlineForNow: Bool = false
    ) -> DeviceStatus {
        var device = DeviceStatus()
        device.id = id
        device.name = id.uppercased()
        device.os = "windows"
        device.state = state
        device.filesOff = filesOff
        device.rttMs = rtt
        device.offlineForNow = offlineForNow
        return device
    }

    func testTheReasonIsTheFirstThatApplies() throws {
        XCTAssertNil(SendTargets.unavailable(device("a")))
        XCTAssertEqual(SendTargets.unavailable(device("a", filesOff: true)), .filesOff)
        XCTAssertEqual(SendTargets.unavailable(device("a", .offline)), .offline)
        XCTAssertEqual(SendTargets.unavailable(device("a", .incompatible)), .offline, "a version that can't connect")
        XCTAssertEqual(SendTargets.unavailable(device("a", .offline, filesOff: true)), .offline, "offline before files off")
        XCTAssertEqual(
            SendTargets.unavailable(device("a", .offline, filesOff: true, offlineForNow: true)), .offlineForNow,
            "offline for now before offline and files off")

        let en = try language("en")
        let zh = try language("zh-Hant")
        XCTAssertEqual(SendTargets.reason(.offline, bundle: en), "Offline")
        XCTAssertEqual(SendTargets.reason(.filesOff, bundle: en), "File transfer with this computer is off")
        XCTAssertEqual(SendTargets.reason(.offline, bundle: zh), "離線")
        XCTAssertEqual(SendTargets.reason(.filesOff, bundle: zh), "沒有和這台互傳檔案")
        XCTAssertEqual(SendTargets.reason(.offlineForNow, bundle: en), "Offline for now")
        XCTAssertEqual(SendTargets.reason(.offlineForNow, bundle: zh), "暫時離線")
    }

    func testAComputerOfflineForNowSaysSo() throws {
        let zh = try language("zh-Hant")
        let group = [device("mini", .offline, offlineForNow: true), device("pc", rtt: 3), device("off", .offline)]
        XCTAssertEqual(SendTargets.choice(group), .only("pc"))
        XCTAssertEqual(
            SendTargets.targets(group, bundle: zh).map { [$0.id, $0.status, "\($0.available)"] },
            [["pc", "已連線・3 ms", "true"], ["mini", "暫時離線", "false"], ["off", "離線", "false"]])
        // This Mac offline for now: the service reports every other computer
        // just offline, so there is nowhere to send.
        let apart = [device("mini", .offline), device("pc", .offline)]
        XCTAssertEqual(SendTargets.choice(apart), .nowhere)
        XCTAssertEqual(SendTargets.targets(apart, bundle: zh).map(\.status), ["離線", "離線"])
    }

    func testTheFileReceiversAreThoseWithoutAReason() {
        var status = AgentStatus()
        status.devices = [device("a"), device("b", filesOff: true), device("c", .offline)]
        let model = AgentModel.sample(status: status)
        XCTAssertEqual(model.fileReceivers.map(\.id), ["a"])
        XCTAssertTrue(model.canReceiveFiles("a"))
        XCTAssertFalse(model.canReceiveFiles("b"))
    }

    func testNowhereOneOrAChoice() {
        XCTAssertEqual(SendTargets.choice([]), .nowhere)
        XCTAssertEqual(SendTargets.choice([device("off", .offline)]), .nowhere, "none connected")
        // Connected, but none can take files: the chooser says why, with
        // nothing to choose.
        let noneCan = [device("off", .offline), device("nofiles", filesOff: true)]
        XCTAssertEqual(SendTargets.choice(noneCan), .ask)
        XCTAssertEqual(SendTargets.targets(noneCan).map(\.available), [false, false])
        XCTAssertNil(SendTargets.chosen(clicked: nil, shown: nil, in: SendTargets.targets(noneCan)))
        XCTAssertEqual(SendTargets.choice([device("off", .offline), device("pc")]), .only("pc"))
        XCTAssertEqual(
            SendTargets.choice([device("nofiles", filesOff: true), device("pc")]), .only("pc"),
            "the only one that can")
        XCTAssertEqual(SendTargets.choice([device("pc"), device("mac")]), .ask)
    }

    func testASecondRequestAddsItsItemsOnce() {
        let a = URL(fileURLWithPath: "/Users/me/a.pdf")
        let b = URL(fileURLWithPath: "/Users/me/Photos", isDirectory: true)
        let c = URL(fileURLWithPath: "/Users/me/c.txt")
        // The same folder, as Finder may give it, without the slash.
        let sameB = URL(fileURLWithPath: "/Users/me/./Photos")
        XCTAssertEqual(SendTargets.merged([a, b], [sameB, c, a]), [a, b, c])
        XCTAssertEqual(SendTargets.merged([], [a]), [a])
    }

    func testWaitingForTheServiceEndsAfterTheLimit() async {
        let model = AgentModel.sample(status: AgentStatus(), link: .connecting)
        let started = ContinuousClock.now
        let connected = await model.connected(within: .milliseconds(200))
        XCTAssertFalse(connected)
        XCTAssertGreaterThanOrEqual(ContinuousClock.now - started, .milliseconds(200))
        let ready = AgentModel.sample(status: AgentStatus())
        let already = await ready.connected(within: .seconds(10))
        XCTAssertTrue(already, "already connected")
    }

    func testTheChooserListsThoseThatCanTakeFilesFirst() throws {
        let devices = [
            device("off", .offline), device("pc", rtt: 3), device("nofiles", filesOff: true, rtt: 4), device("mac"),
        ]
        let targets = SendTargets.targets(devices, bundle: try language("en"))
        XCTAssertEqual(targets.map(\.id), ["pc", "mac", "off", "nofiles"])
        XCTAssertEqual(targets.map(\.available), [true, true, false, false])
        XCTAssertEqual(
            targets.map(\.status),
            ["Connected · 3 ms", "Connected", "Offline", "File transfer with this computer is off"])
        XCTAssertEqual(targets[0].name, "PC")
        // Every line from the same bundle.
        XCTAssertEqual(
            SendTargets.targets(devices, bundle: try language("zh-Hant")).map(\.status),
            ["已連線・3 ms", "已連線", "離線", "沒有和這台互傳檔案"])
    }

    func testTheChoiceStaysWhileItCanTakeFiles() {
        func with(ahead: Inputweave_App_V1_ConnectionState, mac: Inputweave_App_V1_ConnectionState) -> [SendTargets.Target] {
            SendTargets.targets([device("ahead", ahead), device("pc"), device("mac", mac)])
        }
        // By default the first that can.
        XCTAssertEqual(SendTargets.chosen(clicked: nil, shown: nil, in: with(ahead: .offline, mac: .connected)), "pc")
        // A computer that connects ahead of it does not take its place.
        let after = with(ahead: .connected, mac: .connected)
        XCTAssertEqual(SendTargets.chosen(clicked: nil, shown: "pc", in: after), "pc", "the default stays")
        XCTAssertEqual(SendTargets.chosen(clicked: "mac", shown: "mac", in: after), "mac")
        // The one clicked goes offline: the first that can; it comes back
        // when it can again.
        XCTAssertEqual(SendTargets.chosen(clicked: "mac", shown: "mac", in: with(ahead: .connected, mac: .offline)), "ahead")
        XCTAssertEqual(SendTargets.chosen(clicked: "mac", shown: "ahead", in: after), "mac", "a click comes back")
        // A default the choice moved away from does not come back.
        let pcGone = SendTargets.targets([device("ahead"), device("pc", .offline)])
        XCTAssertEqual(SendTargets.chosen(clicked: nil, shown: "pc", in: pcGone), "ahead")
        XCTAssertEqual(SendTargets.chosen(clicked: nil, shown: "ahead", in: after), "ahead", "pc, back, is not chosen again")
        XCTAssertEqual(SendTargets.chosen(clicked: "gone", shown: "gone", in: after), "ahead")
        XCTAssertNil(SendTargets.chosen(clicked: nil, shown: nil, in: SendTargets.targets([device("off", .offline)])))
    }

    func testTheSummaryNamesTheFirstItem() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        let report = URL(fileURLWithPath: "/Users/me/Report.pdf")
        let photos = URL(fileURLWithPath: "/Users/me/Photos", isDirectory: true)
        XCTAssertEqual(SendTargets.summary([report], bundle: en), "Report.pdf")
        XCTAssertEqual(SendTargets.summary([photos], bundle: en), "Photos", "a folder")
        XCTAssertEqual(SendTargets.summary([report, photos], bundle: en), "Report.pdf and 1 more item")
        XCTAssertEqual(SendTargets.summary([report, photos, photos], bundle: en), "Report.pdf and 2 more items")
        XCTAssertEqual(SendTargets.summary([report, photos], bundle: zh), "Report.pdf 與另外 1 個項目")
        XCTAssertEqual(SendTargets.summary([report, photos, photos], bundle: zh), "Report.pdf 與另外 2 個項目")
        XCTAssertEqual(SendTargets.summary([], bundle: en), "")
    }

    func testTheHUDSaysHowManyAndWhere() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        XCTAssertEqual(SendTargets.sending(items: 2, to: "Windows Desktop", bundle: en), "Sending 2 items to Windows Desktop…")
        XCTAssertEqual(SendTargets.sending(items: 1, to: "Windows Desktop", bundle: en), "Sending 1 item to Windows Desktop…")
        XCTAssertEqual(SendTargets.sending(items: 2, to: "Windows 桌機", bundle: zh), "正在傳送 2 個項目到「Windows 桌機」…")
        XCTAssertEqual(SendTargets.sending(items: 1, to: "Mac mini", bundle: zh), "正在傳送 1 個項目到「Mac mini」…")
    }

    func testTheChooserGoesBesideThePointerAndStaysOnItsScreen() throws {
        let screen = try XCTUnwrap(NSScreen.screens.first)
        let visible = screen.visibleFrame
        let size = CGSize(width: 320, height: 240)
        let middle = CGPoint(x: visible.midX, y: visible.midY)
        XCTAssertEqual(SendChooser.origin(for: size, near: middle), CGPoint(x: middle.x + 8, y: middle.y - 8 - 240))
        // At the bottom right corner, it moves up and left onto the screen.
        let corner = SendChooser.origin(for: size, near: CGPoint(x: visible.maxX - 1, y: visible.minY + 1))
        XCTAssertEqual(corner.x, visible.maxX - 320 - 8)
        XCTAssertEqual(corner.y, visible.minY + 8)
    }
}
