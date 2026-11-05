import CoreGraphics

/// Where the invisible drop strips go: along each
/// seam on this computer's screens, inside the edge, so a drag of files
/// reaches one before the cursor can cross. Free of AppKit, so it can be
/// tested.
enum DropStripGeometry {
    /// How far a strip reaches into the screen, in points.
    static let width: CGFloat = 8

    /// Whether strips go on screen at all: the background service is
    /// `connected`, and this computer shares its keyboard and mouse, is not
    /// locked to itself and captures input. While another computer
    /// controls this one, only if it can carry a drag from here
    /// (`drags_carried`).
    static func shown(connected: Bool, status: Inputweave_App_V1_Status) -> Bool {
        connected && status.sharing && !status.locked && status.capturing
            && (status.controlledByDeviceID.isEmpty || status.dragsCarried)
    }

    /// The strips on `this` device's screens, in its desktop coordinates
    /// (points, origin at the top left of the main display, y downward):
    /// one for each run of an edge that other devices' screens touch. Runs
    /// along the same edge that meet or overlap are one strip, so a drag
    /// moving along the edge stays in it.
    static func strips(this: CanvasDevice, others: [CanvasDevice], width: CGFloat = width) -> [CGRect] {
        let eps = CanvasGeometry.epsilon
        var runs: [Run] = []
        for (index, s) in this.frames.enumerated() {
            for o in others.flatMap(\.frames) {
                let (top, bottom) = (max(s.minY, o.minY), min(s.maxY, o.maxY))
                let (left, right) = (max(s.minX, o.minX), min(s.maxX, o.maxX))
                if bottom - top > eps {
                    if abs(s.maxX - o.minX) < eps { runs.append(Run(screen: index, side: .right, from: top, to: bottom)) }
                    if abs(s.minX - o.maxX) < eps { runs.append(Run(screen: index, side: .left, from: top, to: bottom)) }
                }
                if right - left > eps {
                    if abs(s.maxY - o.minY) < eps { runs.append(Run(screen: index, side: .bottom, from: left, to: right)) }
                    if abs(s.minY - o.maxY) < eps { runs.append(Run(screen: index, side: .top, from: left, to: right)) }
                }
            }
        }
        let screens = this.frames
        return merged(runs).map { run in
            let s = screens[run.screen]
            let reach = min(width, run.side.isVertical ? s.width : s.height)
            let rect: CGRect = switch run.side {
            case .right: CGRect(x: s.maxX - reach, y: run.from, width: reach, height: run.to - run.from)
            case .left: CGRect(x: s.minX, y: run.from, width: reach, height: run.to - run.from)
            case .bottom: CGRect(x: run.from, y: s.maxY - reach, width: run.to - run.from, height: reach)
            case .top: CGRect(x: run.from, y: s.minY, width: run.to - run.from, height: reach)
            }
            // From the canvas into this device's desktop.
            return rect.offsetBy(dx: -this.offset.x, dy: -this.offset.y)
        }
    }

    /// A rectangle in the background service's desktop coordinates in
    /// AppKit screen coordinates (origin at the bottom left of the main
    /// display, y upward). `mainHeight` is the height of the main display,
    /// the first of `NSScreen.screens`.
    static func screenFrame(_ desktop: CGRect, mainHeight: CGFloat) -> CGRect {
        CGRect(x: desktop.minX, y: mainHeight - desktop.maxY, width: desktop.width, height: desktop.height)
    }

    private enum Side: Int, Comparable {
        case left, right, top, bottom

        var isVertical: Bool { self == .left || self == .right }

        static func < (a: Side, b: Side) -> Bool { a.rawValue < b.rawValue }
    }

    /// A stretch of one side of one of this device's screens.
    private struct Run {
        let screen: Int
        let side: Side
        var from: CGFloat
        var to: CGFloat
    }

    private static func merged(_ runs: [Run]) -> [Run] {
        let sorted = runs.sorted { ($0.screen, $0.side, $0.from) < ($1.screen, $1.side, $1.from) }
        var result: [Run] = []
        for run in sorted {
            if var last = result.last, last.screen == run.screen, last.side == run.side,
               run.from <= last.to + CanvasGeometry.epsilon {
                last.to = max(last.to, run.to)
                result[result.count - 1] = last
            } else {
                result.append(run)
            }
        }
        return result
    }
}
