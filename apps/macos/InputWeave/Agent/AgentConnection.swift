import Foundation
import Network

/// One connection to the background service's Unix socket, carrying
/// framed messages.
final class AgentConnection: Sendable {
    enum Failure: Error {
        case closed
        case unavailable(String)
    }

    private let connection: NWConnection
    private let queue = DispatchQueue(label: "cc.ctsite.inputweave.ipc")

    init(socketPath: String) {
        // A stream socket: Network.framework runs Unix domain sockets with
        // the TCP parameters.
        connection = NWConnection(to: .unix(path: socketPath), using: .tcp)
    }

    /// Connects, failing at once if nothing listens on the socket.
    func open() async throws {
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
            let once = Once()
            connection.stateUpdateHandler = { state in
                switch state {
                case .ready:
                    once.run { continuation.resume() }
                case .failed(let error), .waiting(let error):
                    once.run { continuation.resume(throwing: Failure.unavailable(error.localizedDescription)) }
                case .cancelled:
                    once.run { continuation.resume(throwing: Failure.closed) }
                default:
                    break
                }
            }
            connection.start(queue: queue)
        }
    }

    func send(_ message: Inputweave_App_V1_ClientMessage) async throws {
        let frame = try Frame.encode(message)
        try await withCheckedThrowingContinuation { (continuation: CheckedContinuation<Void, any Error>) in
            connection.send(content: frame, completion: .contentProcessed { error in
                if let error {
                    continuation.resume(throwing: error)
                } else {
                    continuation.resume()
                }
            })
        }
    }

    /// Everything the service sends, until the connection closes.
    func messages() -> AsyncThrowingStream<Inputweave_App_V1_AgentMessage, any Error> {
        AsyncThrowingStream { continuation in
            let reader = Reader(connection: connection, continuation: continuation)
            reader.receive()
            continuation.onTermination = { [connection] _ in connection.cancel() }
        }
    }

    func close() {
        connection.cancel()
    }
}

/// Reads frames off the connection on its queue.
private final class Reader: @unchecked Sendable {
    // Only touched from the connection's queue, one callback at a time.
    private var buffer = Data()
    private let connection: NWConnection
    private let continuation: AsyncThrowingStream<Inputweave_App_V1_AgentMessage, any Error>.Continuation

    init(
        connection: NWConnection,
        continuation: AsyncThrowingStream<Inputweave_App_V1_AgentMessage, any Error>.Continuation
    ) {
        self.connection = connection
        self.continuation = continuation
    }

    func receive() {
        connection.receive(minimumIncompleteLength: 1, maximumLength: 64 * 1024) { data, _, isComplete, error in
            if let data {
                self.buffer.append(data)
                do {
                    while let message = try Frame.decode(Inputweave_App_V1_AgentMessage.self, from: &self.buffer) {
                        self.continuation.yield(message)
                    }
                } catch {
                    self.continuation.finish(throwing: error)
                    return
                }
            }
            if let error {
                self.continuation.finish(throwing: error)
            } else if isComplete {
                self.continuation.finish(throwing: AgentConnection.Failure.closed)
            } else {
                self.receive()
            }
        }
    }
}

/// Runs a closure at most once, from whichever thread gets there first.
private final class Once: @unchecked Sendable {
    private let lock = NSLock()
    private var done = false

    func run(_ body: () -> Void) {
        lock.lock()
        defer { lock.unlock() }
        guard !done else { return }
        done = true
        body()
    }
}
