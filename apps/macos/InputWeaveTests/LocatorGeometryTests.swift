import CoreGraphics
import XCTest

final class LocatorGeometryTests: XCTestCase {
    func testGrowsHoldsAndShrinksBackUnlessMotionIsReduced() {
        let moving = LocatorGeometry.motion(reduceMotion: false)
        XCTAssertEqual(moving.peak, 4)
        XCTAssertEqual(moving.shown, 1.1, accuracy: 0.001)
        XCTAssertEqual(LocatorGeometry.scale(moving, at: 0), 1)
        // Easing out: most of the way there halfway through growing.
        XCTAssertGreaterThan(LocatorGeometry.scale(moving, at: moving.grow / 2), 3)
        XCTAssertEqual(LocatorGeometry.scale(moving, at: moving.grow), 4)
        XCTAssertEqual(LocatorGeometry.scale(moving, at: moving.grow + moving.hold - 0.01), 4)
        // Halfway back, halfway between; its own size as it goes.
        let halfway = moving.grow + moving.hold + moving.shrink / 2
        XCTAssertEqual(LocatorGeometry.scale(moving, at: halfway), 2.5, accuracy: 0.001)
        XCTAssertEqual(LocatorGeometry.scale(moving, at: moving.shown - 0.001), 1, accuracy: 0.01)
        XCTAssertEqual(LocatorGeometry.scale(moving, at: moving.shown), 1)

        let still = LocatorGeometry.motion(reduceMotion: true)
        XCTAssertEqual(still.grow, 0)
        XCTAssertEqual(still.shrink, 0)
        XCTAssertEqual(still.shown, 1)
        for seconds in [0, 0.5, 0.99] {
            XCTAssertEqual(LocatorGeometry.scale(still, at: seconds), still.peak)
        }
    }

    func testTheTipStaysOnTheCursor() {
        // The arrow: 28 × 40 pt, its tip 5 pt in from the top left.
        let size = CGSize(width: 28, height: 40)
        let tip = CGPoint(x: 5, y: 5)
        let cursor = CGPoint(x: 700, y: 482)
        XCTAssertEqual(
            LocatorGeometry.frame(cursor: cursor, size: size, hotSpot: tip, scale: 1),
            CGRect(x: 695, y: 447, width: 28, height: 40))
        let grown = LocatorGeometry.frame(cursor: cursor, size: size, hotSpot: tip, scale: 4)
        XCTAssertEqual(grown.size, CGSize(width: 112, height: 160))
        // AppKit's y goes up: the tip is 4 × 5 pt in from the left and down
        // from the top.
        XCTAssertEqual(CGPoint(x: grown.minX + 20, y: grown.maxY - 20), cursor)
    }

    func testEachSightingCountsAnewEvenAtTheSamePlace() {
        let first = CursorSighting.after(nil, x: 700, y: 500)
        let again = CursorSighting.after(first, x: 700, y: 500)
        XCTAssertEqual(first.serial, 1)
        XCTAssertEqual(again.serial, 2)
        XCTAssertNotEqual(first, again)
    }
}
