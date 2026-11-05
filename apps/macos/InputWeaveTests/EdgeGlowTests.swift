import XCTest

final class EdgeGlowTests: XCTestCase {
    private typealias Geometry = EdgeGlowGeometry

    private func crossed(
        _ edge: Inputweave_App_V1_ScreenEdge, along: Double, display: CGRect = CGRect(x: 0, y: 0, width: 1512, height: 982)
    ) -> Inputweave_App_V1_CursorCrossed {
        var c = Inputweave_App_V1_CursorCrossed()
        c.displayID = "1"
        c.edge = edge
        c.along = along
        c.displayX = display.minX
        c.displayY = display.minY
        c.displayWidth = display.width
        c.displayHeight = display.height
        return c
    }

    func testTheEdgesAreTheServicesOnes() {
        XCTAssertEqual(Geometry.edge(.left), .left)
        XCTAssertEqual(Geometry.edge(.right), .right)
        XCTAssertEqual(Geometry.edge(.top), .top)
        XCTAssertEqual(Geometry.edge(.bottom), .bottom)
        XCTAssertNil(Geometry.edge(.unspecified))
    }

    func testTheCrossingIsPlacedAlongTheDisplay() {
        // From the top of a left or right edge, the service's y going down.
        XCTAssertEqual(Geometry.fraction(of: crossed(.right, along: 491), edge: .right), 0.5)
        XCTAssertEqual(Geometry.fraction(of: crossed(.left, along: 0), edge: .left), 0)
        // From the left end of a top or bottom one, on a display that does
        // not start at the origin.
        let side = CGRect(x: 1512, y: -300, width: 1080, height: 1920)
        XCTAssertEqual(Geometry.fraction(of: crossed(.top, along: 1782, display: side), edge: .top), 0.25)
        XCTAssertEqual(Geometry.fraction(of: crossed(.left, along: 1620, display: side), edge: .left), 1)
        // Outside the display, at its nearest end.
        XCTAssertEqual(Geometry.fraction(of: crossed(.right, along: -40), edge: .right), 0)
        XCTAssertEqual(Geometry.fraction(of: crossed(.right, along: 5000), edge: .right), 1)
    }

    func testTheGlowRunsTheWholeEdge() {
        let screen = CGRect(x: 0, y: 0, width: 1512, height: 982)
        XCTAssertEqual(
            Geometry.strip(on: screen, edge: .left, thickness: Geometry.depth),
            CGRect(x: 0, y: 0, width: 24, height: 982))
        XCTAssertEqual(
            Geometry.strip(on: screen, edge: .right, thickness: Geometry.depth),
            CGRect(x: 1488, y: 0, width: 24, height: 982))
        // AppKit's y goes up: the top edge is at the top of the frame.
        XCTAssertEqual(
            Geometry.strip(on: screen, edge: .top, thickness: Geometry.depth),
            CGRect(x: 0, y: 958, width: 1512, height: 24))
        XCTAssertEqual(
            Geometry.strip(on: screen, edge: .bottom, thickness: Geometry.solidDepth),
            CGRect(x: 0, y: 0, width: 1512, height: 4))
        // A screen beside and below the main one.
        let side = CGRect(x: 1512, y: -200, width: 1080, height: 1920)
        XCTAssertEqual(
            Geometry.strip(on: side, edge: .left, thickness: Geometry.depth),
            CGRect(x: 1512, y: -200, width: 24, height: 1920))
        XCTAssertEqual(
            Geometry.strip(on: side, edge: .top, thickness: Geometry.depth),
            CGRect(x: 1512, y: 1696, width: 1080, height: 24))
    }

    func testItIsBrightestWhereTheCursorCrossedAndShowsAllAlong() {
        // All of it where it crossed.
        XCTAssertEqual(Geometry.brightness(at: 0.3, spot: 0.3), 1, accuracy: 1e-9)
        // Crossed in the middle: about two fifths at both ends.
        XCTAssertEqual(Geometry.brightness(at: 0, spot: 0.5), Geometry.brightness(at: 1, spot: 0.5), accuracy: 1e-9)
        XCTAssertGreaterThanOrEqual(Geometry.brightness(at: 0, spot: 0.5), 0.35)
        XCTAssertLessThanOrEqual(Geometry.brightness(at: 0, spot: 0.5), 0.45)
        // Crossed at one end: the other end still shows.
        XCTAssertEqual(Geometry.brightness(at: 0, spot: 0), 1, accuracy: 1e-9)
        XCTAssertGreaterThanOrEqual(Geometry.brightness(at: 1, spot: 0), 0.35)
        // Dimmer the further from it, smoothly, never below the least.
        let along = Geometry.stops(spot: 0.2)
        XCTAssertEqual(along.first?.location, 0)
        XCTAssertEqual(along.last?.location, 1)
        let after = along.filter { $0.location >= 0.2 }.map { $0.brightness }
        XCTAssertEqual(after, after.sorted(by: >))
        XCTAssertTrue(along.allSatisfy { $0.brightness >= Geometry.dimmest && $0.brightness <= 1 })
    }

    func testItFadesInHoldsAndFadesOut() {
        let timing = Geometry.animated
        XCTAssertEqual(Geometry.opacity(timing, elapsed: 0), 0)
        XCTAssertEqual(Geometry.opacity(timing, elapsed: 0.05)!, 0.5, accuracy: 1e-9)
        XCTAssertEqual(Geometry.opacity(timing, elapsed: 0.35), 1)
        XCTAssertEqual(Geometry.opacity(timing, elapsed: 0.6)!, 0.5, accuracy: 1e-9)
        XCTAssertNil(Geometry.opacity(timing, elapsed: 0.8))
        // Still: there at once for half a second.
        XCTAssertEqual(Geometry.opacity(Geometry.still, elapsed: 0), 1)
        XCTAssertEqual(Geometry.opacity(Geometry.still, elapsed: 0.49), 1)
        XCTAssertNil(Geometry.opacity(Geometry.still, elapsed: 0.5))
    }

    func testCrossingTheSameEdgeWhileItShowsCarriesOnWithoutGoingDark() {
        let timing = Geometry.animated
        let start = Date()
        func relit(after seconds: Double) -> Date {
            Geometry.litAt(previous: start, now: start + seconds, timing: timing)
        }
        // Held: full at once, and held anew from now.
        let held = relit(after: 0.3)
        XCTAssertEqual(Geometry.opacity(timing, elapsed: (start + 0.3).timeIntervalSince(held))!, 1, accuracy: 1e-5)
        XCTAssertEqual(Geometry.opacity(timing, elapsed: (start + 0.3 + 0.3).timeIntervalSince(held)), 1)
        // Fading, halfway out (0.6 s in): from half as bright, up again.
        let fading = relit(after: 0.6)
        XCTAssertEqual(Geometry.opacity(timing, elapsed: (start + 0.6).timeIntervalSince(fading))!, 0.5, accuracy: 1e-5)
        XCTAssertEqual(Geometry.opacity(timing, elapsed: (start + 0.65).timeIntervalSince(fading))!, 1, accuracy: 1e-5)
        // Coming up (0.05 s in): on from there, not from dark.
        let rising = relit(after: 0.05)
        XCTAssertEqual(Geometry.opacity(timing, elapsed: (start + 0.05).timeIntervalSince(rising))!, 0.5, accuracy: 1e-5)
        // Gone, or never lit: anew. (Another edge or display is another
        // glow, testEachEdgeFadesOnItsOwn.)
        XCTAssertEqual(relit(after: 0.9), start + 0.9)
        XCTAssertEqual(Geometry.litAt(previous: nil, now: start, timing: timing), start)
        // Still: a crossing while it shows keeps it there, for half a second again.
        XCTAssertEqual(Geometry.litAt(previous: start, now: start + 0.4, timing: Geometry.still), start + 0.4)
    }

    func testReduceMotionShowsItStillForHalfASecond() {
        XCTAssertEqual(Geometry.timing(reduceMotion: false), .init(fadeIn: 0.1, hold: 0.3, fadeOut: 0.4))
        XCTAssertEqual(Geometry.timing(reduceMotion: true), .init(fadeIn: 0, hold: 0.5, fadeOut: 0))
    }

    private func crossed(on display: CGRect, id: String) -> Inputweave_App_V1_CursorCrossed {
        var c = crossed(.left, along: display.midY, display: display)
        c.displayID = id
        return c
    }

    func testTheDisplayIsTheScreenOfItsNumberElseOfItsPlace() {
        // A built-in display, and two identical ones to its right, in
        // AppKit's coordinates (y up from the bottom of the main display).
        let screens: [(number: UInt32?, frame: CGRect)] = [
            (1, CGRect(x: 0, y: 0, width: 1512, height: 982)),
            (2, CGRect(x: 1512, y: 0, width: 1920, height: 1080)),
            (3, CGRect(x: 3432, y: 0, width: 1920, height: 1080)),
        ]
        func screen(_ crossed: Inputweave_App_V1_CursorCrossed) -> Int? {
            Geometry.screen(for: crossed, among: screens, mainHeight: 982)
        }
        // By its number, wherever the display was said to be.
        XCTAssertEqual(screen(crossed(on: CGRect(x: 3432, y: -98, width: 1920, height: 1080), id: "2")), 1)
        XCTAssertEqual(screen(crossed(on: CGRect(x: 3432, y: -98, width: 1920, height: 1080), id: "1")), 0)
        // Not a number of these: the one at its place, in the service's
        // coordinates (y down from the top of the main display), even of
        // two the same size.
        XCTAssertEqual(screen(crossed(on: CGRect(x: 3432, y: -98, width: 1920, height: 1080), id: "9")), 2)
        XCTAssertEqual(screen(crossed(on: CGRect(x: 1512, y: -98, width: 1920, height: 1080), id: "main")), 1)
        // Elsewhere: the only one of its size, if there is one.
        XCTAssertEqual(screen(crossed(on: CGRect(x: 500, y: 500, width: 1512, height: 982), id: "9")), 0)
        XCTAssertNil(screen(crossed(on: CGRect(x: 0, y: 700, width: 1920, height: 1080), id: "9")))
        XCTAssertNil(screen(crossed(on: CGRect(x: 0, y: 0, width: 800, height: 600), id: "main")))
    }

    func testTwoIdenticalScreensAboveAndBelowAreToldApart() {
        let screens: [(number: UInt32?, frame: CGRect)] = [
            (1, CGRect(x: 0, y: 0, width: 1512, height: 982)),
            (2, CGRect(x: 0, y: 982, width: 1920, height: 1080)),
            (3, CGRect(x: 0, y: -1080, width: 1920, height: 1080)),
        ]
        func screen(_ crossed: Inputweave_App_V1_CursorCrossed) -> Int? {
            Geometry.screen(for: crossed, among: screens, mainHeight: 982)
        }
        // Above the main display is up in AppKit, and before it going down.
        XCTAssertEqual(screen(crossed(on: CGRect(x: 0, y: -1080, width: 1920, height: 1080), id: "8")), 1)
        XCTAssertEqual(screen(crossed(on: CGRect(x: 0, y: 982, width: 1920, height: 1080), id: "8")), 2)
    }

    @MainActor
    func testEveryCrossingIsToldEvenBackToBack() {
        let model = AgentModel(socketPath: "/nonexistent")
        var told: [Inputweave_App_V1_CursorCrossed] = []
        model.onCursorCrossed = { told.append($0) }
        // Out over one edge and straight back in over another, and the
        // first again: each one is told, in order.
        let left = crossed(.left, along: 400)
        var top = crossed(.top, along: 700)
        top.arriving = true
        for crossing in [left, top, left] {
            model.handle(.with { $0.event = .with { $0.cursorCrossed = crossing } })
        }
        XCTAssertEqual(told, [left, top, left])
    }

    func testEachEdgeFadesOnItsOwn() {
        let timing = Geometry.animated
        let start = Date()
        var glows = Geometry.Glows()
        func opacity(_ edge: Geometry.Edge, after seconds: Double, display: String = "1") -> Double?? {
            var copy = glows
            return copy.fade(at: start + seconds)[Geometry.Glows.Key(display: display, edge: edge)]
        }
        // Two crossings back to back, out over one edge and in over another
        // of the same display: both light.
        glows.light(display: "1", edge: .right, now: start, timing: timing)
        glows.light(display: "1", edge: .top, now: start, timing: timing)
        XCTAssertEqual(opacity(.right, after: 0.05)!!, 0.5, accuracy: 1e-5)
        XCTAssertEqual(opacity(.top, after: 0.05)!!, 0.5, accuracy: 1e-5)
        // Another edge lit later does not cut the first short.
        glows.light(display: "1", edge: .left, now: start + 0.3, timing: timing)
        XCTAssertEqual(opacity(.right, after: 0.3)!!, 1, accuracy: 1e-5)
        XCTAssertEqual(opacity(.right, after: 0.6)!!, 0.5, accuracy: 1e-5)
        XCTAssertEqual(opacity(.left, after: 0.35)!!, 0.5, accuracy: 1e-5)
        // The same edge again carries on from how bright it is.
        glows.light(display: "1", edge: .right, now: start + 0.6, timing: timing)
        XCTAssertEqual(opacity(.right, after: 0.6)!!, 0.5, accuracy: 1e-5)
        // The same edge of another display is its own.
        glows.light(display: "2", edge: .right, now: start + 0.6, timing: timing)
        XCTAssertEqual(opacity(.right, after: 0.6, display: "2")!!, 0, accuracy: 1e-5)
        // Gone: listed once, then forgotten; at most four for a display.
        let top = Geometry.Glows.Key(display: "1", edge: .top)
        XCTAssertEqual(glows.fade(at: start + 0.85)[top], .some(nil))
        XCTAssertFalse(glows.fade(at: start + 0.85).keys.contains(top))
        XCTAssertLessThanOrEqual(glows.lit.keys.filter { $0.display == "1" }.count, 4)
    }

    func testTheGlowIsOnUnlessTurnedOff() throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "EdgeGlowTests"))
        defaults.removePersistentDomain(forName: "EdgeGlowTests")
        XCTAssertTrue(EdgeGlow.isOn(defaults))
        defaults.set(false, forKey: Preferences.showsEdgeGlow)
        XCTAssertFalse(EdgeGlow.isOn(defaults))
        defaults.set(true, forKey: Preferences.showsEdgeGlow)
        XCTAssertTrue(EdgeGlow.isOn(defaults))
        defaults.removePersistentDomain(forName: "EdgeGlowTests")
    }
}
