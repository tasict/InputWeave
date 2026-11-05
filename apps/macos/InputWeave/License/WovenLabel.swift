import SwiftUI

/// "Licensed", as a woven label sewn into a seam: the screen tile's white
/// and edge, a running stitch and the text in the thread color. Besides
/// the seam itself, it is the only place the thread appears in settings.
struct WovenLabel: View {
    enum Size {
        /// The settings sidebar's License row.
        case small
        /// About.
        case medium
        /// The License page.
        case large
    }

    var size: Size = .medium
    /// Sews itself once, after the user's own Activate succeeded.
    var sews = false
    /// Increased contrast regardless of the setting, for snapshots.
    var increasedContrast: Bool?
    /// Told once the sewing has started, so it plays only once.
    var onSewn: (() -> Void)?

    @Environment(\.colorSchemeContrast) private var contrast
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var sewn: CGFloat

    init(size: Size = .medium, sews: Bool = false, increasedContrast: Bool? = nil, onSewn: (() -> Void)? = nil) {
        self.size = size
        self.sews = sews
        self.increasedContrast = increasedContrast
        self.onSewn = onSewn
        _sewn = State(initialValue: sews ? 0 : 1)
    }

    var body: some View {
        let metrics = Metrics(size)
        let high = increasedContrast ?? (contrast == .increased)
        Text("Licensed")
            .font(.system(size: metrics.font, weight: high ? .bold : .semibold))
            .foregroundStyle(Color.thread)
            .lineLimit(1)
            .fixedSize()
            .padding(.horizontal, metrics.padding)
            .frame(height: metrics.height)
            .background(RoundedRectangle(cornerRadius: metrics.corner).fill(Color.tile))
            .overlay(
                RoundedRectangle(cornerRadius: metrics.corner)
                    .strokeBorder(high ? Color.secondary : Color.tileEdge, lineWidth: 1))
            .overlay(
                RoundedRectangle(cornerRadius: max(metrics.corner - 2, 1))
                    .inset(by: metrics.inset)
                    .trim(from: 0, to: sewn)
                    .stroke(
                        Color.thread,
                        style: high
                            ? StrokeStyle(lineWidth: 2)
                            : StrokeStyle(lineWidth: metrics.stitch, lineCap: .round, dash: metrics.dash)))
            .accessibilityElement(children: .ignore)
            .accessibilityLabel(Text("Licensed"))
            .onAppear {
                guard sews else { return }
                if reduceMotion {
                    sewn = 1
                } else {
                    // After the unsewn label is on screen, or there is
                    // nothing to animate from.
                    Task { @MainActor in
                        withAnimation(.easeOut(duration: 0.5)) { sewn = 1 }
                    }
                }
                onSewn?()
            }
    }

    private struct Metrics {
        var font: CGFloat
        var height: CGFloat
        var padding: CGFloat
        var corner: CGFloat
        var inset: CGFloat
        var stitch: CGFloat
        var dash: [CGFloat]

        init(_ size: Size) {
            switch size {
            case .small:
                self = Metrics(font: 10.5, height: 19, padding: 8, corner: 3, inset: 2.5, stitch: 1, dash: [2.5, 2])
            case .medium:
                self = Metrics(font: 11.5, height: 22, padding: 10, corner: 4, inset: 3, stitch: 1, dash: [3, 2.2])
            case .large:
                self = Metrics(font: 17, height: 44, padding: 20, corner: 5, inset: 4, stitch: 1.4, dash: [4, 3])
            }
        }

        init(font: CGFloat, height: CGFloat, padding: CGFloat, corner: CGFloat, inset: CGFloat, stitch: CGFloat, dash: [CGFloat]) {
            self.font = font
            self.height = height
            self.padding = padding
            self.corner = corner
            self.inset = inset
            self.stitch = stitch
            self.dash = dash
        }
    }
}

/// "Trial", in a neutral capsule: what stands where the woven label goes
/// until the computer is licensed.
struct TrialCapsule: View {
    /// Increased on a selected row of a focused list, whose fill is the
    /// accent color.
    @Environment(\.backgroundProminence) private var prominence

    var body: some View {
        let onSelection = prominence == .increased
        Text("Trial")
            .font(.caption)
            .lineLimit(1)
            .fixedSize()
            .padding(.horizontal, 7)
            .padding(.vertical, 1)
            .foregroundStyle(onSelection ? Color.white : Color.secondary)
            .background(Capsule().fill(onSelection ? Color.white.opacity(0.22) : Color.secondary.opacity(0.15)))
    }
}
