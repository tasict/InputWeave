import AppKit

/// The menu bar icon: two screens side by side
/// with the stitches that join them, as a template image the system tints.
/// It is the mark at its smallest level of detail (docs/brand/README.md):
/// no stands, no cursor, no dashes. Each state changes one bold element,
/// so it reads without color and at 1x.
enum StatusIcon {
    enum State: Equatable {
        case normal
        /// Controlling another computer: the other screen is lit.
        case controlling
        /// Controlled from elsewhere: the whole icon knocked out of a tile.
        case controlled
        /// Sharing paused: the stitches are gone.
        case paused
        /// Something needs the user: a dot in the corner.
        case attention
    }

    static func image(_ state: State) -> NSImage {
        let image = NSImage(size: NSSize(width: 22, height: 16), flipped: true) { _ in
            draw(state)
            return true
        }
        image.isTemplate = true
        image.accessibilityDescription = description(state)
        return image
    }

    /// What VoiceOver reads for the icon itself; the panel says more.
    static func description(_ state: State) -> String {
        switch state {
        case .normal: String(localized: "InputWeave, sharing")
        case .controlling: String(localized: "InputWeave, controlling another computer")
        case .controlled: String(localized: "InputWeave, controlled by another computer")
        case .paused: String(localized: "InputWeave, sharing paused")
        case .attention: String(localized: "InputWeave, needs attention")
        }
    }

    // Two equal screens, 3 points apart; the stitches cross that seam.
    private static let left = NSRect(x: 1.5, y: 3.5, width: 8, height: 9)
    private static let right = NSRect(x: 12.5, y: 3.5, width: 8, height: 9)
    private static let corner: CGFloat = 2
    private static let edge: CGFloat = 1.5
    private static let stitchYs: [CGFloat] = [6, 10]
    private static let stitchWidth: CGFloat = 2

    private static func draw(_ state: State) {
        let ink = NSColor.black
        if state == .controlled {
            ink.setFill()
            NSBezierPath(roundedRect: NSRect(x: 0, y: 0, width: 22, height: 16), xRadius: 4, yRadius: 4).fill()
            // Knocked out of the tile.
            NSGraphicsContext.current?.compositingOperation = .destinationOut
        }
        ink.setStroke()
        ink.setFill()
        screen(left, lit: false)
        screen(right, lit: state == .controlling)
        if state != .paused {
            for y in stitchYs {
                let stitch = NSBezierPath()
                stitch.move(to: NSPoint(x: 9, y: y))
                stitch.line(to: NSPoint(x: 13, y: y))
                stitch.lineWidth = stitchWidth
                stitch.lineCapStyle = .round
                stitch.stroke()
            }
        }
        if state == .attention {
            // A ring of clear space keeps the dot apart from the screen.
            NSGraphicsContext.current?.compositingOperation = .destinationOut
            NSBezierPath(ovalIn: NSRect(x: 15.5, y: -1, width: 7.5, height: 7.5)).fill()
            NSGraphicsContext.current?.compositingOperation = .sourceOver
            NSBezierPath(ovalIn: NSRect(x: 16.75, y: 0.25, width: 5, height: 5)).fill()
        }
    }

    private static func screen(_ rect: NSRect, lit: Bool) {
        if lit {
            let outer = rect.insetBy(dx: -edge / 2, dy: -edge / 2)
            NSBezierPath(roundedRect: outer, xRadius: corner + edge / 2, yRadius: corner + edge / 2).fill()
        } else {
            let path = NSBezierPath(roundedRect: rect, xRadius: corner, yRadius: corner)
            path.lineWidth = edge
            path.stroke()
        }
    }
}

extension StatusIcon.State {
    /// The icon for what the app knows. The dot shows only for what the
    /// panel asks the user to fix: the background
    /// service turned off, a missing permission, or a service that has not
    /// answered for longer than a restart takes. Not while it starts or
    /// updates.
    @MainActor
    init(_ model: AgentModel, serviceEnabled: Bool, now: Date) {
        if !serviceEnabled || model.isUnresponsive(at: now) {
            self = .attention
            return
        }
        // Until the service answers, its last status may be old or empty.
        guard model.link == .connected else {
            self = .normal
            return
        }
        let status = model.status
        if !model.missingPermissions.isEmpty {
            self = .attention
        } else if !status.sharing {
            self = .paused
        } else if !status.controlledByDeviceID.isEmpty {
            self = .controlled
        } else if model.controllingOther {
            self = .controlling
        } else {
            self = .normal
        }
    }
}
