import XCTest

final class ProblemWatchTests: XCTestCase {
    private func permissions(accessibility: Bool, inputMonitoring: Bool = true) -> [Inputweave_App_V1_PermissionStatus] {
        [(Inputweave_App_V1_Permission.accessibility, accessibility), (.inputMonitoring, inputMonitoring)].map {
            var status = Inputweave_App_V1_PermissionStatus()
            status.permission = $0.0
            status.granted = $0.1
            return status
        }
    }

    func testWhatIsWrongAtLaunchIsNotNews() {
        var watch = ProblemWatch()
        let first = watch.update(
            service: .needsApproval, permissions: permissions(accessibility: false, inputMonitoring: false), quiet: false)
        XCTAssertEqual(first, ProblemWatch.Change())
        // The service only reports permissions once the app reaches it.
        var later = ProblemWatch()
        XCTAssertEqual(later.update(service: .on, permissions: nil, quiet: false), ProblemWatch.Change())
        XCTAssertEqual(later.update(service: .on, permissions: [], quiet: false), ProblemWatch.Change())
        XCTAssertEqual(
            later.update(service: .on, permissions: permissions(accessibility: false), quiet: false), ProblemWatch.Change())
    }

    func testRaisesAPermissionTurnedOffAndTakesItBackOnceGranted() {
        var watch = ProblemWatch()
        _ = watch.update(service: .on, permissions: permissions(accessibility: true), quiet: false)
        XCTAssertEqual(
            watch.update(service: .on, permissions: permissions(accessibility: false), quiet: false).raised,
            [.permissionTurnedOff(.accessibility)])
        // Still off: nothing new.
        XCTAssertEqual(
            watch.update(service: .on, permissions: permissions(accessibility: false), quiet: false),
            ProblemWatch.Change())
        XCTAssertEqual(
            watch.update(service: .on, permissions: permissions(accessibility: true), quiet: false),
            ProblemWatch.Change(fixed: [.permissionTurnedOff(.accessibility)]))
    }

    func testNoticesAPermissionTurnedOffAcrossARestartOfTheService() {
        var watch = ProblemWatch()
        _ = watch.update(service: .on, permissions: permissions(accessibility: true), quiet: false)
        // macOS may stop the service when a permission goes; it comes back
        // without it, and reports nothing until it has looked.
        XCTAssertEqual(watch.update(service: .on, permissions: nil, quiet: false), ProblemWatch.Change())
        XCTAssertEqual(watch.update(service: .on, permissions: [], quiet: false), ProblemWatch.Change())
        XCTAssertEqual(
            watch.update(service: .on, permissions: permissions(accessibility: true, inputMonitoring: false), quiet: false).raised,
            [.permissionTurnedOff(.inputMonitoring)])
    }

    func testNamesOnlyTheFirstOfPermissionsTurnedOffTogether() {
        var watch = ProblemWatch()
        _ = watch.update(service: .on, permissions: permissions(accessibility: true), quiet: false)
        let change = watch.update(
            service: .on, permissions: permissions(accessibility: false, inputMonitoring: false), quiet: false)
        XCTAssertEqual(change.raised, [.permissionTurnedOff(.accessibility)])
        // Accessibility back on takes the notification back, though Input
        // Monitoring is still off: the panel shows that.
        XCTAssertEqual(
            watch.update(service: .on, permissions: permissions(accessibility: true, inputMonitoring: false), quiet: false),
            ProblemWatch.Change(fixed: [.permissionTurnedOff(.accessibility)]))
    }

    func testIgnoresPermissionsWithoutANotification() {
        var watch = ProblemWatch()
        var network = Inputweave_App_V1_PermissionStatus()
        network.permission = .localNetwork
        network.granted = true
        _ = watch.update(service: .on, permissions: [network], quiet: false)
        network.granted = false
        XCTAssertEqual(watch.update(service: .on, permissions: [network], quiet: false), ProblemWatch.Change())
    }

    func testRaisesTheServiceTurnedOffOnlyAfterSeeingItOn() {
        var watch = ProblemWatch()
        _ = watch.update(service: .on, permissions: nil, quiet: false)
        XCTAssertEqual(
            watch.update(service: .needsApproval, permissions: nil, quiet: false).raised,
            [.serviceTurnedOff(inLoginItems: true)])
        XCTAssertEqual(
            watch.update(service: .on, permissions: nil, quiet: false),
            ProblemWatch.Change(fixed: [.serviceTurnedOff(inLoginItems: true)]))
        XCTAssertEqual(
            watch.update(service: .off, permissions: nil, quiet: false).raised,
            [.serviceTurnedOff(inLoginItems: false)])

        // Seen off first, so not news; and a service missing from the app
        // is a broken install, not one turned off.
        var other = ProblemWatch()
        XCTAssertEqual(other.update(service: .off, permissions: nil, quiet: false), ProblemWatch.Change())
        XCTAssertEqual(other.update(service: .missing, permissions: nil, quiet: false), ProblemWatch.Change())
        XCTAssertEqual(other.update(service: .on, permissions: nil, quiet: false), ProblemWatch.Change())
        XCTAssertEqual(other.update(service: .missing, permissions: nil, quiet: false), ProblemWatch.Change())
    }

    func testStaysQuietBeforeSetupAndWhileTheAppTurnsTheServiceOff() {
        var watch = ProblemWatch()
        _ = watch.update(service: .on, permissions: permissions(accessibility: true), quiet: false)
        // The app unregisters it to register it again, or to quit.
        XCTAssertEqual(watch.update(service: .off, permissions: nil, quiet: true), ProblemWatch.Change())
        XCTAssertEqual(watch.update(service: .on, permissions: nil, quiet: false), ProblemWatch.Change())
        XCTAssertEqual(watch.update(service: .off, permissions: nil, quiet: true), ProblemWatch.Change())
        // Still off once the app is done is not a new change either.
        XCTAssertEqual(watch.update(service: .off, permissions: nil, quiet: false), ProblemWatch.Change())

        var setup = ProblemWatch()
        _ = setup.update(service: .on, permissions: permissions(accessibility: true), quiet: true)
        XCTAssertEqual(
            setup.update(service: .on, permissions: permissions(accessibility: false), quiet: true), ProblemWatch.Change())
        XCTAssertEqual(
            setup.update(service: .on, permissions: permissions(accessibility: false), quiet: false), ProblemWatch.Change())
    }

    func testANewerProblemOfTheSameKindReplacesTheOlder() {
        var watch = ProblemWatch()
        _ = watch.update(service: .on, permissions: permissions(accessibility: true), quiet: false)
        _ = watch.update(service: .on, permissions: permissions(accessibility: false), quiet: false)
        _ = watch.update(service: .on, permissions: permissions(accessibility: true), quiet: false)
        _ = watch.update(service: .on, permissions: permissions(accessibility: true, inputMonitoring: true), quiet: false)
        XCTAssertEqual(
            watch.update(service: .on, permissions: permissions(accessibility: true, inputMonitoring: false), quiet: false).raised,
            [.permissionTurnedOff(.inputMonitoring)])
        XCTAssertEqual(
            watch.update(service: .on, permissions: permissions(accessibility: false, inputMonitoring: false), quiet: false).raised,
            [.permissionTurnedOff(.accessibility)])
        // Only the notification that is still up is taken back.
        XCTAssertEqual(
            watch.update(service: .on, permissions: permissions(accessibility: true, inputMonitoring: true), quiet: false),
            ProblemWatch.Change(fixed: [.permissionTurnedOff(.accessibility)]))
    }
}
