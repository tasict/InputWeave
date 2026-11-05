import CoreGraphics
import Foundation

/// Where, and for how long, the edge lights up as the cursor crosses to or
/// from another computer: the whole edge it
/// crossed in the seam color, brightest where it crossed, fading inward.
/// Free of AppKit, so it is tested on its own.
enum EdgeGlowGeometry {
    enum Edge: Equatable {
        case left, right, top, bottom
    }

    /// Fading inward over about 24 points.
    static let depth: CGFloat = 24
    /// With Increase Contrast, a solid line this thick instead, no fade.
    static let solidDepth: CGFloat = 4
    /// How bright the glow stays along the edge away from where it
    /// crossed, at the least, of its brightest.
    static let dimmest = 0.35
    /// How wide the bright part is, as a share of the edge's length (the
    /// standard deviation of its bell curve).
    static let spread = 0.25

    struct Timing: Equatable {
        let fadeIn: Double
        let hold: Double
        let fadeOut: Double
    }

    /// In 0.1 seconds, held for 0.3, out in 0.4.
    static let animated = Timing(fadeIn: 0.1, hold: 0.3, fadeOut: 0.4)
    /// With Reduce Motion: there at once for half a second, then gone.
    static let still = Timing(fadeIn: 0, hold: 0.5, fadeOut: 0)

    static func timing(reduceMotion: Bool) -> Timing {
        reduceMotion ? still : animated
    }

    static func edge(_ edge: Inputweave_App_V1_ScreenEdge) -> Edge? {
        switch edge {
        case .left: .left
        case .right: .right
        case .top: .top
        case .bottom: .bottom
        default: nil
        }
    }

    /// How far along its edge the crossing lies, from 0 at the edge's top
    /// (a left or right edge) or left end (a top or bottom one) to 1 at the
    /// other end: the background service's coordinates have y going down.
    static func fraction(of crossed: Inputweave_App_V1_CursorCrossed, edge: Edge) -> CGFloat {
        let (start, extent) = switch edge {
        case .left, .right: (crossed.displayY, crossed.displayHeight)
        case .top, .bottom: (crossed.displayX, crossed.displayWidth)
        }
        guard extent > 0, crossed.along.isFinite else { return 0.5 }
        return CGFloat(min(max((crossed.along - start) / extent, 0), 1))
    }

    /// The glow's frame on a screen whose AppKit frame is `screen` (y
    /// going up): the whole edge, `thickness` deep from it.
    static func strip(on screen: CGRect, edge: Edge, thickness: CGFloat) -> CGRect {
        switch edge {
        case .left: CGRect(x: screen.minX, y: screen.minY, width: thickness, height: screen.height)
        case .right: CGRect(x: screen.maxX - thickness, y: screen.minY, width: thickness, height: screen.height)
        case .top: CGRect(x: screen.minX, y: screen.maxY - thickness, width: screen.width, height: thickness)
        case .bottom: CGRect(x: screen.minX, y: screen.minY, width: screen.width, height: thickness)
        }
    }

    /// How bright the glow is at `position` along its edge (0 to 1, as
    /// `fraction(of:edge:)` measures) when the cursor crossed at `spot`:
    /// all of it there, falling off smoothly toward both ends but never
    /// below `dimmest`, so the whole edge shows; about two fifths at the
    /// ends when it crossed in the middle.
    static func brightness(at position: Double, spot: Double) -> Double {
        let away = (position - spot) / spread
        return dimmest + (1 - dimmest) * exp(-away * away / 2)
    }

    /// The brightness along the edge as `count` evenly spaced stops, from
    /// one end to the other, for a gradient.
    static func stops(spot: Double, count: Int = 33) -> [(location: Double, brightness: Double)] {
        (0..<count).map { i in
            let location = Double(i) / Double(count - 1)
            return (location, brightness(at: location, spot: spot))
        }
    }

    /// How opaque the glow is `elapsed` seconds after it lit; nil once it
    /// is gone.
    static func opacity(_ timing: Timing, elapsed: Double) -> Double? {
        if elapsed < 0 { return 0 }
        if elapsed < timing.fadeIn { return elapsed / timing.fadeIn }
        let fading = elapsed - timing.fadeIn - timing.hold
        if fading < 0 { return 1 }
        return fading < timing.fadeOut ? 1 - fading / timing.fadeOut : nil
    }

    /// When a crossing of an edge at `now` lights its glow from, the glow
    /// of that same edge having lit at `previous`, if it did. While that is
    /// still to be seen (coming up, held or fading), it carries on from how
    /// bright it is: up to full again if it was fading, held anew, never
    /// dark in between, so crossing back and forth does not flicker.
    /// Otherwise it lights up anew, from now.
    static func litAt(previous: Date?, now: Date, timing: Timing) -> Date {
        guard let previous, let opacity = opacity(timing, elapsed: now.timeIntervalSince(previous)) else { return now }
        return now.addingTimeInterval(-timing.fadeIn * opacity)
    }

    /// Which of `screens` (their AppKit frames, and their numbers,
    /// `NSScreenNumber`) the display crossed is: the one whose number is
    /// the display's id, as the background service names Mac displays;
    /// else the one at the display's place and size, its bounds being in
    /// the service's desktop coordinates (`ScreenIdentity.desktopFrame`);
    /// else the only one of its size; else none.
    static func screen(
        for crossed: Inputweave_App_V1_CursorCrossed, among screens: [(number: UInt32?, frame: CGRect)],
        mainHeight: CGFloat
    ) -> Int? {
        if let number = UInt32(crossed.displayID),
           let index = screens.firstIndex(where: { $0.number == number }) {
            return index
        }
        let close = { (a: Double, b: CGFloat) in abs(CGFloat(a) - b) < 1 }
        let sameSize = { (frame: CGRect) in
            close(crossed.displayWidth, frame.width) && close(crossed.displayHeight, frame.height)
        }
        if let index = screens.firstIndex(where: {
            let desktop = ScreenIdentity.desktopFrame(of: $0.frame, mainHeight: mainHeight)
            return sameSize(desktop) && close(crossed.displayX, desktop.minX) && close(crossed.displayY, desktop.minY)
        }) {
            return index
        }
        let ofSize = screens.indices.filter { sameSize(screens[$0].frame) }
        return ofSize.count == 1 ? ofSize[0] : nil
    }

    /// The glows lit, one for each edge of each display, each fading on
    /// its own: a crossing of another edge of the same display does not cut
    /// one short.
    struct Glows {
        struct Key: Hashable {
            let display: String
            let edge: Edge
        }

        /// When each glow lit, which its opacity counts from, and its
        /// timing. (Where along its edge it is brightest is what its panel
        /// shows, `EdgeGlowLook`.)
        private(set) var lit: [Key: (at: Date, timing: Timing)] = [:]

        /// Lights the glow of `edge` on `display` at `now`, carrying on from
        /// how bright it is while it still shows (`litAt`); returns its key.
        @discardableResult
        mutating func light(display: String, edge: Edge, now: Date, timing: Timing) -> Key {
            let key = Key(display: display, edge: edge)
            lit[key] = (EdgeGlowGeometry.litAt(previous: lit[key]?.at, now: now, timing: timing), timing)
            return key
        }

        /// How bright each glow is at `now`; those gone are forgotten and
        /// listed with nil, once.
        mutating func fade(at now: Date) -> [Key: Double?] {
            var opacities: [Key: Double?] = [:]
            for (key, glow) in lit {
                let opacity = EdgeGlowGeometry.opacity(glow.timing, elapsed: now.timeIntervalSince(glow.at))
                opacities[key] = opacity
                if opacity == nil { lit[key] = nil }
            }
            return opacities
        }
    }
}
