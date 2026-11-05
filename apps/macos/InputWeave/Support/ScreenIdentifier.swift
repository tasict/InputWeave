import AppKit
import SwiftUI

/// Shows this computer's name on each of its screens for a moment when the
/// screens are identified, here or from another computer: large in the middle, with the screen's number and size
/// beneath, inside a border in the seam color along the screen's edges.
/// The panels take neither focus nor clicks, and go away by themselves;
/// identified again, the new ones replace them.
@MainActor
final class ScreenIdentifier {
    private let model: AgentModel
    private var panels: [NSPanel] = []
    private var hideTask: Task<Void, Never>?
    private var seen: UInt64?

    init(model: AgentModel) {
        self.model = model
    }

    /// Shows the names if the service said to since last time.
    func update() {
        guard let identification = model.screenIdentification, identification.serial != seen else { return }
        seen = identification.serial
        show(for: ScreenIdentity.shownFor(durationMs: identification.durationMs))
    }

    private func show(for seconds: Double) {
        hideTask?.cancel()
        for panel in panels { panel.orderOut(nil) }
        let screens = NSScreen.screens
        guard let main = screens.first else { return }
        let name = model.status.thisDeviceName
        let thisDevice = model.arrangement.devices.first { $0.thisDevice }
        let arranged = thisDevice?.screens ?? []
        let jump = thisDevice.flatMap { $0.jumpSlot > 0 ? model.jumpShortcut(slot: $0.jumpSlot) : nil }
        panels = screens.enumerated().map { index, screen in
            let label = ScreenIdentity.label(
                frame: screen.frame, mainHeight: main.frame.height, backingScale: screen.backingScaleFactor,
                screens: arranged, fallback: index)
            let panel = NSPanel(
                contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel],
                backing: .buffered, defer: false)
            panel.isOpaque = false
            panel.backgroundColor = .clear
            panel.hasShadow = false
            // Over the menu bar and open menus, as the cursor locator is.
            panel.level = .popUpMenu
            panel.ignoresMouseEvents = true
            panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient, .ignoresCycle]
            panel.contentView = NSHostingView(rootView: ScreenIdentityView(name: name, label: label, jump: jump))
            panel.setFrame(screen.frame, display: true)
            panel.orderFrontRegardless()
            return panel
        }
        NSAccessibility.post(
            element: NSApp as Any, notification: .announcementRequested,
            userInfo: [.announcement: name, .priority: NSAccessibilityPriorityLevel.medium.rawValue])
        let shown = panels
        hideTask = Task { [weak self] in
            try? await Task.sleep(for: .seconds(seconds))
            guard !Task.isCancelled else { return }
            // With Reduce Motion they go at once.
            if !NSWorkspace.shared.accessibilityDisplayShouldReduceMotion {
                await NSAnimationContext.runAnimationGroup { context in
                    context.duration = 0.3
                    for panel in shown { panel.animator().alphaValue = 0 }
                }
            }
            guard !Task.isCancelled else { return }
            for panel in shown { panel.orderOut(nil) }
            self?.panels.removeAll { shown.contains($0) }
        }
    }
}

/// What one screen shows: this computer's name, the screen's number and
/// size, the shortcut that jumps here when there is one, and a border in
/// the seam color along its edges.
struct ScreenIdentityView: View {
    let name: String
    let label: ScreenIdentity.Label
    var jump: String? = nil

    var body: some View {
        // Pixels, as the inspector shows them: without grouping.
        let size = "\(label.width) × \(label.height)"
        ZStack {
            Rectangle()
                .strokeBorder(Color.thread, lineWidth: 6)
            VStack(spacing: 6) {
                Text(name)
                    .font(.system(size: 56, weight: .semibold))
                    .lineLimit(1)
                    .minimumScaleFactor(0.4)
                Text("Screen \(label.number) · \(size)")
                    .font(.title3)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                if let jump {
                    Text("Jump here with \(jump)")
                        .font(.title3)
                        .foregroundStyle(.secondary)
                }
            }
            .padding(.horizontal, 40)
            .padding(.vertical, 24)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 20))
            .overlay {
                RoundedRectangle(cornerRadius: 20).strokeBorder(Color.thread, lineWidth: 3)
            }
            .padding(40)
        }
        .accessibilityElement(children: .combine)
    }
}

/// "Identify Screens", on the arrangement page and in the panel's menu.
struct IdentifyScreensButton: View {
    @Environment(AgentModel.self) private var model

    var body: some View {
        Button("Identify Screens") { Task { try? await model.identifyScreens() } }
            .disabled(model.link != .connected)
            .help(Text("Shows each computer's name on its screens for a moment."))
    }
}
