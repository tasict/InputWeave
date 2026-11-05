import Foundation

/// The source side of dragging files to another computer
/// (docs/architecture.md §9), free of AppKit: what to tell the
/// background service as a drag of files enters the strips along the
/// seams and leaves them.
///
/// The drag is offered (`BeginDrag`) as soon as it enters a strip, before
/// the cursor reaches the edge, because the service lists folders first.
/// Leaving the strip for this computer's screen ends it (`EndDrag`).
/// Dropped on a strip, nothing is said: either the user let go there,
/// which the service sees, or the service let go of the button there as
/// the cursor crossed, and the drag goes on on the other computer.
struct OutgoingDrag: Equatable {
    enum Phase: Equatable {
        case idle
        /// `BeginDrag` was sent and not answered yet. `left` once the drag
        /// left the strip meanwhile, `dropped` once it was dropped on it.
        case beginning(token: Int, left: Bool, dropped: Bool)
        case begun(id: String)
    }

    private(set) var phase: Phase = .idle
    private var tokens = 0

    /// The drag going on from here, once the service answered.
    var dragID: String? {
        if case .begun(let id) = phase { id } else { nil }
    }

    /// A drag of files entered a strip. Returns a token when `BeginDrag` is
    /// to be sent, to pass back with its answer.
    mutating func entered() -> Int? {
        switch phase {
        case .idle, .beginning(_, _, true):
            // A drag dropped before its answer came is over; this is
            // another, and the service puts it in place of the first.
            tokens += 1
            phase = .beginning(token: tokens, left: false, dropped: false)
            return tokens
        case .beginning(let token, _, false):
            // Back before the answer came.
            phase = .beginning(token: token, left: false, dropped: false)
            return nil
        case .begun:
            return nil
        }
    }

    /// The drag left the strips for this computer's screen, or ended
    /// without being dropped on one. Returns the drag to end.
    mutating func left() -> String? {
        switch phase {
        case .idle:
            return nil
        case .beginning(let token, _, let dropped):
            phase = .beginning(token: token, left: true, dropped: dropped)
            return nil
        case .begun(let id):
            phase = .idle
            return id
        }
    }

    /// The drag was dropped on a strip.
    mutating func dropped() {
        if case .beginning(let token, let left, _) = phase {
            phase = .beginning(token: token, left: left, dropped: true)
        } else {
            phase = .idle
        }
    }

    /// The answer to the `BeginDrag` sent with `token`: the drag's id, or
    /// nil when the service refused it. Returns a drag to end at once, as
    /// it left the strip while the service was answering.
    mutating func answered(token: Int, id: String?) -> String? {
        guard case .beginning(token, let left, let dropped) = phase else { return nil }
        guard let id, !dropped else {
            phase = .idle
            return nil
        }
        if left {
            phase = .idle
            return id
        }
        phase = .begun(id: id)
        return nil
    }

    /// The service says drag `id` ended.
    mutating func ended(_ id: String) {
        if dragID == id { phase = .idle }
    }
}

/// An item of a drag offered here, as `DragArrived` lists it.
struct PromisedItem: Equatable {
    let name: String
    let folder: Bool
}

/// A drag another computer offers here (docs/architecture.md §9), from
/// `DragArrived` until each item is where the user dropped it, free of
/// AppKit. The app starts a system drag with one promised file per item,
/// passes in what happens, and carries out the returned steps.
///
/// Dropped, the system asks for each item at a URL in the folder it went
/// to: the first such request tells the service the folder
/// (`AcceptDrag`), and each item's promise is kept once its transfer ends.
/// Its transfers carry the drag's id, but only items that go are offered,
/// while a transfer's `batch_index` counts every item the other computer
/// listed; so items and transfers are matched by name, and by order once
/// all transfers are there.
struct PromisedDrag {
    enum Step: Equatable {
        /// Tell the service the items were dropped in `destination`.
        case accept(destination: String)
        /// Tell the service they were not dropped here.
        case decline
        /// Item `index` arrived at `path` (empty if unknown); the promise is
        /// kept.
        case fulfill(index: Int, path: String)
        /// Item `index` will not arrive; the promise is broken.
        case fail(index: Int)
    }

    enum Phase: Equatable {
        /// The system drag is going on.
        case offered
        /// Dropped somewhere that takes it, waiting to be asked for the
        /// items, since `at`.
        case dropped(at: Date)
        /// `AcceptDrag` sent.
        case accepting
        /// Accepted at `at`: the transfers come.
        case accepted(at: Date)
        /// Over: declined, withdrawn, refused, or every item settled.
        case over
    }

    /// How long a drop may go without the system asking for the items.
    static let askedWithin: TimeInterval = 5
    /// How long items may go without a transfer of theirs showing up while
    /// none of the drag's transfers is going: before any shows up, as long
    /// as the service keeps the folder for the drag (they may wait behind
    /// other files sent to this computer), and after the last one ended.
    static let firstWithin: TimeInterval = 10 * 60
    static let quietFor: TimeInterval = 2 * 60

    let id: String
    let items: [PromisedItem]
    private(set) var phase: Phase = .offered
    /// Where the system asked for each item, by index.
    private(set) var promised: [Int: URL] = [:]
    private var settled: Set<Int> = []
    private var rows: [FileTransfer] = []
    /// When the drag's transfers last changed.
    private var lastNews: Date = .distantPast
    /// The source took the offer back, or it expired.
    private(set) var withdrawn = false

    init(id: String, items: [PromisedItem]) {
        self.id = id
        self.items = items
    }

    /// Whether the system drag may still be dropped somewhere.
    var droppable: Bool { phase == .offered && !withdrawn }

    /// Whether nothing is left to do: no promise waits to be kept.
    var finished: Bool {
        phase == .over && promised.keys.allSatisfy { settled.contains($0) }
    }

    /// The system drag ended: dropped where it is taken, or not.
    mutating func sessionEnded(dropped: Bool, now: Date) -> [Step] {
        switch phase {
        case .offered where dropped:
            phase = .dropped(at: now)
            return []
        case .offered:
            phase = .over
            return withdrawn ? [] : [.decline]
        default:
            return []
        }
    }

    /// The system asks for item `index` at `url`.
    mutating func promise(index: Int, url: URL, now: Date) -> [Step] {
        guard items.indices.contains(index), !settled.contains(index) else { return [] }
        promised[index] = url
        switch phase {
        case .over:
            settled.insert(index)
            return [.fail(index: index)]
        case .offered, .dropped:
            phase = .accepting
            lastNews = now
            return [.accept(destination: url.deletingLastPathComponent().path)]
                + settle()
        case .accepting, .accepted:
            return settle()
        }
    }

    /// The service's answer to `AcceptDrag`.
    mutating func accepted(_ ok: Bool, now: Date) -> [Step] {
        guard phase == .accepting else { return [] }
        if ok {
            phase = .accepted(at: now)
            lastNews = now
            return settle()
        }
        phase = .over
        return failRest()
    }

    /// The transfers changed; this drag's are those carrying its id.
    mutating func transfers(_ all: [FileTransfer], now: Date) -> [Step] {
        let mine = all.filter { $0.dragID == id && !$0.outgoing }
        if mine != rows {
            rows = mine
            lastNews = now
        }
        return settle()
    }

    /// The service says the offer ended here: taken back, or expired.
    mutating func ended() -> [Step] {
        switch phase {
        case .offered, .dropped:
            withdrawn = true
            phase = .over
            return failRest()
        default:
            // Accepted: the transfers tell how it goes.
            return []
        }
    }

    /// Gives up on what has waited too long by `now`.
    mutating func tick(now: Date) -> [Step] {
        switch phase {
        case .dropped(let at) where now.timeIntervalSince(at) >= Self.askedWithin:
            // Taken by something that never asked for the items.
            phase = .over
            return [.decline]
        case .accepted(let at):
            guard !rows.contains(where: { !$0.state.isFinished }) else { return [] }
            let waited = rows.isEmpty ? now.timeIntervalSince(at) : now.timeIntervalSince(max(at, lastNews))
            guard waited >= (rows.isEmpty ? Self.firstWithin : Self.quietFor) else { return [] }
            phase = .over
            return failRest()
        default:
            return []
        }
    }

    /// Keeps or breaks the promises whose items' transfers ended.
    private mutating func settle() -> [Step] {
        let matched = Self.match(items: items, rows: rows)
        var steps: [Step] = []
        for index in promised.keys.sorted() where !settled.contains(index) {
            guard let row = matched[index], row.state.isFinished else { continue }
            settled.insert(index)
            // A folder is done when any of its files arrived.
            steps.append(row.state == .done ? .fulfill(index: index, path: row.path) : .fail(index: index))
        }
        if case .accepted = phase, settled.count == items.count {
            phase = .over
        }
        return steps
    }

    private mutating func failRest() -> [Step] {
        var steps: [Step] = []
        for index in promised.keys.sorted() where !settled.contains(index) {
            settled.insert(index)
            steps.append(.fail(index: index))
        }
        return steps
    }

    /// Which transfer is which item: the first unmatched one of the same
    /// name and kind, in the order the other computer listed them; once
    /// there is one transfer per item, the rest in order.
    static func match(items: [PromisedItem], rows: [FileTransfer]) -> [Int: FileTransfer] {
        var byID: [String: FileTransfer] = [:]
        for row in rows { byID[row.transferID] = row }
        let ordered = byID.values.sorted { ($0.batchIndex, $0.transferID) < ($1.batchIndex, $1.transferID) }
        var used = Set<String>()
        var matched: [Int: FileTransfer] = [:]
        for (index, item) in items.enumerated() {
            if let row = ordered.first(where: { !used.contains($0.transferID) && $0.name == item.name && $0.folder == item.folder }) {
                used.insert(row.transferID)
                matched[index] = row
            }
        }
        if ordered.count == items.count {
            let rest = ordered.filter { !used.contains($0.transferID) }
            for (index, row) in zip(items.indices.filter { matched[$0] == nil }, rest) {
                matched[index] = row
            }
        }
        return matched
    }

    /// Where an item that arrived at `arrived` is to be moved, so that it is
    /// at the URL the system asked for (which may have another name, such
    /// as after Keep Both); nil when it is there already, or when the two
    /// are in different folders.
    static func move(arrived: String, promised: URL) -> (from: URL, to: URL)? {
        guard !arrived.isEmpty else { return nil }
        let from = URL(fileURLWithPath: arrived).standardizedFileURL
        let to = promised.standardizedFileURL
        guard from.lastPathComponent != to.lastPathComponent,
              from.deletingLastPathComponent().path == to.deletingLastPathComponent().path
        else { return nil }
        return (from, to)
    }
}

/// The words for dragging files across.
enum DragText {
    /// While files from here are dragged over another computer.
    static func dragging(items: UInt32, device: String) -> String {
        items == 1
            ? String(localized: "Dragging 1 item to “\(device)”")
            : String(localized: "Dragging \(items) items to “\(device)”")
    }

    /// The other computer took the drag: the files go.
    static func sending(device: String) -> String {
        String(localized: "Sending to “\(device)”…")
    }

    /// The other computer's user did not drop it there.
    static func declined(device: String) -> String {
        String(localized: "“\(device)” did not take the items.")
    }

    /// A drag offered here.
    static var dropHere: String {
        String(localized: "Drop the items where you want them")
    }

    /// Too much to send at once (`too_many_items`, `too_large`).
    static var limits: String {
        String(localized: "Up to 1,000 items, 10,000 files or 100 GB can be sent at once. Send them in smaller batches.")
    }

    /// The service refused the folder the items were dropped in.
    static func acceptRefused(_ code: String) -> String {
        switch code {
        case "invalid_folder": String(localized: "The items can't be saved in this folder.")
        case "not_found": String(localized: "The drag has already ended.")
        default: String(localized: "The items could not be received.")
        }
    }
}
