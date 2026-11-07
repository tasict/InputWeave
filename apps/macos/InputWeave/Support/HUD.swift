import AppKit
import SwiftUI

/// Brief, non-activating notes at the top of the screen the cursor is on:
/// why the keyboard and mouse came back,
/// the cursor being locked or unlocked, relative mode starting and ending,
/// the take-back hint after the first crossings, files being dragged
/// to another computer, another computer being woken and waking up, and
/// computers taken offline for now or brought back.
/// One at a time; a new note replaces the shown one.
@MainActor
final class HUDController {
    private let model: AgentModel
    private let defaults: UserDefaults
    private var panel: NSPanel?
    private var hideTask: Task<Void, Never>?
    private var seenReturns: UInt64?
    private var wasControlling = false
    private var hadSecureInput = false
    private var wasLocked: Bool?
    private var relative = RelativeText.News()
    private var wake = WakeText.Watch()
    private var offline = OfflineText.Watch()
    private var seenRefusal: UInt64?
    /// The device files from here were last seen dragged over.
    private var draggedOver = ""

    private static let shownFor: Duration = .milliseconds(2500)
    private static let hintTimes = 3

    /// `defaults` keep how many take-back hints were shown.
    init(model: AgentModel, defaults: UserDefaults = .standard) {
        self.model = model
        self.defaults = defaults
    }

    /// Compares the status with what was last seen and shows a note if
    /// something worth telling happened.
    func update(at now: Date = Date()) {
        if let message = message(for: model, at: now) {
            show(message)
        }
    }

    /// What to tell about `model` as it is now, if anything, compared with
    /// what was last seen; apart from showing it, for the tests.
    func message(for model: AgentModel, at now: Date) -> String? {
        let status = model.status
        guard model.link == .connected else {
            // A restarted service starts over; what it says first is no news.
            _ = relative.update(nil, at: now, preempted: true)
            _ = wake.update(nil)
            _ = offline.update(nil)
            return nil
        }
        let dragging = status.hasDragging ? status.dragging : Inputweave_App_V1_Dragging()
        let refusal = model.relativeRefusal?.serial ?? 0
        defer {
            seenReturns = status.controlReturns
            wasControlling = model.controllingOther
            hadSecureInput = status.secureInput
            wasLocked = status.locked
            draggedOver = dragging.deviceID
            seenRefusal = refusal
        }
        let back: String? = if let seen = seenReturns, status.controlReturns > seen {
            returnMessage(status, model: model)
        } else {
            nil
        }
        let refused: String? = if let seenRefusal, refusal > seenRefusal, let device = model.relativeRefusal?.deviceID {
            RelativeText.unsupported(model.name(of: device))
        } else {
            nil
        }
        // Only what is shown counts as told.
        let relativeNote = relative.update(
            RelativeText.State(status), at: now, preempted: back != nil || refused != nil)
        // Followed with every status, told or not: pressing Wake tells of it
        // here too, from the status the service answers with.
        let wakeNews = wake.update(status)
        // Likewise; the keyboard and mouse coming back from a computer taken
        // offline for now says it instead.
        let offlineNews = offline.update(status)
        if let message = back ?? refused {
            return message
        }
        if let relativeNote {
            let message = RelativeText.message(relativeNote, model: model)
            // Relative mode as the cursor first crosses: how to take back
            // control goes with it.
            if let hint = takeBackHint(model) {
                return "\(hint)\n\(message)"
            }
            return message
        }
        // Ahead of the take-back hint, which a drag crossing would bring.
        if !dragging.deviceID.isEmpty && dragging.deviceID != draggedOver {
            return DragText.dragging(items: dragging.items, device: model.name(of: dragging.deviceID))
        }
        if let wasLocked, status.locked != wasLocked {
            return lockMessage(status, model: model)
        }
        if let offlineNews {
            return OfflineText.message(offlineNews, name: model.name(of: offlineNews))
        }
        if let wakeNews {
            return WakeText.message(wakeNews, name: model.name(of: wakeNews.device))
        }
        if let hint = takeBackHint(model) {
            return hint
        }
        if status.secureInput && !hadSecureInput && model.controllingOther {
            return String(localized: "A password field is active, so keystrokes can't reach other computers right now.")
        }
        return nil
    }

    /// How to take back control, the first few times this computer takes
    /// control of another; each counts.
    private func takeBackHint(_ model: AgentModel) -> String? {
        guard model.controllingOther && !wasControlling else { return nil }
        let shown = defaults.integer(forKey: Preferences.takeBackHints)
        guard shown < Self.hintTimes else { return nil }
        defaults.set(shown + 1, forKey: Preferences.takeBackHints)
        return model.takeBackReminder
    }

    private func lockMessage(_ status: AgentStatus, model: AgentModel) -> String {
        guard status.locked else { return String(localized: "Unlocked.") }
        // Locked while the cursor is on another computer, it stays there.
        if model.controllingOther {
            return String(localized: "Locked to “\(model.name(of: status.activeDeviceID))”. Press \(model.lockShortcut) to unlock.")
        }
        return String(localized: "Locked to this computer. Press \(model.lockShortcut) to unlock.")
    }

    private func returnMessage(_ status: AgentStatus, model: AgentModel) -> String? {
        let name = model.name(of: status.lastReturnDeviceID)
        return switch status.lastReturn {
        case .peerLost: String(localized: "The connection to “\(name)” was lost, so the keyboard and mouse are back on this computer.")
        case .remoteActivity: String(localized: "Someone is using “\(name)”, so the keyboard and mouse are back on this computer.")
        case .hotkey: String(localized: "Back on this computer.")
        case .layoutChanged: String(localized: "The arrangement changed, so the keyboard and mouse are back on this computer.")
        case .screenLocked: String(localized: "The screen of “\(name)” locked, and input from here can't reach it, so the keyboard and mouse are back on this computer.")
        case .inputUnavailable: String(localized: "“\(name)” can't take input from here right now, so the keyboard and mouse are back on this computer.")
        case .peerPaused: String(localized: "“\(name)” paused sharing, so the keyboard and mouse are back on this computer.")
        case .captureEnded: String(localized: "The desktop took the keyboard and mouse back to this computer.")
        // This computer taken offline for now while it controlled another:
        // what took it offline says so.
        case .offlineForNow: status.lastReturnDeviceID == status.thisDeviceID ? nil : OfflineText.wentOffline(name)
        default: nil
        }
    }

    /// Shows a note about something other parts of the app saw happen.
    func note(_ message: String) {
        show(message)
    }

    private func show(_ message: String) {
        hideTask?.cancel()
        panel?.orderOut(nil)
        let panel = NSPanel(
            contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel],
            backing: .buffered, defer: false)
        panel.isOpaque = false
        panel.backgroundColor = .clear
        panel.level = .statusBar
        panel.ignoresMouseEvents = false
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient]
        let view = NSHostingView(rootView: HUDView(message: message) { [weak self] hovering in
            if hovering {
                self?.hideTask?.cancel()
            } else {
                self?.scheduleHide()
            }
        })
        panel.contentView = view
        let size = view.fittingSize
        let mouse = NSEvent.mouseLocation
        let screen = NSScreen.screens.first { $0.frame.contains(mouse) } ?? NSScreen.main
        if let visible = screen?.visibleFrame {
            panel.setFrame(
                NSRect(x: visible.midX - size.width / 2, y: visible.maxY - size.height - 12, width: size.width, height: size.height),
                display: true)
        }
        panel.alphaValue = 1
        panel.orderFrontRegardless()
        NSAccessibility.post(
            element: NSApp as Any, notification: .announcementRequested,
            userInfo: [.announcement: message, .priority: NSAccessibilityPriorityLevel.high.rawValue])
        self.panel = panel
        scheduleHide()
    }

    private func scheduleHide() {
        hideTask?.cancel()
        hideTask = Task { [weak self] in
            try? await Task.sleep(for: Self.shownFor)
            guard !Task.isCancelled, let panel = self?.panel else { return }
            await NSAnimationContext.runAnimationGroup { context in
                context.duration = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion ? 0 : 0.3
                panel.animator().alphaValue = 0
            }
            panel.orderOut(nil)
        }
    }
}

private struct HUDView: View {
    let message: String
    let hovering: (Bool) -> Void

    var body: some View {
        Text(message)
            .font(.callout)
            .multilineTextAlignment(.center)
            .frame(maxWidth: 420)
            .padding(.horizontal, 16)
            .padding(.vertical, 10)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 12))
            .fixedSize()
            .onHover(perform: hovering)
    }
}
