import CoreGraphics
import Foundation
import XCTest

final class DropStripGeometryTests: XCTestCase {
    /// A Mac whose desktop sits at (0, 200) on the canvas.
    let mac = CanvasDevice(id: "mac", offset: CGPoint(x: 0, y: 200), screens: [CGRect(x: 0, y: 0, width: 1512, height: 982)])

    func device(_ id: String, at offset: CGPoint, _ screens: [CGRect]) -> CanvasDevice {
        CanvasDevice(id: id, offset: offset, screens: screens)
    }

    func testStandsWhileFilesCanLeaveFromHereEvenWhenControlled() {
        var status = Inputweave_App_V1_Status()
        status.sharing = true
        status.capturing = true
        XCTAssertTrue(DropStripGeometry.shown(connected: true, status: status))
        // Controlled by the PC: only if the PC can carry a drag from here.
        status.controlledByDeviceID = "pc"
        XCTAssertFalse(DropStripGeometry.shown(connected: true, status: status))
        status.dragsCarried = true
        XCTAssertTrue(DropStripGeometry.shown(connected: true, status: status))
        XCTAssertFalse(DropStripGeometry.shown(connected: false, status: status))
        status.locked = true
        XCTAssertFalse(DropStripGeometry.shown(connected: true, status: status))
        status.locked = false
        status.sharing = false
        XCTAssertFalse(DropStripGeometry.shown(connected: true, status: status))
        status.sharing = true
        status.capturing = false
        XCTAssertFalse(DropStripGeometry.shown(connected: true, status: status))
    }

    func testPutsAStripInsideTheEdgeWhereTheSeamIs() {
        // The PC's screen meets the Mac's right edge from y = 400 down.
        let pc = device("pc", at: CGPoint(x: 1512, y: 600), [CGRect(x: 0, y: 0, width: 2560, height: 1440)])
        XCTAssertEqual(
            DropStripGeometry.strips(this: mac, others: [pc]),
            // In the Mac's own desktop: the seam runs from 400 to 982.
            [CGRect(x: 1504, y: 400, width: 8, height: 582)])
        // Not touching: no strip.
        let apart = device("pc", at: CGPoint(x: 1600, y: 0), [CGRect(x: 0, y: 0, width: 2560, height: 1440)])
        XCTAssertTrue(DropStripGeometry.strips(this: mac, others: [apart]).isEmpty)
    }

    func testFindsEverySide() {
        let left = device("left", at: CGPoint(x: -1000, y: 200), [CGRect(x: 0, y: 0, width: 1000, height: 500)])
        let above = device("above", at: CGPoint(x: 100, y: -880), [CGRect(x: 0, y: 0, width: 1920, height: 1080)])
        let below = device("below", at: CGPoint(x: -200, y: 1182), [CGRect(x: 0, y: 0, width: 800, height: 600)])
        XCTAssertEqual(
            Set(DropStripGeometry.strips(this: mac, others: [left, above, below]).map(\.debugDescription)),
            Set([
                CGRect(x: 0, y: 0, width: 8, height: 500),
                CGRect(x: 100, y: 0, width: 1412, height: 8),
                CGRect(x: 0, y: 974, width: 600, height: 8),
            ].map(\.debugDescription)))
    }

    func testRunsAlongOneEdgeMakeOneStrip() {
        // Two screens of another computer stacked against the right edge,
        // and a third device's screen continuing below them.
        let pc = device("pc", at: CGPoint(x: 1512, y: 0), [
            CGRect(x: 0, y: 0, width: 1920, height: 600),
            CGRect(x: 0, y: 600, width: 1920, height: 400),
        ])
        let mini = device("mini", at: CGPoint(x: 1512, y: 1000), [CGRect(x: 0, y: 0, width: 1920, height: 1080)])
        XCTAssertEqual(
            DropStripGeometry.strips(this: mac, others: [pc, mini]),
            [CGRect(x: 1504, y: 0, width: 8, height: 982)])
    }

    func testFollowsThisComputersOwnScreens() {
        // The external screen is right of the built-in one; the PC is right
        // of the external screen only.
        let mac = device("mac", at: CGPoint(x: 0, y: 0), [
            CGRect(x: 0, y: 0, width: 1512, height: 982),
            CGRect(x: 1512, y: -300, width: 1920, height: 1080),
        ])
        let pc = device("pc", at: CGPoint(x: 3432, y: -300), [CGRect(x: 0, y: 0, width: 2560, height: 1440)])
        XCTAssertEqual(
            DropStripGeometry.strips(this: mac, others: [pc]),
            [CGRect(x: 3424, y: -300, width: 8, height: 1080)])
    }

    func testTurnsDesktopRectanglesIntoScreenRectangles() {
        // The main display is 982 points high: the top of the desktop is at
        // the top of AppKit's screen space.
        XCTAssertEqual(
            DropStripGeometry.screenFrame(CGRect(x: 1504, y: 400, width: 8, height: 582), mainHeight: 982),
            CGRect(x: 1504, y: 0, width: 8, height: 582))
        XCTAssertEqual(
            DropStripGeometry.screenFrame(CGRect(x: 100, y: 0, width: 1412, height: 8), mainHeight: 982),
            CGRect(x: 100, y: 974, width: 1412, height: 8))
        // A screen above the main one.
        XCTAssertEqual(
            DropStripGeometry.screenFrame(CGRect(x: 0, y: -300, width: 8, height: 100), mainHeight: 982),
            CGRect(x: 0, y: 1182, width: 8, height: 100))
    }
}

final class OutgoingDragTests: XCTestCase {
    func testOffersOnEnteringAndEndsOnLeaving() throws {
        var drag = OutgoingDrag()
        let token = try XCTUnwrap(drag.entered())
        XCTAssertNil(drag.answered(token: token, id: "d1"))
        XCTAssertEqual(drag.dragID, "d1")
        // Moving about on the strip asks nothing more.
        XCTAssertNil(drag.entered())
        XCTAssertEqual(drag.left(), "d1")
        XCTAssertNil(drag.left())
        XCTAssertEqual(drag.phase, .idle)
    }

    func testLeavingBeforeTheAnswerEndsTheDragOnceItComes() throws {
        var drag = OutgoingDrag()
        let token = try XCTUnwrap(drag.entered())
        XCTAssertNil(drag.left())
        XCTAssertEqual(drag.answered(token: token, id: "d1"), "d1")
        XCTAssertNil(drag.dragID)
    }

    func testComingBackBeforeTheAnswerKeepsTheDrag() throws {
        var drag = OutgoingDrag()
        let token = try XCTUnwrap(drag.entered())
        XCTAssertNil(drag.left())
        XCTAssertNil(drag.entered())
        XCTAssertNil(drag.answered(token: token, id: "d1"))
        XCTAssertEqual(drag.dragID, "d1")
    }

    func testADropOnTheStripSaysNothing() throws {
        // As the cursor crosses, the service lets go of the button over the
        // strip: the drag goes on elsewhere, and the strip's end is no exit.
        var drag = OutgoingDrag()
        let token = try XCTUnwrap(drag.entered())
        _ = drag.answered(token: token, id: "d1")
        drag.dropped()
        XCTAssertNil(drag.left())
        XCTAssertEqual(drag.phase, .idle)

        // Dropped before the answer came: the service saw the release.
        let next = try XCTUnwrap(drag.entered())
        drag.dropped()
        XCTAssertNil(drag.answered(token: next, id: "d2"))
        XCTAssertEqual(drag.phase, .idle)
    }

    func testANewDragAfterADropBeforeItsAnswerIsOfferedAnew() throws {
        var drag = OutgoingDrag()
        let first = try XCTUnwrap(drag.entered())
        drag.dropped()
        let second = try XCTUnwrap(drag.entered())
        XCTAssertNotEqual(first, second)
        // The first answer is stale; the service put the second in its place.
        XCTAssertNil(drag.answered(token: first, id: "d1"))
        XCTAssertNil(drag.answered(token: second, id: "d2"))
        XCTAssertEqual(drag.dragID, "d2")
    }

    func testARefusalOrTheServiceEndingItLeavesNothingToEnd() throws {
        var drag = OutgoingDrag()
        let token = try XCTUnwrap(drag.entered())
        XCTAssertNil(drag.answered(token: token, id: nil))
        XCTAssertNil(drag.left())
        XCTAssertNotNil(drag.entered())

        var ended = OutgoingDrag()
        let again = try XCTUnwrap(ended.entered())
        _ = ended.answered(token: again, id: "d1")
        ended.ended("other")
        XCTAssertEqual(ended.dragID, "d1")
        ended.ended("d1")
        XCTAssertNil(ended.left())
    }
}

final class PromisedDragTests: XCTestCase {
    let start = Date(timeIntervalSince1970: 1_000_000)
    let folder = URL(fileURLWithPath: "/Users/me/Desktop", isDirectory: true)

    func row(
        _ id: String, _ name: String, index: UInt32, folder: Bool = false,
        _ state: Inputweave_App_V1_TransferState = .transferring, path: String = "", drag: String = "d1"
    ) -> FileTransfer {
        var row = FileTransfer()
        row.transferID = id
        row.name = name
        row.batchIndex = index
        row.folder = folder
        row.state = state
        row.path = path
        row.dragID = drag
        return row
    }

    func offered(_ names: [(String, Bool)] = [("Photos", true), ("Notes.txt", false)]) -> PromisedDrag {
        PromisedDrag(id: "d1", items: names.map { PromisedItem(name: $0.0, folder: $0.1) })
    }

    func testNotDroppedIsDeclined() {
        var drag = offered()
        XCTAssertTrue(drag.droppable)
        XCTAssertEqual(drag.sessionEnded(dropped: false, now: start), [.decline])
        XCTAssertTrue(drag.finished)
    }

    func testAcceptsOnceAndKeepsEachPromiseAsItsItemArrives() {
        var drag = offered()
        XCTAssertEqual(drag.sessionEnded(dropped: true, now: start), [])
        XCTAssertEqual(
            drag.promise(index: 1, url: folder.appending(path: "Notes.txt"), now: start),
            [.accept(destination: "/Users/me/Desktop")])
        XCTAssertEqual(drag.promise(index: 0, url: folder.appending(path: "Photos"), now: start), [])
        XCTAssertEqual(drag.accepted(true, now: start), [])

        // The other computer lists a symbolic link first, which does not go:
        // the transfers' places in the batch skip it.
        var rows = [row("f", "Photos", index: 1, folder: true), row("n", "Notes.txt", index: 2)]
        XCTAssertEqual(drag.transfers(rows, now: start), [])
        rows[1].state = .done
        rows[1].path = "/Users/me/Desktop/Notes.txt"
        XCTAssertEqual(drag.transfers(rows, now: start), [.fulfill(index: 1, path: "/Users/me/Desktop/Notes.txt")])
        // A folder some of whose files failed is done all the same.
        rows[0].state = .done
        rows[0].filesFailed = 2
        rows[0].path = "/Users/me/Desktop/Photos (1)"
        XCTAssertEqual(drag.transfers(rows, now: start), [.fulfill(index: 0, path: "/Users/me/Desktop/Photos (1)")])
        XCTAssertTrue(drag.finished)
    }

    func testAnItemThatFailsBreaksItsPromise() {
        var drag = offered([("Notes.txt", false)])
        _ = drag.sessionEnded(dropped: true, now: start)
        _ = drag.promise(index: 0, url: folder.appending(path: "Notes.txt"), now: start)
        _ = drag.accepted(true, now: start)
        XCTAssertEqual(drag.transfers([row("n", "Notes.txt", index: 0, .failed)], now: start), [.fail(index: 0)])
        XCTAssertTrue(drag.finished)
    }

    func testOtherDragsAndOutgoingTransfersAreNotItsOwn() {
        var drag = offered([("Notes.txt", false)])
        _ = drag.promise(index: 0, url: folder.appending(path: "Notes.txt"), now: start)
        _ = drag.accepted(true, now: start)
        var outgoing = row("n", "Notes.txt", index: 0, .done)
        outgoing.outgoing = true
        XCTAssertEqual(drag.transfers([row("x", "Notes.txt", index: 0, .done, drag: "d2"), outgoing], now: start), [])
    }

    func testMatchesByNameThenByOrderOnceAllAreThere() {
        let items = [PromisedItem(name: "a:b.txt", folder: false), PromisedItem(name: "c.txt", folder: false), PromisedItem(name: "c.txt", folder: false)]
        // Saved under a name this computer allows, which differs.
        let rows = [row("1", "a_b.txt", index: 0), row("2", "c.txt", index: 1), row("3", "c.txt", index: 2)]
        let partial = PromisedDrag.match(items: items, rows: Array(rows.dropFirst()))
        XCTAssertEqual(partial[1]?.transferID, "2")
        XCTAssertEqual(partial[2]?.transferID, "3")
        XCTAssertNil(partial[0])
        XCTAssertEqual(PromisedDrag.match(items: items, rows: rows)[0]?.transferID, "1")
    }

    func testMovesAnItemToTheNameTheSystemChose() {
        let keepBoth = PromisedDrag.move(arrived: "/Users/me/Desktop/Photos (1)", promised: folder.appending(path: "Photos 2"))
        XCTAssertEqual(keepBoth?.from.path, "/Users/me/Desktop/Photos (1)")
        XCTAssertEqual(keepBoth?.to.path, "/Users/me/Desktop/Photos 2")
        XCTAssertNil(PromisedDrag.move(arrived: "/Users/me/Desktop/Photos", promised: folder.appending(path: "Photos")))
        XCTAssertNil(PromisedDrag.move(arrived: "/Users/me/Downloads/Photos", promised: folder.appending(path: "Photos 2")))
        XCTAssertNil(PromisedDrag.move(arrived: "", promised: folder.appending(path: "Photos")))
    }

    func testWithdrawnTheDragCannotBeDroppedAndIsNotDeclined() {
        var drag = offered()
        XCTAssertEqual(drag.ended(), [])
        XCTAssertFalse(drag.droppable)
        // The system drag still ends somewhere; the offer is gone already.
        XCTAssertEqual(drag.sessionEnded(dropped: true, now: start), [])
        XCTAssertEqual(drag.promise(index: 0, url: folder.appending(path: "Photos"), now: start), [.fail(index: 0)])
        XCTAssertTrue(drag.finished)
    }

    func testDroppedWhereNothingAsksForTheItemsIsDeclined() {
        var drag = offered()
        _ = drag.sessionEnded(dropped: true, now: start)
        XCTAssertEqual(drag.tick(now: start + 1), [])
        XCTAssertEqual(drag.tick(now: start + PromisedDrag.askedWithin), [.decline])
        XCTAssertEqual(drag.promise(index: 0, url: folder.appending(path: "Photos"), now: start + 6), [.fail(index: 0)])
    }

    func testARefusedFolderBreaksEveryPromise() {
        var drag = offered()
        _ = drag.promise(index: 0, url: folder.appending(path: "Photos"), now: start)
        XCTAssertEqual(drag.accepted(false, now: start), [.fail(index: 0)])
        XCTAssertEqual(drag.promise(index: 1, url: folder.appending(path: "Notes.txt"), now: start), [.fail(index: 1)])
        XCTAssertTrue(drag.finished)
    }

    func testGivesUpOnItemsThatNeverCome() {
        // Nothing at all came.
        var none = offered()
        _ = none.promise(index: 0, url: folder.appending(path: "Photos"), now: start)
        _ = none.accepted(true, now: start)
        XCTAssertEqual(none.tick(now: start + PromisedDrag.quietFor), [])
        XCTAssertEqual(none.tick(now: start + PromisedDrag.firstWithin), [.fail(index: 0)])

        // One came and ended; the other never showed up.
        var half = offered()
        _ = half.promise(index: 0, url: folder.appending(path: "Photos"), now: start)
        _ = half.promise(index: 1, url: folder.appending(path: "Notes.txt"), now: start)
        _ = half.accepted(true, now: start)
        let later = start + 60
        XCTAssertEqual(
            half.transfers([row("n", "Notes.txt", index: 1, .done, path: "/Users/me/Desktop/Notes.txt")], now: later),
            [.fulfill(index: 1, path: "/Users/me/Desktop/Notes.txt")])
        XCTAssertEqual(half.tick(now: later + PromisedDrag.quietFor - 1), [])
        XCTAssertEqual(half.tick(now: later + PromisedDrag.quietFor), [.fail(index: 0)])
        XCTAssertTrue(half.finished)
    }

    func testTheHUDWords() {
        XCTAssertEqual(DragText.dragging(items: 3, device: "Windows PC"), "Dragging 3 items to “Windows PC”")
        XCTAssertEqual(DragText.dragging(items: 1, device: "Windows PC"), "Dragging 1 item to “Windows PC”")
        XCTAssertEqual(DragText.dropHere, "Drop the items where you want them")
        XCTAssertEqual(DragText.acceptRefused("invalid_folder"), "The items can't be saved in this folder.")
    }
}
