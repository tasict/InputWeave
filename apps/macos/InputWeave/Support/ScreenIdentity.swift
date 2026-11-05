import CoreGraphics

/// Which of this computer's screens, as the arrangement lists them, a
/// screen AppKit reports is, for the label shown on it when the screens are
/// identified. Free of AppKit, so it can be
/// tested.
enum ScreenIdentity {
    struct Label: Equatable {
        /// Its place among this computer's screens in the arrangement, from 1.
        let number: Int
        /// Its size in pixels.
        let width: Int
        let height: Int
    }

    /// The label for the screen whose AppKit `frame` (origin at the bottom
    /// left of the main display, y upward) is given; `mainHeight` is the
    /// main display's height. It is the screen of `screens` (this
    /// computer's in the arrangement, in the background service's desktop
    /// coordinates: origin at the top left of the main display, y downward)
    /// at the same place and size. When none is, as before the service has
    /// reported the displays, it is the `fallback`-th screen, counted from
    /// 0, at the frame's size times `backingScale`.
    static func label(
        frame: CGRect, mainHeight: CGFloat, backingScale: CGFloat, screens: [Inputweave_App_V1_Screen], fallback: Int
    ) -> Label {
        let desktop = desktopFrame(of: frame, mainHeight: mainHeight)
        let close = { (a: Double, b: CGFloat) in abs(CGFloat(a) - b) < 1 }
        if let index = screens.firstIndex(where: {
            close($0.x, desktop.minX) && close($0.y, desktop.minY)
                && close($0.width, desktop.width) && close($0.height, desktop.height)
        }) {
            let screen = screens[index]
            let scale = screen.scale > 0 ? screen.scale : Double(backingScale)
            return Label(
                number: index + 1,
                width: Int((screen.width * scale).rounded()), height: Int((screen.height * scale).rounded()))
        }
        return Label(
            number: fallback + 1,
            width: Int((frame.width * backingScale).rounded()), height: Int((frame.height * backingScale).rounded()))
    }

    /// An AppKit screen `frame` (origin at the bottom left of the main
    /// display, y upward) in the background service's desktop coordinates
    /// (origin at the top left of the main display, y downward);
    /// `mainHeight` is the main display's height.
    static func desktopFrame(of frame: CGRect, mainHeight: CGFloat) -> CGRect {
        CGRect(x: frame.minX, y: mainHeight - frame.maxY, width: frame.width, height: frame.height)
    }

    /// How long the names stay, in seconds, for what the service asked.
    static func shownFor(durationMs: UInt32) -> Double {
        Double(durationMs == 0 ? defaultDurationMs : durationMs) / 1000
    }

    /// What the service asks for, should it not say.
    static let defaultDurationMs: UInt32 = 2500
}
