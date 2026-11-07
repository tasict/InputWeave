import AppKit
import SwiftUI

/// The code shown on the computer being paired:
/// a floating panel in the middle of the screen that needs no input; its
/// only button declines. The background service decides when it shows.
@MainActor
final class PairingCodePanel {
    private var panel: NSPanel?
    private let model: AgentModel

    init(model: AgentModel) {
        self.model = model
    }

    /// Shows or hides the panel to match the pairing state.
    func update() {
        if StatusPresentation.showsPairingCode(model.pairing) {
            show()
        } else {
            panel?.orderOut(nil)
            panel = nil
        }
    }

    private func show() {
        guard panel == nil else { return }
        let panel = NSPanel(
            contentRect: NSRect(x: 0, y: 0, width: 420, height: 260),
            styleMask: [.titled, .fullSizeContentView, .nonactivatingPanel],
            backing: .buffered, defer: false)
        panel.titlebarAppearsTransparent = true
        panel.titleVisibility = .hidden
        panel.level = .floating
        panel.isMovableByWindowBackground = true
        panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary]
        panel.contentView = NSHostingView(rootView: PairingCodeView().environment(model))
        panel.center()
        panel.orderFrontRegardless()
        self.panel = panel
    }
}

#if DEBUG
/// The panel's content, for previews and snapshots.
struct PairingCodePanelPreview: View {
    var body: some View { PairingCodeView() }
}
#endif

private struct PairingCodeView: View {
    @Environment(AgentModel.self) private var model

    var body: some View {
        VStack(spacing: 14) {
            Text("Enter this code on “\(model.pairing.peerName)”")
                .font(.title3)
                .multilineTextAlignment(.center)
            Text(verbatim: groupedCode(model.pairing.code))
                .font(.system(size: 48, weight: .semibold).monospacedDigit())
                .kerning(2)
                .textSelection(.enabled)
                .accessibilityLabel(Text(model.pairing.code.map(String.init).joined(separator: " ")))
            TimelineView(.periodic(from: .now, by: 1)) { context in
                Text("Valid for \(countdown(until: model.pairing.expiresAtUnixMs, now: context.date))")
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
            }
            Button("Decline") { Task { try? await model.cancelPairing() } }
                .keyboardShortcut(.cancelAction)
        }
        .padding(28)
        .frame(width: 420)
    }
}
