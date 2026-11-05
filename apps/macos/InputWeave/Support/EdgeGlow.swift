import AppKit
import SwiftUI

/// Lights up the edge of a screen where the cursor crosses to or from
/// another computer: a short glow in the seam
/// color along the whole edge, brightest where it crossed, in a panel that
/// takes neither focus nor clicks, on every Space and over full-screen
/// apps, as the cursor locator is. Each edge of each display has its own
/// glow, fading on its own; crossing the same edge again while it still
/// shows carries on from how bright it is, its brightest part sliding to
/// the new crossing. Off when the user turned it off (General › Switching).
@MainActor
final class EdgeGlow {
    /// One edge's glow panel and what it shows, kept for the next crossing
    /// there.
    @MainActor private final class Strip {
        let panel: NSPanel
        let look: EdgeGlowLook

        init(edge: EdgeGlowGeometry.Edge, solid: Bool, spot: Double) {
            look = EdgeGlowLook(edge: edge, solid: solid, spot: spot)
            let hosting = NSHostingView(rootView: EdgeGlowPanelView(look: look))
            panel = NSPanel(
                contentRect: .zero, styleMask: [.borderless, .nonactivatingPanel],
                backing: .buffered, defer: false)
            panel.isOpaque = false
            panel.backgroundColor = .clear
            panel.hasShadow = false
            // Over the menu bar and open menus, as the cursor locator is.
            panel.level = .popUpMenu
            panel.ignoresMouseEvents = true
            panel.collectionBehavior = [.canJoinAllSpaces, .fullScreenAuxiliary, .transient, .ignoresCycle]
            panel.contentView = hosting
        }
    }

    private let defaults: UserDefaults
    private var glows = EdgeGlowGeometry.Glows()
    /// At most four for each display, one for each edge.
    private var strips: [EdgeGlowGeometry.Glows.Key: Strip] = [:]
    /// Runs while any glow shows, to fade it.
    private var timer: Timer?

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    /// Whether the user wants the edges lit; on unless turned off.
    nonisolated static func isOn(_ defaults: UserDefaults) -> Bool {
        defaults.object(forKey: Preferences.showsEdgeGlow) as? Bool ?? true
    }

    /// Lights the edge the service said the cursor crossed; told of each
    /// crossing as it comes (`AgentModel.onCursorCrossed`).
    func light(_ crossed: Inputweave_App_V1_CursorCrossed) {
        guard Self.isOn(defaults), let edge = EdgeGlowGeometry.edge(crossed.edge) else { return }
        let now = Date()
        let all = NSScreen.screens
        let numbered = all.map { screen in
            (number: (screen.deviceDescription[NSDeviceDescriptionKey("NSScreenNumber")] as? NSNumber)?.uint32Value,
             frame: screen.frame)
        }
        let mainHeight = all.first?.frame.height ?? 0
        guard let index = EdgeGlowGeometry.screen(for: crossed, among: numbered, mainHeight: mainHeight) else {
            return
        }
        let solid = NSWorkspace.shared.accessibilityDisplayShouldIncreaseContrast
        let frame = EdgeGlowGeometry.strip(
            on: all[index].frame, edge: edge, thickness: solid ? EdgeGlowGeometry.solidDepth : EdgeGlowGeometry.depth)
        let spot = Double(EdgeGlowGeometry.fraction(of: crossed, edge: edge))
        let reduceMotion = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        let key = glows.light(
            display: crossed.displayID, edge: edge, now: now, timing: EdgeGlowGeometry.timing(reduceMotion: reduceMotion))

        let strip = strips[key] ?? Strip(edge: edge, solid: solid, spot: spot)
        strips[key] = strip
        strip.look.solid = solid
        if strip.panel.isVisible && !reduceMotion {
            // Still showing: its brightest part slides to the new crossing.
            withAnimation(.easeOut(duration: 0.15)) { strip.look.spot = spot }
        } else {
            strip.look.spot = spot
        }
        strip.panel.setFrame(frame, display: true)
        fade(at: now)
        strip.panel.orderFrontRegardless()
        if timer == nil {
            let timer = Timer(timeInterval: 1.0 / 60, repeats: true) { [weak self] _ in
                MainActor.assumeIsolated { self?.fade(at: Date()) }
            }
            RunLoop.main.add(timer, forMode: .common)
            self.timer = timer
        }
    }

    /// Sets each glow's opacity for `now`, puts away those gone, and stops
    /// once none shows.
    private func fade(at now: Date) {
        for (key, opacity) in glows.fade(at: now) {
            guard let panel = strips[key]?.panel else { continue }
            if let opacity {
                panel.alphaValue = opacity
            } else {
                panel.orderOut(nil)
            }
        }
        if glows.lit.isEmpty {
            timer?.invalidate()
            timer = nil
        }
    }
}

/// What one edge's glow shows; its panel's view follows it, so the
/// brightest part can slide to a new crossing.
@MainActor @Observable
final class EdgeGlowLook {
    let edge: EdgeGlowGeometry.Edge
    var solid: Bool
    var spot: Double

    init(edge: EdgeGlowGeometry.Edge, solid: Bool, spot: Double) {
        self.edge = edge
        self.solid = solid
        self.spot = spot
    }
}

private struct EdgeGlowPanelView: View {
    let look: EdgeGlowLook

    var body: some View {
        EdgeGlowView(edge: look.edge, solid: look.solid, spot: look.spot)
    }
}

/// The glow itself, along the whole edge: the seam color strongest at the
/// screen's edge and fading inward, brightest at `spot` (0 to 1 along the
/// edge, as `EdgeGlowGeometry.fraction` measures) and dimmer toward both
/// ends (`EdgeGlowGeometry.brightness`); with Increase Contrast, an even
/// solid line instead. `spot` animates.
struct EdgeGlowView: View, Animatable {
    let edge: EdgeGlowGeometry.Edge
    var solid = false
    var spot = 0.5

    nonisolated var animatableData: Double {
        get { spot }
        set { spot = newValue }
    }

    var body: some View {
        Group {
            if solid {
                Rectangle().fill(Color.thread)
            } else {
                Rectangle()
                    .fill(LinearGradient(
                        colors: [Color.thread.opacity(0.95), Color.thread.opacity(0)],
                        startPoint: outer, endPoint: inner))
                    .mask(LinearGradient(
                        stops: EdgeGlowGeometry.stops(spot: spot).map {
                            .init(color: .black.opacity($0.brightness), location: $0.location)
                        },
                        startPoint: alongStart, endPoint: alongEnd))
            }
        }
        .accessibilityHidden(true)
    }

    /// The side at the screen's edge, and the one toward its middle.
    private var outer: UnitPoint {
        switch edge {
        case .left: .leading
        case .right: .trailing
        case .top: .top
        case .bottom: .bottom
        }
    }

    private var inner: UnitPoint {
        switch edge {
        case .left: .trailing
        case .right: .leading
        case .top: .bottom
        case .bottom: .top
        }
    }

    private var alongStart: UnitPoint {
        edge == .left || edge == .right ? .top : .leading
    }

    private var alongEnd: UnitPoint {
        edge == .left || edge == .right ? .bottom : .trailing
    }
}
