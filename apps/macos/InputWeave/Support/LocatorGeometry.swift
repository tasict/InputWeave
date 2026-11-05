import CoreGraphics
import Foundation

/// The parts of the cursor locator that need no
/// screen: how the cursor grows and shrinks, and where its enlarged copy
/// goes so that its tip stays on the cursor.
enum LocatorGeometry {
    /// How the cursor is shown.
    struct Motion: Equatable {
        /// How many times its size the cursor grows to.
        let peak: CGFloat
        /// How long it takes to grow, in seconds; 0 is at its peak at once.
        let grow: TimeInterval
        /// How long it then stays at its peak.
        let hold: TimeInterval
        /// How long it then takes to shrink back to its own size; 0 is gone
        /// at once.
        let shrink: TimeInterval

        /// How long it is on screen in all.
        var shown: TimeInterval { grow + hold + shrink }
    }

    /// The cursor grows to four times its size, stays a moment and shrinks
    /// back, in 1.1 seconds. With Reduce Motion it is three times its size
    /// for a second and is gone.
    static func motion(reduceMotion: Bool) -> Motion {
        reduceMotion
            ? Motion(peak: 3, grow: 0, hold: 1, shrink: 0)
            : Motion(peak: 4, grow: 0.15, hold: 0.6, shrink: 0.35)
    }

    /// How many times its size the cursor is `seconds` after it began to
    /// grow: easing out as it grows, in and out as it shrinks, and its own
    /// size once it is done.
    static func scale(_ motion: Motion, at seconds: TimeInterval) -> CGFloat {
        let shrinking = motion.grow + motion.hold
        if seconds < motion.grow {
            let t = max(seconds, 0) / motion.grow
            return 1 + (motion.peak - 1) * CGFloat(1 - pow(1 - t, 3))
        }
        if seconds < shrinking {
            return motion.peak
        }
        if seconds < motion.shown {
            let t = (seconds - shrinking) / motion.shrink
            let eased = t < 0.5 ? 4 * t * t * t : 1 - pow(-2 * t + 2, 3) / 2
            return motion.peak - (motion.peak - 1) * CGFloat(eased)
        }
        return 1
    }

    /// The frame, in AppKit screen coordinates, of a cursor image of `size`
    /// at `scale` times that, with its hot spot on `cursor`. `hotSpot` is
    /// measured from the image's top left, as `NSCursor` has it.
    static func frame(cursor: CGPoint, size: CGSize, hotSpot: CGPoint, scale: CGFloat) -> CGRect {
        CGRect(
            x: cursor.x - hotSpot.x * scale, y: cursor.y - (size.height - hotSpot.y) * scale,
            width: size.width * scale, height: size.height * scale)
    }
}
