import CoreGraphics
import XCTest

/// Names given to other computers, files turned off with them, and the
/// labels on screens being identified.
@MainActor
final class DeviceSharingTests: XCTestCase {
    private func model() -> AgentModel {
        var status = AgentStatus()
        status.thisDeviceID = "mac"
        status.thisDeviceName = "MacBook Pro"
        func device(_ id: String, _ name: String, reported: String, state: Inputweave_App_V1_ConnectionState) -> DeviceStatus {
            var device = DeviceStatus()
            device.id = id
            device.name = name
            device.reportedName = reported
            device.state = state
            return device
        }
        var studio = device("studio", "Studio PC", reported: "DESKTOP-4KQ", state: .connected)
        studio.filesOff = true
        status.devices = [
            device("pc", "Windows Desktop", reported: "Windows Desktop", state: .connected),
            studio,
            device("laptop", "Laptop", reported: "Laptop", state: .offline),
            // From a service before names given here.
            device("old", "Old Mac", reported: "", state: .connected),
        ]
        return AgentModel.sample(status: status)
    }

    func testFilesGoOnlyToConnectedComputersTheyAreOnWith() {
        let model = model()
        XCTAssertEqual(model.fileReceivers.map(\.id), ["pc", "old"])
        XCTAssertTrue(model.canReceiveFiles("pc"))
        XCTAssertFalse(model.canReceiveFiles("studio"))
        XCTAssertFalse(model.canReceiveFiles("laptop"))
        XCTAssertFalse(model.canReceiveFiles("mac"))
        XCTAssertTrue(model.filesOff("studio"))
        XCTAssertFalse(model.filesOff("pc"))
        XCTAssertFalse(model.filesOff("ghost"))
    }

    func testTellsTheNameAComputerGivesItselfOnlyWhenItWasRenamed() {
        let model = model()
        XCTAssertEqual(model.reportedName(of: "studio"), "DESKTOP-4KQ")
        XCTAssertNil(model.reportedName(of: "pc"))
        XCTAssertNil(model.reportedName(of: "old"))
        XCTAssertNil(model.reportedName(of: "mac"))
        // Everywhere else the app names it, the name given here shows.
        XCTAssertEqual(model.name(of: "studio"), "Studio PC")
    }

    func testEachIdentificationCountsAnew() {
        let first = ScreenIdentification.after(nil, durationMs: 2500)
        let second = ScreenIdentification.after(first, durationMs: 2500)
        XCTAssertNotEqual(first, second)
        XCTAssertEqual(ScreenIdentity.shownFor(durationMs: 2500), 2.5)
        XCTAssertEqual(ScreenIdentity.shownFor(durationMs: 0), 2.5)
    }

    private func screen(_ id: String, _ x: Double, _ y: Double, _ w: Double, _ h: Double, scale: Double) -> Inputweave_App_V1_Screen {
        var screen = Inputweave_App_V1_Screen()
        screen.id = id
        screen.x = x
        screen.y = y
        screen.width = w
        screen.height = h
        screen.scale = scale
        return screen
    }

    func testNumbersScreensAsTheArrangementListsThem() {
        // The main display, and one above it to the right, in the service's
        // coordinates (y downward from the main display's top).
        let screens = [
            screen("builtin", 0, 0, 1512, 982, scale: 2),
            screen("ext", 1512, -1080, 1920, 1080, scale: 1),
        ]
        // AppKit puts the main display's bottom at 0, y upward.
        let main = ScreenIdentity.label(
            frame: CGRect(x: 0, y: 0, width: 1512, height: 982), mainHeight: 982, backingScale: 2,
            screens: screens, fallback: 1)
        XCTAssertEqual(main, ScreenIdentity.Label(number: 1, width: 3024, height: 1964))
        let external = ScreenIdentity.label(
            frame: CGRect(x: 1512, y: 982, width: 1920, height: 1080), mainHeight: 982, backingScale: 1,
            screens: screens, fallback: 0)
        XCTAssertEqual(external, ScreenIdentity.Label(number: 2, width: 1920, height: 1080))
    }

    func testFallsBackToTheScreenItselfBeforeTheArrangementKnowsIt() {
        let label = ScreenIdentity.label(
            frame: CGRect(x: -1920, y: 0, width: 1920, height: 1200), mainHeight: 982, backingScale: 2,
            screens: [screen("builtin", 0, 0, 1512, 982, scale: 2)], fallback: 2)
        XCTAssertEqual(label, ScreenIdentity.Label(number: 3, width: 3840, height: 2400))
    }
}
