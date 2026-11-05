import AppKit

/// Shows where the cursor is after the user shook the mouse with the cursor
/// on this computer: the cursor grows to
/// several times its size and shrinks back, its tip staying on the cursor
/// as it moves. What grows is a copy of the arrow above everything else,
/// following the cursor each frame; the system's cursor is left as it is,
/// so nothing stays changed if the app goes away. It takes neither focus
/// nor clicks, makes no sound, and is gone in about a second. One at a
/// time; a new one starts over.
@MainActor
final class CursorLocator {
    /// How often the copy catches up with the cursor.
    private static let interval: TimeInterval = 1.0 / 120

    private let model: AgentModel
    /// What grows: the arrow, not whatever another app shows, as reading
    /// that is screen capture, which this app does not ask for.
    private let cursor = NSCursor.arrow
    private var panel: NSPanel?
    private var timer: Timer?
    private var motion = LocatorGeometry.motion(reduceMotion: false)
    private var start = Date()
    private var seen: UInt64?

    init(model: AgentModel) {
        self.model = model
    }

    /// Shows the cursor if the service has said where it is since last
    /// time. Where it is now is read here each frame, so the copy stays
    /// on it even as it moves on.
    func update() {
        guard let sighting = model.cursorSighting, sighting.serial != seen else { return }
        seen = sighting.serial
        show()
    }

    private func show() {
        stop()
        let view = NSImageView(image: cursor.image)
        view.imageScaling = .scaleAxesIndependently
        view.autoresizingMask = [.width, .height]
        let panel = NSPanel(
            contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered, defer: false)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.hasShadow = false
        // Above open menus too, where the cursor often is.
        panel.level = .popUpMenu
        panel.ignoresMouseEvents = true
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient, .ignoresCycle]
        panel.contentView = view
        self.panel = panel
        motion = LocatorGeometry.motion(
            reduceMotion: NSWorkspace.shared.accessibilityDisplayShouldReduceMotion)
        start = Date()
        place()
        panel.orderFrontRegardless()
        let timer = Timer(timeInterval: Self.interval, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.place() }
        }
        RunLoop.main.add(timer, forMode: .common)
        self.timer = timer
    }

    /// Puts the copy where the cursor is, at its size for now, or takes it
    /// away once it is done.
    private func place() {
        let seconds = Date().timeIntervalSince(start)
        guard let panel, seconds < motion.shown else {
            stop()
            return
        }
        let frame = LocatorGeometry.frame(
            cursor: NSEvent.mouseLocation, size: cursor.image.size, hotSpot: cursor.hotSpot,
            scale: LocatorGeometry.scale(motion, at: seconds))
        panel.setFrame(frame, display: true)
    }

    private func stop() {
        timer?.invalidate()
        timer = nil
        panel?.orderOut(nil)
        panel = nil
    }
}
