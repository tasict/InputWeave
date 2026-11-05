import AppKit
import XCTest

@MainActor
final class DesktopPictureTests: XCTestCase {
    /// A JPEG of `width` × `height` pixels in one color, made here rather
    /// than read from the user's desktop.
    static func jpeg(width: Int, height: Int, color: NSColor = .systemOrange) throws -> Data {
        let rep = try XCTUnwrap(NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height, bitsPerSample: 8, samplesPerPixel: 4,
            hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0))
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        color.setFill()
        NSRect(x: 0, y: 0, width: width, height: height).fill()
        NSGraphicsContext.restoreGraphicsState()
        return try XCTUnwrap(rep.representation(using: .jpeg, properties: [.compressionFactor: 0.7]))
    }

    func testDecodesAJPEGOfAnyShape() throws {
        let wide = try XCTUnwrap(DesktopPicture.decode(Self.jpeg(width: 320, height: 180)))
        XCTAssertEqual(wide.size, CGSize(width: 320, height: 180))
        let tall = try XCTUnwrap(DesktopPicture.decode(Self.jpeg(width: 90, height: 320)))
        XCTAssertEqual(tall.size, CGSize(width: 90, height: 320))
    }

    func testRefusesWhatIsNotAPicture() throws {
        let jpeg = try Self.jpeg(width: 64, height: 36)
        XCTAssertNil(DesktopPicture.decode(Data()))
        XCTAssertNil(DesktopPicture.decode(Data((0..<200).map { UInt8(truncatingIfNeeded: $0 &* 37) })))
        // A JPEG's first bytes, then garbage: NSImage(data:) makes an image
        // of it, without representations or size.
        XCTAssertNil(DesktopPicture.decode(Data([0xFF, 0xD8, 0xFF, 0xE0] + (0..<200).map { UInt8(truncatingIfNeeded: $0 &* 37) })))
        XCTAssertNil(DesktopPicture.decode(jpeg.prefix(40)))
        XCTAssertNil(DesktopPicture.decode(jpeg.prefix(jpeg.count / 2)))
        // Another format, even a valid one, is not decoded.
        let rep = try XCTUnwrap(NSBitmapImageRep(data: jpeg))
        XCTAssertNil(DesktopPicture.decode(try XCTUnwrap(rep.representation(using: .png, properties: [:]))))
        // Nor are bytes far over what the background service passes on.
        XCTAssertNil(DesktopPicture.decode(jpeg + Data(count: DesktopPicture.maxBytes)))
    }

    func testDecodesEachScreensPictureOnceUntilItChanges() throws {
        let cache = DesktopPictureCache()
        let first = try Self.jpeg(width: 64, height: 36, color: .systemBlue)
        let second = try Self.jpeg(width: 64, height: 36, color: .systemGreen)
        XCTAssertNotEqual(first, second)

        let picture = try XCTUnwrap(cache.picture(device: "mac", screen: "builtin", data: first))
        XCTAssertTrue(cache.picture(device: "mac", screen: "builtin", data: first) === picture)
        // The same bytes received again, in a new message.
        XCTAssertTrue(cache.picture(device: "mac", screen: "builtin", data: Data(first)) === picture)
        XCTAssertEqual(cache.decodes, 1)

        // Another computer's screen with the same id is its own entry.
        let other = try XCTUnwrap(cache.picture(device: "mini", screen: "builtin", data: second))
        XCTAssertFalse(other === picture)
        XCTAssertTrue(cache.picture(device: "mac", screen: "builtin", data: first) === picture)

        // A new picture replaces the old; none clears it.
        let changed = try XCTUnwrap(cache.picture(device: "mac", screen: "builtin", data: second))
        XCTAssertFalse(changed === picture)
        XCTAssertNil(cache.picture(device: "mac", screen: "builtin", data: Data()))
        XCTAssertEqual(cache.decodes, 3)

        // Bytes that do not decode give no picture, and are not decoded
        // again on the next redraw.
        let garbage = Data([0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3])
        XCTAssertNil(cache.picture(device: "pc", screen: "main", data: garbage))
        XCTAssertNil(cache.picture(device: "pc", screen: "main", data: garbage))
        XCTAssertEqual(cache.decodes, 4)
    }
}
