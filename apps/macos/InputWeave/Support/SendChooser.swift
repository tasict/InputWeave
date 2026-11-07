import AppKit
import Observation
import SwiftUI

/// The items an open chooser sends, which another request adds to, and
/// the computer chosen.
@MainActor @Observable
final class SendItems {
    var urls: [URL]
    /// The computer the user clicked, which comes back when it can take
    /// files again.
    var clicked: String?
    /// The computer the chooser last showed chosen, by the user or by
    /// default. The view keeps it as it draws, so it is not observed.
    @ObservationIgnored var shown: String?

    init(_ urls: [URL]) {
        self.urls = urls
    }
}

/// The small panel near the pointer that asks where files sent from
/// Finder's Services menu go (`SendService`), or says that no connected
/// computer can take them. It floats above other apps without taking the
/// focus from Finder for good: it becomes the key window so Return sends
/// and Esc cancels, and Finder stays the active app.
@MainActor
final class SendChooser {
    private var panel: NSPanel?
    /// What the open chooser sends; nil while none is open.
    private var items: SendItems?

    /// Whether the chooser is open, asking where items go.
    var isChoosing: Bool { items != nil }

    /// Asks where `urls` go; `send` gets the computer chosen and the items,
    /// with those added since.
    func choose(_ urls: [URL], model: AgentModel, send: @escaping (String, [URL]) -> Void) {
        let items = SendItems(urls)
        show(SendChooserView(model: model, items: items, cancel: { [weak self] in
            self?.close()
        }, send: { [weak self] id in
            self?.close()
            send(id, items.urls)
        }))
        self.items = items
    }

    /// Adds another request's items to the open chooser, each once, and
    /// brings it to the front.
    func add(_ urls: [URL]) {
        guard let items else { return }
        items.urls = SendTargets.merged(items.urls, urls)
        panel?.makeKeyAndOrderFront(nil)
    }

    /// Says `message`, with only OK.
    func notice(_ message: String) {
        show(SendNoticeView(message: message) { [weak self] in self?.close() })
    }

    func close() {
        panel?.orderOut(nil)
        panel = nil
        items = nil
    }

    private func show(_ view: some View) {
        close()
        let panel = KeyPanel(
            contentRect: .zero, styleMask: [.titled, .fullSizeContentView, .nonactivatingPanel],
            backing: .buffered, defer: false)
        panel.titleVisibility = .hidden
        panel.titlebarAppearsTransparent = true
        for button in [NSWindow.ButtonType.closeButton, .miniaturizeButton, .zoomButton] {
            panel.standardWindowButton(button)?.isHidden = true
        }
        panel.isMovableByWindowBackground = true
        panel.level = .floating
        panel.hidesOnDeactivate = false
        panel.isReleasedWhenClosed = false
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient]
        // At the size it asks for, which the panel follows as rows come and
        // go or the summary changes.
        let host = NSHostingView(rootView: view.fixedSize().onGeometryChange(for: CGSize.self) { $0.size } action: {
            [weak self, weak panel] size in
            guard let self, let panel, panel === self.panel else { return }
            Self.resize(panel, to: size)
        })
        panel.contentView = host
        let size = host.fittingSize
        panel.setContentSize(size)
        panel.setFrameOrigin(Self.origin(for: panel.frame.size, near: NSEvent.mouseLocation))
        panel.makeKeyAndOrderFront(nil)
        self.panel = panel
    }

    /// Sizes `panel` to its content's `size`, keeping its top left where
    /// it is.
    private static func resize(_ panel: NSPanel, to size: CGSize) {
        let frame = panel.frameRect(forContentRect: CGRect(origin: .zero, size: size))
        guard frame.size != panel.frame.size, size.width > 0, size.height > 0 else { return }
        let top = panel.frame.maxY
        panel.setFrame(CGRect(x: panel.frame.minX, y: top - frame.height, width: frame.width, height: frame.height), display: true)
    }

    /// Where a panel of `size` goes: its top left just below and right of
    /// the pointer, kept on the screen the pointer is on.
    static func origin(for size: CGSize, near pointer: CGPoint) -> CGPoint {
        let screen = NSScreen.screens.first { $0.frame.contains(pointer) } ?? NSScreen.main
        var origin = CGPoint(x: pointer.x + 8, y: pointer.y - 8 - size.height)
        if let visible = screen?.visibleFrame {
            origin.x = min(max(origin.x, visible.minX + 8), visible.maxX - size.width - 8)
            origin.y = min(max(origin.y, visible.minY + 8), visible.maxY - size.height - 8)
        }
        return origin
    }
}

/// A panel that takes the keyboard without making the app active.
private final class KeyPanel: NSPanel {
    override var canBecomeKey: Bool { true }
}

/// "Send to which computer?": every other computer, one radio button each,
/// those that can't take files listed but off, with why. It follows the
/// service while it is open, and the items as another request adds to
/// them.
struct SendChooserView: View {
    let model: AgentModel
    let items: SendItems
    /// The app's icon; snapshots pass their own.
    var icon: NSImage?
    let cancel: () -> Void
    let send: (String) -> Void

    var body: some View {
        let targets = SendTargets.targets(model.status.devices, profileFilesOff: ProfileText.filesOff(model.status))
        let chosen = SendTargets.chosen(clicked: items.clicked, shown: items.shown, in: targets)
        // Kept as it is shown: a computer that connects ahead of it does not
        // take its place.
        let _ = items.shown = chosen
        VStack(alignment: .leading, spacing: 12) {
            SendPanelHeading(icon: icon, title: String(localized: "Send to which computer?"), detail: SendTargets.summary(items.urls))
            // Only a computer that can take files can be chosen, however
            // the radio button was pressed.
            Picker(selection: Binding(
                get: { chosen ?? "" },
                set: { id in
                    if targets.contains(where: { $0.id == id && $0.available }) { items.clicked = id }
                }
            )) {
                ForEach(targets) { target in
                    SendTargetLabel(target: target)
                        .tag(target.id)
                        .disabled(!target.available)
                }
            } label: {
                Text("Send to which computer?")
            }
            .pickerStyle(.radioGroup)
            .labelsHidden()
            HStack {
                Spacer()
                Button("Cancel", action: cancel)
                    .keyboardShortcut(.cancelAction)
                Button("Send") {
                    if let chosen { send(chosen) }
                }
                .keyboardShortcut(.defaultAction)
                .disabled(chosen == nil)
            }
        }
        .padding(16)
        .frame(width: 320)
    }
}

/// A computer in the chooser: its icon, its name and how it is connected,
/// or why files can't go to it.
private struct SendTargetLabel: View {
    let target: SendTargets.Target

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: OSLabel(os: target.os).symbol)
                .foregroundStyle(target.available ? .secondary : .tertiary)
                .frame(width: 20)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text(verbatim: target.name)
                    .foregroundStyle(target.available ? .primary : .secondary)
                Text(verbatim: target.status)
                    .font(.caption)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
    }
}

/// "No connected computer can receive files.", or why nothing can be sent
/// now, with only OK.
struct SendNoticeView: View {
    let message: String
    /// The app's icon; snapshots pass their own.
    var icon: NSImage?
    let ok: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 14) {
            SendPanelHeading(icon: icon, title: message, detail: nil)
            HStack {
                Spacer()
                Button("OK", action: ok)
                    .keyboardShortcut(.defaultAction)
            }
        }
        .padding(16)
        .frame(width: 300)
    }
}

/// The app's icon beside the panel's title and, under it, what is sent.
private struct SendPanelHeading: View {
    let icon: NSImage?
    let title: String
    let detail: String?

    var body: some View {
        HStack(spacing: 10) {
            Image(nsImage: icon ?? NSApp?.applicationIconImage ?? NSImage())
                .resizable()
                .frame(width: 32, height: 32)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 2) {
                Text(verbatim: title)
                    .font(.headline)
                    .fixedSize(horizontal: false, vertical: true)
                if let detail {
                    Text(verbatim: detail)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                        .truncationMode(.middle)
                }
            }
        }
    }
}
