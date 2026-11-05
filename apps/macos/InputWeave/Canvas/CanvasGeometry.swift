import CoreGraphics

/// A device on the arrangement canvas: its screens in its own desktop
/// coordinates, and where its desktop origin sits on the canvas. All in
/// logical units, y growing downwards, as the background service uses.
struct CanvasDevice: Identifiable, Equatable {
    let id: String
    var offset: CGPoint
    let screens: [CGRect]

    /// The screens on the canvas.
    var frames: [CGRect] {
        screens.map { $0.offsetBy(dx: offset.x, dy: offset.y) }
    }

    var bounds: CGRect {
        CanvasGeometry.union(frames) ?? .zero
    }
}

/// A shared edge between two devices' screens: where the cursor crosses.
struct Seam: Equatable {
    let start: CGPoint
    let end: CGPoint
}

/// Geometry for drawing and editing the arrangement, free of UI so it can
/// be tested.
enum CanvasGeometry {
    /// Edges closer than this count as touching.
    static let epsilon: CGFloat = 0.001

    static func union(_ rects: [CGRect]) -> CGRect? {
        rects.reduce(nil) { partial, rect in partial.map { $0.union(rect) } ?? rect }
    }

    /// Maps canvas coordinates into a view and back.
    struct Fit: Equatable {
        var scale: CGFloat
        /// The view point that canvas (0, 0) lands on.
        var origin: CGPoint

        func toView(_ p: CGPoint) -> CGPoint {
            CGPoint(x: origin.x + p.x * scale, y: origin.y + p.y * scale)
        }

        func toView(_ r: CGRect) -> CGRect {
            CGRect(origin: toView(r.origin), size: CGSize(width: r.width * scale, height: r.height * scale))
        }
    }

    /// Fits `content` centered into `size`, leaving `padding` on every side.
    static func fit(_ content: CGRect, in size: CGSize, padding: CGFloat) -> Fit {
        guard content.width > 0, content.height > 0 else {
            return Fit(scale: 1, origin: CGPoint(x: size.width / 2, y: size.height / 2))
        }
        let scale = min(
            (size.width - 2 * padding) / content.width,
            (size.height - 2 * padding) / content.height)
        let scaled = CGSize(width: content.width * scale, height: content.height * scale)
        return Fit(
            scale: max(scale, 0.001),
            origin: CGPoint(
                x: (size.width - scaled.width) / 2 - content.minX * scale,
                y: (size.height - scaled.height) / 2 - content.minY * scale))
    }

    /// Whether any screen of `a` overlaps any screen of `b`. Touching is not
    /// overlapping.
    static func overlaps(_ a: [CGRect], _ b: [CGRect]) -> Bool {
        a.contains { r in
            b.contains { s in
                r.minX < s.maxX - epsilon && s.minX < r.maxX - epsilon
                    && r.minY < s.maxY - epsilon && s.minY < r.maxY - epsilon
            }
        }
    }

    /// Moves `moving` so that its edges meet or line up with the nearest
    /// edges of the other devices, if they are within `threshold`, and
    /// returns its new offset. Each axis snaps on its own.
    static func snap(_ moving: CanvasDevice, among others: [CanvasDevice], threshold: CGFloat) -> CGPoint {
        var best = (x: threshold + 1, dx: CGFloat(0), y: threshold + 1, dy: CGFloat(0))
        func consider(_ value: CGFloat, axis: KeyPath<CGPoint, CGFloat>) {
            if axis == \CGPoint.x {
                if abs(value) <= threshold && abs(value) < best.x { best.x = abs(value); best.dx = value }
            } else {
                if abs(value) <= threshold && abs(value) < best.y { best.y = abs(value); best.dy = value }
            }
        }
        for f in moving.frames {
            for o in others.flatMap(\.frames) {
                let nearVertically = f.minY < o.maxY + threshold && o.minY < f.maxY + threshold
                let nearHorizontally = f.minX < o.maxX + threshold && o.minX < f.maxX + threshold
                if nearVertically {
                    // Side by side.
                    consider(o.maxX - f.minX, axis: \.x)
                    consider(o.minX - f.maxX, axis: \.x)
                }
                if nearHorizontally {
                    // One above the other.
                    consider(o.maxY - f.minY, axis: \.y)
                    consider(o.minY - f.maxY, axis: \.y)
                }
                let sideBySide = abs(o.maxX - f.minX) <= threshold || abs(o.minX - f.maxX) <= threshold
                let stacked = abs(o.maxY - f.minY) <= threshold || abs(o.minY - f.maxY) <= threshold
                if sideBySide {
                    // Line up top or bottom edges.
                    consider(o.minY - f.minY, axis: \.y)
                    consider(o.maxY - f.maxY, axis: \.y)
                }
                if stacked {
                    consider(o.minX - f.minX, axis: \.x)
                    consider(o.maxX - f.maxX, axis: \.x)
                }
            }
        }
        return CGPoint(x: moving.offset.x + best.dx, y: moving.offset.y + best.dy)
    }

    /// The seams between screens of different devices.
    static func seams(_ devices: [CanvasDevice]) -> [Seam] {
        var seams: [Seam] = []
        for (i, a) in devices.enumerated() {
            for b in devices[(i + 1)...] {
                for p in a.frames {
                    for q in b.frames {
                        if let seam = seam(p, q) ?? seam(q, p) {
                            seams.append(seam)
                        }
                    }
                }
            }
        }
        return seams
    }

    /// Which of a computer's screen tiles the mini map labels: the main
    /// screen, else the widest, as long as the tile leaves `room` for the
    /// name. `nil` when none does, so the name is left out rather than
    /// spilling over its tile.
    static func labelTile(_ tiles: [CGRect], main: Int, room: CGSize) -> Int? {
        let fits = { (tile: CGRect) in tile.width >= room.width && tile.height >= room.height }
        if tiles.indices.contains(main), fits(tiles[main]) { return main }
        return tiles.indices.filter { fits(tiles[$0]) }.max { tiles[$0].width < tiles[$1].width }
    }

    /// Where the keyboard-and-mouse badge of `size` goes on `tile`: in the
    /// middle, or in the middle of the space below the name when the name,
    /// ending at `nameBottom`, would cover it, keeping `inset` from the
    /// tile's edges. Without that much room below the name it sits as low
    /// as the tile allows. A badge larger than the tile stays centered on
    /// it and spills over evenly, since it is the only sign of where the
    /// keyboard and mouse are.
    static func badgeFrame(_ size: CGSize, on tile: CGRect, nameBottom: CGFloat?, inset: CGFloat) -> CGRect {
        var frame = CGRect(
            x: tile.midX - size.width / 2, y: tile.midY - size.height / 2,
            width: size.width, height: size.height)
        let gap: CGFloat = 1
        let bottom = tile.maxY - inset
        guard let nameBottom, frame.minY < nameBottom + gap,
              size.height <= tile.height - 2 * inset
        else { return frame }
        let top = nameBottom + gap
        frame.origin.y = bottom - top >= size.height
            ? (top + bottom - size.height) / 2
            : bottom - size.height
        return frame
    }

    /// The edge `p` shares with `q` where `q` is right of or below `p`.
    private static func seam(_ p: CGRect, _ q: CGRect) -> Seam? {
        if abs(p.maxX - q.minX) < epsilon {
            let (top, bottom) = (max(p.minY, q.minY), min(p.maxY, q.maxY))
            if bottom - top > epsilon {
                return Seam(start: CGPoint(x: q.minX, y: top), end: CGPoint(x: q.minX, y: bottom))
            }
        }
        if abs(p.maxY - q.minY) < epsilon {
            let (left, right) = (max(p.minX, q.minX), min(p.maxX, q.maxX))
            if right - left > epsilon {
                return Seam(start: CGPoint(x: left, y: q.minY), end: CGPoint(x: right, y: q.minY))
            }
        }
        return nil
    }
}
