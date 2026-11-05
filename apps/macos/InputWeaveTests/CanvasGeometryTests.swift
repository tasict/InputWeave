import CoreGraphics
import XCTest

final class CanvasGeometryTests: XCTestCase {
    let mac = CanvasDevice(id: "mac", offset: .zero, screens: [CGRect(x: 0, y: 0, width: 1512, height: 982)])
    func pc(at offset: CGPoint) -> CanvasDevice {
        CanvasDevice(id: "pc", offset: offset, screens: [CGRect(x: 0, y: 0, width: 2560, height: 1440)])
    }

    func testSnapsBesideAndLinesUpTops() {
        // Dropped a little right of the Mac and a little low.
        let snapped = CanvasGeometry.snap(pc(at: CGPoint(x: 1520, y: 6)), among: [mac], threshold: 10)
        XCTAssertEqual(snapped, CGPoint(x: 1512, y: 0))
    }

    func testLeavesFarAwayDevicesAlone() {
        let far = pc(at: CGPoint(x: 1700, y: 300))
        XCTAssertEqual(CanvasGeometry.snap(far, among: [mac], threshold: 10), far.offset)
    }

    func testSnapsOnTop() {
        let snapped = CanvasGeometry.snap(pc(at: CGPoint(x: 4, y: -1436)), among: [mac], threshold: 10)
        XCTAssertEqual(snapped, CGPoint(x: 0, y: -1440))
    }

    func testTouchingIsNotOverlapping() {
        XCTAssertFalse(CanvasGeometry.overlaps(mac.frames, pc(at: CGPoint(x: 1512, y: 0)).frames))
        XCTAssertTrue(CanvasGeometry.overlaps(mac.frames, pc(at: CGPoint(x: 1500, y: 0)).frames))
    }

    func testFindsTheSharedEdge() {
        let seams = CanvasGeometry.seams([mac, pc(at: CGPoint(x: 1512, y: 400))])
        XCTAssertEqual(seams, [Seam(start: CGPoint(x: 1512, y: 400), end: CGPoint(x: 1512, y: 982))])
        XCTAssertTrue(CanvasGeometry.seams([mac, pc(at: CGPoint(x: 1600, y: 0))]).isEmpty)
    }

    func testLabelsTheMainScreenOrTheWidestThatHasRoom() {
        let room = CGSize(width: 36, height: 20)
        let roomy = CGRect(x: 0, y: 0, width: 90, height: 50)
        let narrow = CGRect(x: 90, y: -10, width: 30, height: 70)
        let wide = CGRect(x: 120, y: 0, width: 60, height: 40)
        XCTAssertEqual(CanvasGeometry.labelTile([roomy, narrow, wide], main: 0, room: room), 0)
        XCTAssertEqual(CanvasGeometry.labelTile([roomy, narrow, wide], main: 2, room: room), 2)
        XCTAssertEqual(CanvasGeometry.labelTile([roomy, narrow, wide], main: 1, room: room), 0)
        XCTAssertNil(CanvasGeometry.labelTile([narrow], main: 0, room: room))
    }

    func testCentersTheBadgeWhenTheNameLeavesTheMiddleFree() {
        let badge = CGSize(width: 30, height: 14)
        let tall = CGRect(x: 10, y: 0, width: 80, height: 60)
        XCTAssertEqual(
            CanvasGeometry.badgeFrame(badge, on: tall, nameBottom: 17, inset: 2),
            CGRect(x: 35, y: 23, width: 30, height: 14))
        // No name on this tile: the middle, whatever the tile's size.
        let short = CGRect(x: 10, y: 0, width: 50, height: 34)
        XCTAssertEqual(
            CanvasGeometry.badgeFrame(badge, on: short, nameBottom: nil, inset: 2),
            CGRect(x: 20, y: 10, width: 30, height: 14))
    }

    func testMovesTheBadgeBelowANameThatCoversTheMiddle() {
        let badge = CGSize(width: 30, height: 14)
        // The space below the name runs from 18 (a point under it) to 32.
        let short = CGRect(x: 10, y: 0, width: 50, height: 34)
        let frame = CanvasGeometry.badgeFrame(badge, on: short, nameBottom: 17, inset: 2)
        XCTAssertEqual(frame, CGRect(x: 20, y: 18, width: 30, height: 14))
        XCTAssertTrue(short.insetBy(dx: 2, dy: 2).contains(frame))
        // Taller: centered in that space.
        let taller = CGRect(x: 10, y: 0, width: 50, height: 42)
        XCTAssertEqual(
            CanvasGeometry.badgeFrame(badge, on: taller, nameBottom: 17, inset: 2),
            CGRect(x: 20, y: 22, width: 30, height: 14))
    }

    func testKeepsTheBadgeInsideATileWithoutRoomBelowTheName() {
        let badge = CGSize(width: 30, height: 14)
        // As low as the tile allows: over the name's last points.
        let flat = CGRect(x: 10, y: 0, width: 50, height: 26)
        XCTAssertEqual(
            CanvasGeometry.badgeFrame(badge, on: flat, nameBottom: 17, inset: 2),
            CGRect(x: 20, y: 10, width: 30, height: 14))
        // Larger than the tile: centered on it, spilling over evenly.
        let tiny = CGRect(x: 10, y: 0, width: 20, height: 12)
        XCTAssertEqual(
            CanvasGeometry.badgeFrame(badge, on: tiny, nameBottom: 17, inset: 2),
            CGRect(x: 5, y: -1, width: 30, height: 14))
    }

    func testFitCentersContent() {
        let fit = CanvasGeometry.fit(CGRect(x: 0, y: 0, width: 200, height: 100), in: CGSize(width: 420, height: 420), padding: 10)
        XCTAssertEqual(fit.scale, 2)
        XCTAssertEqual(fit.toView(CGPoint(x: 0, y: 0)), CGPoint(x: 10, y: 110))
        XCTAssertEqual(fit.toView(CGPoint(x: 200, y: 100)), CGPoint(x: 410, y: 310))
    }
}
