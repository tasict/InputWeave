import AppKit

/// The desktop pictures in the arrangement (`Screen.previewJpeg`), decoded
/// for the screen tiles. Other computers send these bytes, so they are
/// checked before and after decoding, and nothing is assumed about the
/// picture's size or shape.
enum DesktopPicture {
    /// The background service passes on previews of at most 96 KB; anything
    /// far larger is not one.
    static let maxBytes = 512 * 1024
    /// Previews are at most 320 pixels on their long edge. A JPEG claiming
    /// far more would take memory out of all proportion once drawn.
    static let maxPixels = 4096

    /// The picture in `data`, or `nil` when there is none: empty, not a
    /// JPEG, or bytes that do not decode into a picture with a size.
    static func decode(_ data: Data) -> NSImage? {
        // JPEG only, so no other image format's decoder sees these bytes.
        guard !data.isEmpty, data.count <= maxBytes, data.starts(with: [0xFF, 0xD8, 0xFF]) else { return nil }
        // Broken JPEGs still make an image, without representations or size.
        guard let image = NSImage(data: data), image.isValid,
              image.size.width > 0, image.size.height > 0,
              image.size.width.isFinite, image.size.height.isFinite,
              !image.representations.isEmpty,
              image.representations.allSatisfy({ rep in
                  (1...maxPixels).contains(rep.pixelsWide) && (1...maxPixels).contains(rep.pixelsHigh)
              })
        else { return nil }
        return image
    }
}

/// Decoded desktop pictures by screen, so a tile decodes its picture once
/// rather than on every redraw. A screen's entry is replaced when its bytes
/// change; empty bytes or ones that do not decode give no picture.
@MainActor
final class DesktopPictureCache {
    static let shared = DesktopPictureCache()

    /// Screen ids come from each computer, so two computers can share one.
    private struct Key: Hashable {
        let device: String
        let screen: String
    }

    private struct Entry {
        /// The bytes the picture came from. Kept, not copied, to tell when
        /// they change; the comparison stops at a different count.
        let data: Data
        let image: NSImage?
    }

    private var entries: [Key: Entry] = [:]
    /// Far more screens than any group of computers has; past it, the cache
    /// starts over rather than keep screens that are long gone.
    private let capacity = 64

    /// How many times bytes were decoded, for tests.
    private(set) var decodes = 0

    func picture(device: String, screen: String, data: Data) -> NSImage? {
        let key = Key(device: device, screen: screen)
        if let entry = entries[key], entry.data == data { return entry.image }
        if data.isEmpty {
            entries[key] = nil
            return nil
        }
        if entries.count >= capacity { entries.removeAll() }
        decodes += 1
        let image = DesktopPicture.decode(data)
        entries[key] = Entry(data: data, image: image)
        return image
    }
}
