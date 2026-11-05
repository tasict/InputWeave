import AppKit
import UniformTypeIdentifiers

/// Drags of files another computer offers here (docs/architecture.md §9).
///
/// The cursor came here with the button held, which the background service
/// pressed again here. The app starts a system drag where the cursor is,
/// from a small invisible panel, with one promised file per item (a folder
/// is promised as a folder). Dropped where files go, the system asks for
/// the items at URLs in that folder: the app tells the service the folder
/// (`AcceptDrag`), the files arrive there, and each promise is kept once
/// its item's transfer ends, after renaming the item if the system chose
/// another name for it. Not dropped anywhere, the drag is declined
/// (`DeclineDrag`). `PromisedDrag` decides; this carries it out.
@MainActor
final class DragPromises {
    private let model: AgentModel
    private let hud: HUDController
    private var drags: [String: PromisedDrag] = [:]
    /// The system's completion handlers for the promises asked for, by
    /// drag and item.
    private var handlers: [String: [Int: PromiseHandler]] = [:]
    /// Each drag's source view, kept as long as its promises may be asked
    /// for: the providers hold their delegate weakly.
    private var sources: [String: PromiseSourceView] = [:]
    private var panel: NSPanel?
    private var ticker: Task<Void, Never>?
    /// Where the system asks for the items, off the main thread, as their
    /// writing waits for the transfers.
    fileprivate let writes: OperationQueue = {
        let queue = OperationQueue()
        queue.name = "cc.ctsite.inputweave.promises"
        queue.qualityOfService = .userInitiated
        return queue
    }()

    init(model: AgentModel, hud: HUDController) {
        self.model = model
        self.hud = hud
    }

    /// Another computer offers a drag here.
    func arrived(_ arrived: DragArrived) {
        let id = arrived.dragID
        let items = arrived.items.map { PromisedItem(name: $0.name, folder: $0.folder) }
        guard !items.isEmpty, drags[id] == nil else { return }
        drags[id] = PromisedDrag(id: id, items: items)
        hud.note(DragText.dropHere)
        startSession(id: id, items: arrived.items)
        startTicking()
    }

    /// An offer here ended: taken back, or expired.
    func ended(_ ended: DragEnded) {
        update(ended.dragID) { $0.ended() }
    }

    /// Looks at the transfers; called whenever the model changes.
    func update() {
        guard !drags.isEmpty else { return }
        let transfers = model.transfers.transfers
        let now = Date.now
        for id in drags.keys {
            update(id) { $0.transfers(transfers, now: now) }
        }
    }

    // MARK: - The system drag

    private func startSession(id: String, items: [Inputweave_App_V1_DraggedItem]) {
        panel?.orderOut(nil)
        let mouse = NSEvent.mouseLocation
        let side: CGFloat = 32
        let panel = NSPanel(
            contentRect: CGRect(x: mouse.x - side / 2, y: mouse.y - side / 2, width: side, height: side),
            styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: false)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.level = .popUpMenu
        panel.ignoresMouseEvents = true
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient, .ignoresCycle]
        let view = PromiseSourceView(frame: CGRect(x: 0, y: 0, width: side, height: side), dragID: id, owner: self)
        panel.contentView = view
        panel.orderFrontRegardless()
        self.panel = panel
        sources[id] = view

        // The button is down already, pressed by the service; the drag
        // starts from a press made up here, at the cursor.
        let location = panel.convertPoint(fromScreen: mouse)
        guard let press = NSEvent.mouseEvent(
            with: .leftMouseDown, location: location, modifierFlags: [],
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: panel.windowNumber,
            context: nil, eventNumber: 0, clickCount: 1, pressure: 1)
        else {
            sessionEnded(id, dropped: false)
            return
        }
        let dragged = items.enumerated().map { index, item in
            let provider = NSFilePromiseProvider(fileType: Self.fileType(item), delegate: view)
            provider.userInfo = PromiseInfo(dragID: id, index: index, name: item.name)
            let dragging = NSDraggingItem(pasteboardWriter: provider)
            // A small pile under the cursor.
            let shift = CGFloat(min(index, 3)) * 3
            dragging.setDraggingFrame(
                CGRect(x: shift, y: -shift, width: side, height: side), contents: Self.icon(item))
            return dragging
        }
        let session = view.beginDraggingSession(with: dragged, event: press, source: view)
        session.animatesToStartingPositionsOnCancelOrFail = false
        session.draggingFormation = .pile
    }

    /// The promised file's type, which must be data or a folder.
    private static func fileType(_ item: Inputweave_App_V1_DraggedItem) -> String {
        if item.folder { return UTType.folder.identifier }
        let ext = (item.name as NSString).pathExtension
        if let type = UTType(filenameExtension: ext, conformingTo: .data),
           type.conforms(to: .data) || type.conforms(to: .directory) {
            return type.identifier
        }
        return UTType.data.identifier
    }

    private static func icon(_ item: Inputweave_App_V1_DraggedItem) -> NSImage {
        NSWorkspace.shared.icon(for: UTType(fileType(item)) ?? .data)
    }

    fileprivate func droppable(_ id: String) -> Bool {
        drags[id]?.droppable ?? false
    }

    fileprivate func sessionEnded(_ id: String, dropped: Bool) {
        panel?.orderOut(nil)
        panel = nil
        update(id) { $0.sessionEnded(dropped: dropped, now: .now) }
    }

    fileprivate func promise(_ info: PromiseInfo, url: URL, handler: PromiseHandler) {
        guard drags[info.dragID] != nil else {
            handler.call(CocoaError(.userCancelled))
            return
        }
        handlers[info.dragID, default: [:]][info.index] = handler
        update(info.dragID) { $0.promise(index: info.index, url: url, now: .now) }
    }

    // MARK: - Carrying out

    private func update(_ id: String, _ change: (inout PromisedDrag) -> [PromisedDrag.Step]) {
        guard var drag = drags[id] else { return }
        let steps = change(&drag)
        drags[id] = drag
        for step in steps { carry(step, of: id) }
        if drags[id]?.finished == true { forget(id) }
    }

    private func carry(_ step: PromisedDrag.Step, of id: String) {
        switch step {
        case .accept(let destination):
            Task {
                do {
                    try await model.acceptDrag(id, destination: destination)
                    update(id) { $0.accepted(true, now: .now) }
                } catch {
                    let code = (error as? AgentRefusal)?.code ?? ""
                    hud.note(DragText.acceptRefused(code))
                    update(id) { $0.accepted(false, now: .now) }
                }
            }
        case .decline:
            Task { try? await model.declineDrag(id) }
        case .fulfill(let index, let path):
            if let url = drags[id]?.promised[index], let move = PromisedDrag.move(arrived: path, promised: url),
               !FileManager.default.fileExists(atPath: move.to.path) {
                // Where the system asked for it, under the name it chose.
                try? FileManager.default.moveItem(at: move.from, to: move.to)
            }
            handlers[id]?.removeValue(forKey: index)?.call(nil)
        case .fail(let index):
            let withdrawn = drags[id]?.withdrawn ?? true
            handlers[id]?.removeValue(forKey: index)?.call(
                withdrawn ? CocoaError(.userCancelled) : CocoaError(.fileWriteUnknown))
        }
    }

    private func forget(_ id: String) {
        drags[id] = nil
        // Asked for again after it was settled: nothing more comes.
        for handler in (handlers.removeValue(forKey: id) ?? [:]).values {
            handler.call(CocoaError(.userCancelled))
        }
        sources[id] = nil
    }

    /// Looks every second whether something waited too long, while there
    /// are drags.
    private func startTicking() {
        guard ticker == nil else { return }
        ticker = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(1))
                guard let self else { return }
                let now = Date.now
                for id in self.drags.keys {
                    self.update(id) { $0.tick(now: now) }
                }
                if self.drags.isEmpty {
                    self.ticker = nil
                    return
                }
            }
        }
    }
}

/// Which item a promise is for.
private struct PromiseInfo: Sendable {
    let dragID: String
    let index: Int
    let name: String
}

/// A completion handler the system passed, to call once from the main
/// thread.
private final class PromiseHandler: @unchecked Sendable {
    private let done: (Error?) -> Void

    init(_ done: @escaping (Error?) -> Void) {
        self.done = done
    }

    func call(_ error: Error?) {
        done(error)
    }
}

/// Where the system drag starts, and what the system asks about it.
private final class PromiseSourceView: NSView, NSDraggingSource, NSFilePromiseProviderDelegate {
    private let dragID: String
    private weak var owner: DragPromises?

    init(frame: CGRect, dragID: String, owner: DragPromises) {
        self.dragID = dragID
        self.owner = owner
        super.init(frame: frame)
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError() }

    func draggingSession(_: NSDraggingSession, sourceOperationMaskFor context: NSDraggingContext) -> NSDragOperation {
        guard context == .outsideApplication, owner?.droppable(dragID) == true else { return [] }
        return .copy
    }

    func draggingSession(_: NSDraggingSession, endedAt _: NSPoint, operation: NSDragOperation) {
        owner?.sessionEnded(dragID, dropped: !operation.isEmpty)
    }

    func filePromiseProvider(_ provider: NSFilePromiseProvider, fileNameForType _: String) -> String {
        (provider.userInfo as? PromiseInfo)?.name ?? "InputWeave"
    }

    func operationQueue(for _: NSFilePromiseProvider) -> OperationQueue {
        owner?.writes ?? .main
    }

    nonisolated func filePromiseProvider(
        _ provider: NSFilePromiseProvider, writePromiseTo url: URL, completionHandler: @escaping (Error?) -> Void
    ) {
        let handler = PromiseHandler(completionHandler)
        guard let info = provider.userInfo as? PromiseInfo else {
            handler.call(CocoaError(.fileWriteUnknown))
            return
        }
        Task { @MainActor [weak self] in
            guard let owner = self?.owner else {
                handler.call(CocoaError(.userCancelled))
                return
            }
            owner.promise(info, url: url, handler: handler)
        }
    }
}
