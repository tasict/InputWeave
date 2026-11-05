import AppKit

/// Invisible drop strips along the seams on this computer's screens, for
/// dragging files to another computer (docs/architecture.md §9).
///
/// A drag of files entering a strip is offered to the background service
/// (`BeginDrag`), which then lets the cursor cross the seam with the button
/// held. As the cursor crosses, the service lets go of the button here, on
/// the strip, which takes the drop and does nothing with it, so the drag
/// here ends where the cursor left; the other computer presses the button
/// again and offers the files there. Leaving the strip for this computer's
/// screen ends the drag (`EndDrag`).
///
/// The strips take no clicks: they are on screen only while a drag of
/// files goes on. Whether one does is read from the mouse buttons and the
/// drag pasteboard, a few times a second while strips exist, so the app
/// needs no permission to watch input (docs/architecture.md §2).
@MainActor
final class DragStrips {
    private let model: AgentModel
    private let hud: HUDController
    private var panels: [NSPanel] = []
    /// The panels' frames, in AppKit screen coordinates.
    private var frames: [CGRect] = []
    private var flow = OutgoingDrag()
    private var poll: Timer?
    /// The left button was down at the last look.
    private var wasDown = false
    /// The drag pasteboard's change count while the button was last up,
    /// and the last count looked into.
    private var restingCount = 0
    private var checkedCount = 0
    private var armed = false
    private var screens: NSObjectProtocol?

    /// How often to look at the mouse button while strips exist.
    private static let interval: TimeInterval = 0.1

    init(model: AgentModel, hud: HUDController) {
        self.model = model
        self.hud = hud
    }

    /// Puts the strips where the seams are now; called whenever the model
    /// changes.
    func update() {
        if screens == nil {
            screens = NotificationCenter.default.addObserver(
                forName: NSApplication.didChangeScreenParametersNotification, object: nil, queue: .main
            ) { [weak self] _ in
                MainActor.assumeIsolated { self?.update() }
            }
        }
        let wanted = wantedFrames()
        guard wanted != frames else { return }
        rebuild(wanted)
    }

    /// A drag ended: the one from here, or one offered here.
    func ended(_ ended: DragEnded) {
        flow.ended(ended.dragID)
        guard !ended.deviceID.isEmpty else { return }
        let device = model.name(of: ended.deviceID)
        switch ended.reason {
        case .accepted: hud.note(DragText.sending(device: device))
        case .declined: hud.note(DragText.declined(device: device))
        default: break
        }
    }

    /// Strips only where the cursor could cross with the drag: this
    /// computer shares its keyboard and mouse, and the computer on the
    /// other side is connected, files not turned off with it. While
    /// another computer controls this one, only if it can carry the drag.
    private func wantedFrames() -> [CGRect] {
        guard DropStripGeometry.shown(connected: model.link == .connected, status: model.status),
              let main = NSScreen.screens.first
        else { return [] }
        let devices = model.arrangement.devices.filter(\.placed)
        guard let this = devices.first(where: \.thisDevice) else { return [] }
        let others = devices.filter { !$0.thisDevice && model.canReceiveFiles($0.id) }
        guard !others.isEmpty else { return [] }
        return DropStripGeometry.strips(this: Self.canvas(this), others: others.map(Self.canvas))
            .map { DropStripGeometry.screenFrame($0, mainHeight: main.frame.height) }
    }

    private static func canvas(_ device: ArrangedDevice) -> CanvasDevice {
        CanvasDevice(
            id: device.id, offset: CGPoint(x: device.x, y: device.y),
            screens: device.screens.map { CGRect(x: $0.x, y: $0.y, width: $0.width, height: $0.height) })
    }

    private func rebuild(_ wanted: [CGRect]) {
        for panel in panels { panel.orderOut(nil) }
        panels = wanted.map(makePanel)
        frames = wanted
        if armed { show() }
        if wanted.isEmpty {
            poll?.invalidate()
            poll = nil
            disarm()
        } else if poll == nil {
            wasDown = false
            restingCount = NSPasteboard(name: .drag).changeCount
            checkedCount = restingCount
            let timer = Timer(timeInterval: Self.interval, repeats: true) { [weak self] _ in
                MainActor.assumeIsolated { self?.look() }
            }
            timer.tolerance = Self.interval / 2
            RunLoop.main.add(timer, forMode: .common)
            poll = timer
        }
    }

    private func makePanel(_ frame: CGRect) -> NSPanel {
        let panel = NSPanel(
            contentRect: frame, styleMask: [.borderless, .nonactivatingPanel], backing: .buffered, defer: true)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        panel.isReleasedWhenClosed = false
        panel.hidesOnDeactivate = false
        // Above the menu bar and the Dock, which may sit on a seam.
        panel.level = .popUpMenu
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .stationary, .ignoresCycle]
        // Only on screen while files are dragged, when it takes the drag.
        panel.ignoresMouseEvents = false
        panel.contentView = DropStripView(frame: CGRect(origin: .zero, size: frame.size), owner: self)
        return panel
    }

    /// Looks whether a drag of files is going on: the left button went down
    /// and the drag pasteboard changed since, holding file URLs.
    private func look() {
        let down = NSEvent.pressedMouseButtons & 1 != 0
        defer { wasDown = down }
        let pasteboard = NSPasteboard(name: .drag)
        guard down else {
            if wasDown || armed {
                disarm()
                restingCount = pasteboard.changeCount
                checkedCount = restingCount
            }
            return
        }
        guard !armed else { return }
        let count = pasteboard.changeCount
        guard count != restingCount, count != checkedCount else { return }
        checkedCount = count
        if pasteboard.types?.contains(.fileURL) == true {
            armed = true
            show()
        }
    }

    private func show() {
        for panel in panels { panel.orderFrontRegardless() }
    }

    /// The button is up: the drag is over here, whether it was dropped, let
    /// go of as the cursor crossed, or ended elsewhere; the service saw the
    /// release.
    private func disarm() {
        armed = false
        for panel in panels { panel.orderOut(nil) }
        flow.dropped()
    }

    // MARK: - From the strips

    fileprivate func entered(_ urls: [URL]) {
        guard let token = flow.entered() else { return }
        Task {
            do {
                let begun = try await model.beginDrag(urls)
                if let id = flow.answered(token: token, id: begun.dragID) { end(id) }
            } catch let refusal as AgentRefusal {
                _ = flow.answered(token: token, id: nil)
                if ["too_many_items", "too_large"].contains(refusal.code) { hud.note(DragText.limits) }
            } catch {
                _ = flow.answered(token: token, id: nil)
            }
        }
    }

    fileprivate func left() {
        if let id = flow.left() { end(id) }
    }

    fileprivate func dropped() {
        flow.dropped()
    }

    private func end(_ id: String) {
        Task { try? await model.endDrag(id) }
    }
}

/// A strip's content: takes drags of files, and drops of them, which it
/// leaves be.
private final class DropStripView: NSView {
    private weak var owner: DragStrips?
    private var dragging = false

    init(frame: CGRect, owner: DragStrips) {
        self.owner = owner
        super.init(frame: frame)
        wantsLayer = true
        // Next to transparent: a window shows nothing, yet is there to
        // take the drag.
        layer?.backgroundColor = NSColor(white: 0, alpha: 0.004).cgColor
        registerForDraggedTypes([.fileURL])
    }

    @available(*, unavailable)
    required init?(coder _: NSCoder) { fatalError() }

    override func draggingEntered(_ sender: any NSDraggingInfo) -> NSDragOperation {
        let urls = sender.draggingPasteboard.readObjects(
            forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
        dragging = !urls.isEmpty
        guard dragging else { return [] }
        owner?.entered(urls)
        return operation(sender)
    }

    override func draggingUpdated(_ sender: any NSDraggingInfo) -> NSDragOperation {
        dragging ? operation(sender) : []
    }

    override func draggingExited(_: (any NSDraggingInfo)?) {
        guard dragging else { return }
        dragging = false
        owner?.left()
    }

    override func prepareForDragOperation(_: any NSDraggingInfo) -> Bool {
        dragging
    }

    override func performDragOperation(_: any NSDraggingInfo) -> Bool {
        // Taken so the drag here ends without sliding back; the files go
        // where the drag goes on, if it does.
        dragging = false
        owner?.dropped()
        return true
    }

    override func draggingEnded(_: any NSDraggingInfo) {
        // Ended without a drop while over the strip, as when cancelled.
        guard dragging else { return }
        dragging = false
        owner?.left()
    }

    /// Copy, as the files stay where they are.
    private func operation(_ sender: any NSDraggingInfo) -> NSDragOperation {
        let allowed = sender.draggingSourceOperationMask
        for operation: NSDragOperation in [.copy, .generic, .move] where allowed.contains(operation) {
            return operation
        }
        return []
    }
}
