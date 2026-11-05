import Foundation
import SwiftProtobuf

/// The framing on the background service's socket, as in iw-proto's
/// `frame` module: a big-endian u32 length, then one protobuf message.
enum Frame {
    /// The service refuses larger frames, and so does the app.
    static let maxLength = 4 * 1024 * 1024
    static let headerLength = 4

    enum Failure: Error, Equatable {
        case tooLarge(Int)
    }

    static func encode(_ message: some SwiftProtobuf.Message) throws -> Data {
        let body: Data = try message.serializedData()
        guard body.count <= maxLength else { throw Failure.tooLarge(body.count) }
        var frame = Data(capacity: headerLength + body.count)
        withUnsafeBytes(of: UInt32(body.count).bigEndian) { frame.append(contentsOf: $0) }
        frame.append(body)
        return frame
    }

    /// Removes one complete frame from the front of `buffer` and decodes
    /// it, or returns nil while `buffer` does not hold a whole frame yet.
    static func decode<M: SwiftProtobuf.Message>(_ type: M.Type, from buffer: inout Data) throws -> M? {
        guard buffer.count >= headerLength else { return nil }
        let start = buffer.startIndex
        let length = buffer[start..<start + headerLength].reduce(0) { $0 << 8 | Int($1) }
        guard length <= maxLength else { throw Failure.tooLarge(length) }
        guard buffer.count >= headerLength + length else { return nil }
        let body = buffer[(start + headerLength)..<(start + headerLength + length)]
        buffer = Data(buffer[(start + headerLength + length)...])
        return try M(serializedBytes: body)
    }
}
