import SwiftUI

/// Where the shared keyboard and mouse are: a keyboard and a mouse on the
/// main screen of that computer, saying so in words on the arrangement
/// page when the screen has room. On material with a hairline edge, so it
/// reads on any desktop picture.
struct KeyboardMouseBadge: View {
    /// The mini map's smaller badge, symbols only.
    var compact = false
    @Environment(\.displayScale) private var displayScale

    var body: some View {
        if compact {
            capsule(words: false)
        } else {
            // The words when they fit on the screen, else the symbols alone.
            ViewThatFits(in: .horizontal) {
                capsule(words: true)
                capsule(words: false)
            }
        }
    }

    private func capsule(words: Bool) -> some View {
        HStack(spacing: compact ? 2 : 4) {
            Image(systemName: "keyboard")
            Image(systemName: "computermouse")
            if words {
                Text("Keyboard and mouse here").lineLimit(1)
            }
        }
        .font(compact ? .system(size: 9, weight: .semibold) : .caption.weight(.semibold))
        .foregroundStyle(.primary)
        .padding(.horizontal, compact ? 4 : 7)
        .frame(height: compact ? 14 : 20)
        .background(.regularMaterial, in: Capsule())
        .overlay { Capsule().strokeBorder(Color.tileEdge, lineWidth: 1 / max(displayScale, 1)) }
        .fixedSize()
    }
}

/// Pulses the badge once whenever the background service says where the
/// cursor is, after the user shook the mouse, so
/// an open panel or window points at it too. Not with Reduce Motion.
struct LocatePulse: ViewModifier {
    let trigger: UInt64?
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        content.keyframeAnimator(initialValue: CGFloat(1), trigger: trigger) { view, scale in
            view.scaleEffect(scale)
        } keyframes: { _ in
            CubicKeyframe(reduceMotion ? 1 : 1.15, duration: 0.2)
            CubicKeyframe(1, duration: 0.2)
        }
    }
}

/// Places the badge on a screen tile, both in canvas view coordinates,
/// clear of the computer's name (`CanvasGeometry.badgeFrame`). It fills
/// the canvas, so it goes in the canvas's stack above everything else.
struct BadgePlacement: Layout {
    let tile: CGRect
    /// Where the name on that tile ends, if it has one.
    let nameBottom: CGFloat?
    let inset: CGFloat

    func sizeThatFits(proposal: ProposedViewSize, subviews _: Subviews, cache _: inout ()) -> CGSize {
        proposal.replacingUnspecifiedDimensions()
    }

    func placeSubviews(in bounds: CGRect, proposal _: ProposedViewSize, subviews: Subviews, cache _: inout ()) {
        for badge in subviews {
            let room = ProposedViewSize(width: max(tile.width - 2 * inset, 0), height: nil)
            let size = badge.sizeThatFits(room)
            let frame = CanvasGeometry.badgeFrame(size, on: tile, nameBottom: nameBottom, inset: inset)
            badge.place(
                at: CGPoint(x: bounds.minX + frame.minX, y: bounds.minY + frame.minY),
                anchor: .topLeading, proposal: room)
        }
    }
}
