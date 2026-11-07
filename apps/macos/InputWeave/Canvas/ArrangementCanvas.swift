import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// The arrangement: every device as a group of its screens at their real
/// relative sizes, on the dotted canvas, with seams stitched where devices
/// meet. Interactive, the user drags whole devices; a device's own screens
/// keep the layout its operating system gives them. Files dropped on a
/// connected computer are sent to it. Clicking a computer on the mini map,
/// or its context menu on the canvas, moves the cursor there.
struct ArrangementCanvas: View {
    @Environment(AgentModel.self) private var model
    @Environment(\.undoManager) private var undoManager
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    /// Draggable, with selection; otherwise a read-only mini map.
    var interactive = true
    @Binding var selection: String?
    var padding: CGFloat = 40

    @State private var dragging: Drag?
    @State private var refused: String?
    /// Files are being dragged over the canvas.
    @State private var draggingFiles = false
    /// The computer files would be sent to if dropped now.
    @State private var dropTarget: String?
    @FocusState private var focused: Bool

    private struct Drag {
        let id: String
        var offset: CGPoint
        /// The fit when the drag started, so the canvas does not rescale
        /// under the pointer.
        let fit: CanvasGeometry.Fit
        let start: CGPoint
    }

    /// Snap distance in view points.
    private let snapDistance: CGFloat = 8
    /// Arrow keys move a device this many view points.
    private let nudge: CGFloat = 10

    private var devices: [CanvasDevice] {
        model.arrangement.devices
            .filter { $0.placed && !$0.screens.isEmpty }
            .map { d in
                var offset = CGPoint(x: d.x, y: d.y)
                if let dragging, dragging.id == d.id { offset = dragging.offset }
                return CanvasDevice(
                    id: d.id, offset: offset,
                    screens: d.screens.map { CGRect(x: $0.x, y: $0.y, width: $0.width, height: $0.height) })
            }
    }

    var body: some View {
        GeometryReader { geometry in
            let devices = devices
            let fit = dragging?.fit
                ?? CanvasGeometry.fit(
                    CanvasGeometry.union(devices.flatMap(\.frames)) ?? .zero,
                    in: geometry.size, padding: padding)
            ZStack(alignment: .topLeading) {
                DotGrid(spacing: interactive ? 16 : 10)
                ForEach(devices) { device in
                    deviceView(device, fit: fit, all: devices)
                }
                // On top: the stitches cross both screens at the seam.
                ForEach(Array(CanvasGeometry.seams(devices).enumerated()), id: \.offset) { _, seam in
                    StitchedSeam(start: fit.toView(seam.start), end: fit.toView(seam.end), compact: !interactive)
                }
                // Above the seams too, so nothing covers it.
                badge(devices, fit: fit)
            }
            .frame(width: geometry.size.width, height: geometry.size.height)
            .clipped()
            .contentShape(Rectangle())
            .onDrop(
                of: [.fileURL],
                delegate: FileDrop(
                    frames: devices.map { (id: $0.id, frame: fit.toView($0.bounds)) },
                    canReceive: canReceive, active: $draggingFiles, target: $dropTarget,
                    send: send))
        }
        .background(Color.canvas)
        .focusable(interactive)
        .focused($focused)
        .focusEffectDisabled()
        .onKeyPress(keys: [.leftArrow, .rightArrow, .upArrow, .downArrow]) { press in
            guard interactive, let selection else { return .ignored }
            let step = nudge / currentScale
            let delta = switch press.key {
            case .leftArrow: CGPoint(x: -step, y: 0)
            case .rightArrow: CGPoint(x: step, y: 0)
            case .upArrow: CGPoint(x: 0, y: -step)
            default: CGPoint(x: 0, y: step)
            }
            moveBy(selection, delta)
            return .handled
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text("Arrangement"))
    }

    /// The scale the canvas was last drawn at, for keyboard nudges.
    @State private var currentScale: CGFloat = 0.1

    @ViewBuilder
    private func deviceView(_ device: CanvasDevice, fit: CanvasGeometry.Fit, all: [CanvasDevice]) -> some View {
        let info = model.arrangement.devices.first { $0.id == device.id }
        let frame = fit.toView(device.bounds)
        let offline = model.offlineOffer(device.id)
        DeviceGroup(
            device: device, info: info, fit: fit,
            selected: interactive && selection == device.id,
            active: model.status.activeDeviceID == device.id && model.controllingOther,
            compact: !interactive,
            jump: model.jumpShortcut(slot: info?.jumpSlot ?? 0),
            nameSymbol: ConnectionText.nameSymbol(
                offlineForNow: model.showsOfflineForNow(device.id),
                screenLocked: screenLocked(device.id),
                wakeable: WakeText.showsMoon(model.device(device.id), supported: model.wakeSupported))
        )
        .frame(width: frame.width, height: frame.height)
        .offset(x: frame.minX, y: frame.minY)
        .modifier(Shake(amount: refused == device.id ? 1 : 0))
        .modifier(DropHighlight(
            state: !draggingFiles ? .idle
                : dropTarget == device.id ? .target
                : canReceive(device.id) ? .receivable
                : .unavailable(info?.thisDevice == true ? nil : unavailableReason(device.id)),
            compact: !interactive))
        .onAppear { currentScale = fit.scale }
        .onChange(of: fit) { _, fit in currentScale = fit.scale }
        .gesture(interactive ? dragGesture(device, fit: fit, all: all) : nil)
        .onTapGesture {
            if interactive {
                selection = device.id
                focused = true
            } else if model.canJump(to: device.id) {
                jump(to: device.id)
            }
        }
        .modifier(JumpMenu(
            shown: interactive,
            // While this computer is offline for now, or the profile in use
            // keeps another computer away, its menu has only Rename….
            jumps: device.id == model.thisDeviceID || (!model.offlineHere && !model.keptAway(device.id)),
            enabled: model.canJump(to: device.id), jump: { jump(to: device.id) },
            rename: model.device(device.id) == nil ? nil : { rename(device.id) },
            wake: model.wakeOffer(device.id) == .hidden ? nil : { model.wake(device.id) },
            wakeEnabled: model.wakeOffer(device.id) == .enabled,
            offline: OfflineText.label(offline).map { label in
                (label, { model.setOffline(device.id, offline == .takeOffline) })
            }))
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(accessibilityLabel(device, info: info, all: all))
        .accessibilityValue(accessibilityValue(device.id))
        .accessibilityAddTraits(selection == device.id ? .isSelected : [])
        .accessibilityAddTraits(interactive ? [] : .isButton)
        .accessibilityAction(named: Text("Move Cursor Here")) {
            if model.canJump(to: device.id) { jump(to: device.id) }
        }
    }

    /// The keyboard-and-mouse badge on the main screen of the computer they
    /// are on. While files are dragged over the canvas, the computers say
    /// whether they can take them instead.
    @ViewBuilder
    private func badge(_ devices: [CanvasDevice], fit: CanvasGeometry.Fit) -> some View {
        let id = model.cursorDeviceID
        if !draggingFiles, let device = devices.first(where: { $0.id == id }) {
            let main = model.arrangement.devices.first { $0.id == id }?.screens.firstIndex { $0.primary } ?? 0
            let tiles = device.frames.map { fit.toView($0) }
            if let tile = tiles[safe: main] {
                BadgePlacement(
                    tile: tile, nameBottom: NameCapsule.bottom(on: tiles, main: main, compact: !interactive),
                    inset: interactive ? NameCapsule.inset : 2
                ) {
                    KeyboardMouseBadge(compact: !interactive)
                        .modifier(LocatePulse(trigger: model.cursorSighting?.serial))
                }
                .modifier(Shake(amount: refused == id ? 1 : 0))
                .allowsHitTesting(false)
                .accessibilityHidden(true)
            }
        }
    }

    private func dragGesture(_ device: CanvasDevice, fit: CanvasGeometry.Fit, all: [CanvasDevice]) -> some Gesture {
        DragGesture(minimumDistance: 2)
            .onChanged { value in
                let start = dragging?.start ?? device.offset
                let fit = dragging?.fit ?? fit
                selection = device.id
                var moving = device
                moving.offset = CGPoint(
                    x: start.x + value.translation.width / fit.scale,
                    y: start.y + value.translation.height / fit.scale)
                let others = all.filter { $0.id != device.id }
                let snapped = CanvasGeometry.snap(moving, among: others, threshold: snapDistance / fit.scale)
                dragging = Drag(id: device.id, offset: snapped, fit: fit, start: start)
            }
            .onEnded { _ in
                guard let drag = dragging else { return }
                commit(drag.id, from: drag.start, to: drag.offset, all: all)
            }
    }

    private func moveBy(_ id: String, _ delta: CGPoint) {
        guard let device = devices.first(where: { $0.id == id }) else { return }
        let to = CGPoint(x: device.offset.x + delta.x, y: device.offset.y + delta.y)
        commit(id, from: device.offset, to: to, all: devices)
    }

    /// Applies a move unless it makes screens overlap, in which case the
    /// device springs back.
    private func commit(_ id: String, from: CGPoint, to: CGPoint, all: [CanvasDevice]) {
        guard let device = all.first(where: { $0.id == id }) else { return }
        var moved = device
        moved.offset = to
        let others = all.filter { $0.id != id }
        if CanvasGeometry.overlaps(moved.frames, others.flatMap(\.frames)) || from == to {
            withAnimation(reduceMotion ? nil : .spring(duration: 0.2)) { dragging = nil }
            if from != to { flagRefused(id) }
            return
        }
        Task {
            do {
                try await model.move([(id: id, x: to.x, y: to.y)])
                undoManager?.registerUndo(withTarget: model) { model in
                    Task { @MainActor in try? await model.move([(id: id, x: from.x, y: from.y)]) }
                }
                undoManager?.setActionName(String(localized: "Move Device"))
            } catch {
                flagRefused(id)
            }
            withAnimation(reduceMotion ? nil : .spring(duration: 0.2)) { dragging = nil }
        }
    }

    /// Moves the cursor to a computer. Refusals the panel already explains
    /// (sharing paused, cursor locked, the profile in use paused or leaving
    /// it out) are left at that; others shake the computer.
    private func jump(to id: String) {
        Task {
            do {
                try await model.jump(to: id)
            } catch let refusal as AgentRefusal
                where ["sharing_paused", "locked", "not_in_profile", "profile_paused"].contains(refusal.code)
            {
                // The panel's headline, or the computer's status, says why.
            } catch {
                flagRefused(id)
            }
        }
    }

    /// Whether a connected computer's screen is locked.
    private func screenLocked(_ id: String) -> Bool {
        model.device(id).map { $0.state == .connected && $0.screenLocked } ?? false
    }

    /// What VoiceOver says of a computer's state: taken offline for now,
    /// left out of the profile in use, its screen locked, the keyboard and
    /// mouse on it.
    private func accessibilityValue(_ id: String) -> Text {
        if model.showsOfflineForNow(id) {
            return Text(OfflineText.status())
        }
        if model.showsNotInProfile(id) {
            return Text(ProfileText.notInProfile())
        }
        let here = model.cursorDeviceID == id
        if screenLocked(id) && model.outOfReach(id) {
            return Text("Screen locked, can't take input")
        }
        if model.device(id)?.inputUnavailable == true {
            return Text("Can't take input")
        }
        switch (screenLocked(id), here) {
        case (true, true): return Text("Screen locked, keyboard and mouse here")
        case (true, false): return Text("Screen locked")
        case (false, true): return Text("Keyboard and mouse here")
        case (false, false): return Text(verbatim: "")
        }
    }

    private func canReceive(_ id: String) -> Bool {
        model.canReceiveFiles(id)
    }

    /// Why files dropped on another computer would not go there.
    private func unavailableReason(_ id: String) -> String {
        if model.showsOfflineForNow(id) { return OfflineText.status() }
        if model.showsNotInProfile(id) { return ProfileText.notInProfile() }
        guard model.device(id)?.state == .connected else { return String(localized: "Offline") }
        guard model.filesOff(id) else {
            // Connected, so the profile in use keeps files off.
            return interactive ? SendTargets.reason(.filesOff) : DeviceSharingText.filesOffShort
        }
        return interactive ? DeviceSharingText.filesOff(model.name(of: id)) : DeviceSharingText.filesOffShort
    }

    /// Selects a computer and puts the cursor in its name field.
    private func rename(_ id: String) {
        selection = id
        AppActions.navigation.renamingDevice = id
    }

    private func send(_ urls: [URL], to id: String) {
        Task {
            do {
                try await model.sendFiles(urls, to: id)
            } catch {
                flagRefused(id)
            }
        }
    }

    private func flagRefused(_ id: String) {
        withAnimation(reduceMotion ? nil : .linear(duration: 0.3)) { refused = id }
        Task {
            try? await Task.sleep(for: .milliseconds(350))
            refused = nil
        }
    }

    private func accessibilityLabel(_ device: CanvasDevice, info: ArrangedDevice?, all: [CanvasDevice]) -> Text {
        let name = info?.name ?? device.id
        let count = device.screens.count
        let neighbours = all.filter { $0.id != device.id }
            .filter { !CanvasGeometry.seams([device, $0]).isEmpty }
            .map { other in model.name(of: other.id) }
        if neighbours.isEmpty {
            return Text("\(name), \(count) screens, not next to another computer")
        }
        return Text("\(name), \(count) screens, next to \(neighbours.formatted())")
    }
}

/// Where a computer's name sits, so the keyboard-and-mouse badge can keep
/// clear of it.
private enum NameCapsule {
    /// The arrangement page's: how far inside the main screen's corner, and
    /// at least its height.
    static let inset: CGFloat = 6
    static let height: CGFloat = 18
    /// The mini map's: how far inside its tile's corner it sits, its height,
    /// and the least width worth showing (a few letters). A tile without
    /// room for it gets no name.
    static let compactInset: CGFloat = 3
    static let compactSize = CGSize(width: 30, height: 14)
    /// The mini map's name runs this narrow at least before the jump
    /// shortcut beside it is left out.
    static let compactNameWidth: CGFloat = 24
    /// What the symbol beside the name (a slashed network, a lock, a moon)
    /// takes on the mini map, with the gap before it.
    static let symbolWidth: CGFloat = 12

    /// Whether the mini map has room for the symbol beside the name on a
    /// tile `width` wide, with the capsule's padding and the name's least
    /// width.
    static func fitsSymbol(width: CGFloat) -> Bool {
        2 * compactInset + 8 + compactNameWidth + symbolWidth <= width
    }

    /// Whether the mini map has room for `shortcut` beside the name on a
    /// tile `width` wide.
    static func fitsShortcut(_ shortcut: String, width: CGFloat) -> Bool {
        let font = NSFont.systemFont(
            ofSize: NSFont.preferredFont(forTextStyle: .caption2).pointSize, weight: .semibold)
        let text = (shortcut as NSString).size(withAttributes: [.font: font]).width
        // The capsule's padding, the gap, and the name's least width.
        return 2 * compactInset + 8 + 3 + compactNameWidth + ceil(text) <= width
    }

    /// The tile the mini map names a computer on, if any.
    static func compactTile(_ tiles: [CGRect], main: Int) -> Int? {
        CanvasGeometry.labelTile(
            tiles, main: main,
            room: CGSize(width: compactSize.width + 2 * compactInset, height: compactSize.height + 2 * compactInset))
    }

    /// Where the name on the main screen's tile ends, or `nil` when the
    /// mini map names the computer on another of its screens, or not at all.
    static func bottom(on tiles: [CGRect], main: Int, compact: Bool) -> CGFloat? {
        guard let tile = tiles[safe: main] else { return nil }
        if !compact { return tile.minY + inset + height }
        return compactTile(tiles, main: main) == main ? tile.minY + compactInset + compactSize.height : nil
    }
}

/// One computer: an outline around its screens, its name, and the screens
/// as tiles, each with the computer's desktop picture when it shares one.
private struct DeviceGroup: View {
    let device: CanvasDevice
    let info: ArrangedDevice?
    let fit: CanvasGeometry.Fit
    let selected: Bool
    let active: Bool
    let compact: Bool
    /// The shortcut that moves the cursor here, if any.
    let jump: String?
    /// The symbol beside the name, if any: a slashed network while it is
    /// taken offline for now, a lock while its screen is locked, a moon
    /// while it is asleep and can be woken (`ConnectionText.nameSymbol`).
    let nameSymbol: String?

    var body: some View {
        let bounds = device.bounds
        let online = info?.connected ?? false
        let tiles = device.screens.map { screen in
            CGRect(
                x: (screen.minX - bounds.minX + device.offset.x) * fit.scale,
                y: (screen.minY - bounds.minY + device.offset.y) * fit.scale,
                width: screen.width * fit.scale, height: screen.height * fit.scale)
        }
        // Names go on the main screen, where the bounding box's corner may
        // be empty or belong to another device.
        let main = info?.screens.firstIndex { $0.primary } ?? 0
        let name = info?.name ?? device.id
        ZStack(alignment: .topLeading) {
            ForEach(Array(tiles.enumerated()), id: \.offset) { index, rect in
                let screen = info?.screens[safe: index]
                ScreenTile(
                    screen: screen,
                    picture: screen.flatMap {
                        DesktopPictureCache.shared.picture(device: device.id, screen: $0.id, data: $0.previewJpeg)
                    },
                    showsDetail: !compact && rect.width > 90)
                    .frame(width: rect.width, height: rect.height)
                    .offset(x: rect.minX, y: rect.minY)
            }
            if compact {
                if let labeled = NameCapsule.compactTile(tiles, main: main) {
                    let tile = tiles[labeled]
                    let inset = NameCapsule.compactInset
                    let symbol = nameSymbol.flatMap { NameCapsule.fitsSymbol(width: tile.width) ? $0 : nil }
                    let room = tile.width - (symbol == nil ? 0 : NameCapsule.symbolWidth)
                    let shortcut = jump.flatMap { NameCapsule.fitsShortcut($0, width: room) ? $0 : nil }
                    HStack(spacing: 3) {
                        Text(name)
                            .lineLimit(1)
                            .truncationMode(.tail)
                        if let symbol {
                            Image(systemName: symbol).imageScale(.small)
                        }
                        if let shortcut {
                            Text(verbatim: shortcut)
                                .foregroundStyle(.secondary)
                                .fixedSize()
                        }
                    }
                    .font(.caption2.weight(.semibold))
                    .padding(.horizontal, 4)
                    .frame(height: NameCapsule.compactSize.height)
                    .background(.regularMaterial, in: Capsule())
                    .frame(maxWidth: tile.width - 2 * inset, alignment: .leading)
                    .offset(x: tile.minX + inset, y: tile.minY + inset)
                }
            } else {
                let corner = tiles[safe: main]?.origin ?? .zero
                HStack(spacing: 4) {
                    Label(name, systemImage: OSLabel(os: info?.os ?? "").symbol)
                    if let nameSymbol {
                        Image(systemName: nameSymbol).imageScale(.small)
                    }
                    if let jump {
                        Text(verbatim: jump).foregroundStyle(.secondary)
                    }
                }
                .font(.caption.weight(.semibold))
                .lineLimit(1)
                .padding(.horizontal, 6)
                .padding(.vertical, 2)
                .background(.regularMaterial, in: Capsule())
                .padding(NameCapsule.inset)
                .offset(x: corner.x, y: corner.y)
            }
        }
        // Screens are placed from the group's top-left corner; without this
        // the parent frame would center them.
        .frame(width: bounds.width * fit.scale, height: bounds.height * fit.scale, alignment: .topLeading)
        .opacity(online || info?.thisDevice == true ? 1 : 0.55)
        .overlay {
            RoundedRectangle(cornerRadius: 6)
                .strokeBorder(
                    selected ? Color.accentColor : (active ? Color.thread : Color.clear),
                    style: StrokeStyle(lineWidth: selected || active ? 2 : 1))
                .padding(-3)
        }
        .overlay {
            if !(online || info?.thisDevice == true) {
                // Offline devices are drawn dashed, not just faded.
                RoundedRectangle(cornerRadius: 6)
                    .strokeBorder(Color.tileEdge, style: StrokeStyle(lineWidth: 1, dash: [4, 3]))
                    .padding(-3)
            }
        }
        .contentShape(Rectangle())
    }
}

/// "Move Cursor Here", and "Rename…" for another computer, in a computer's
/// context menu on the arrangement page; then "Wake" for another computer
/// that can be woken, and "Take Offline" or "Bring Back Online" for
/// another computer.
private struct JumpMenu: ViewModifier {
    let shown: Bool
    /// "Move Cursor Here" is offered: not for another computer while this
    /// one is offline for now.
    var jumps = true
    let enabled: Bool
    let jump: () -> Void
    let rename: (() -> Void)?
    var wake: (() -> Void)?
    var wakeEnabled = false
    /// The words for taking it offline for now or bringing it back, and
    /// doing it.
    var offline: (label: String, action: () -> Void)?

    func body(content: Content) -> some View {
        if shown {
            content.contextMenu {
                if jumps {
                    Button("Move Cursor Here", action: jump)
                        .disabled(!enabled)
                }
                if let rename {
                    Button("Rename…", action: rename)
                }
                if wake != nil || offline != nil {
                    Divider()
                }
                if let wake {
                    Button("Wake", action: wake)
                        .disabled(!wakeEnabled)
                }
                if let offline {
                    Button(offline.label, action: offline.action)
                }
            }
        } else {
            content
        }
    }
}

/// A screen: the tile color, or the computer's desktop picture filling it
/// under a veil of the tile color that keeps names, seams and selection
/// clear; then its edge, and on the arrangement page its resolution.
private struct ScreenTile: View {
    let screen: Inputweave_App_V1_Screen?
    let picture: NSImage?
    let showsDetail: Bool
    @Environment(\.colorScheme) private var colorScheme

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: 3)
        shape
            .fill(Color.tile)
            .overlay {
                if let picture {
                    // The whole picture is sent, whatever the screen's shape:
                    // it fills the screen and is cropped to it.
                    Color.clear
                        .overlay {
                            Image(nsImage: picture)
                                .resizable()
                                .interpolation(.high)
                                .scaledToFill()
                        }
                        .overlay { Color.tile.opacity(colorScheme == .dark ? 0.35 : 0.25) }
                        .clipShape(shape)
                        .allowsHitTesting(false)
                }
            }
            .overlay { shape.strokeBorder(Color.tileEdge, lineWidth: 1) }
            .overlay(alignment: .bottomTrailing) {
                if showsDetail, let screen {
                    let detail = VStack(alignment: .trailing, spacing: 1) {
                        if screen.primary {
                            Text("Main").font(.caption2.weight(.semibold))
                        }
                        Text(verbatim: "\(Int((screen.width * screen.scale).rounded())) × \(Int((screen.height * screen.scale).rounded()))")
                            .font(.caption2)
                            .monospacedDigit()
                    }
                    .foregroundStyle(.secondary)
                    if picture == nil {
                        detail.padding(5)
                    } else {
                        // On a picture, on material of its own to stay readable.
                        detail
                            .padding(.horizontal, 4)
                            .padding(.vertical, 2)
                            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 4))
                            .padding(3)
                    }
                }
            }
    }
}

/// The thread where two devices meet: stitches crossing the seam back and
/// forth. With Increase Contrast it is a solid, thicker line.
struct StitchedSeam: View {
    let start: CGPoint
    let end: CGPoint
    /// Finer stitches for the mini map.
    var compact = false
    @Environment(\.colorSchemeContrast) private var contrast
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var progress: CGFloat = 0

    var body: some View {
        SeamShape(
            start: start, end: end, solid: contrast == .increased,
            pitch: compact ? 4 : 6, reach: compact ? 2 : 3.5)
            .trim(from: 0, to: progress)
            .stroke(
                Color.thread,
                style: StrokeStyle(
                    lineWidth: contrast == .increased ? 3 : (compact ? 1.5 : 2), lineCap: .round, lineJoin: .round))
            .allowsHitTesting(false)
            .onAppear {
                // The one deliberate animation: the seam is sewn shut.
                withAnimation(reduceMotion ? nil : .easeOut(duration: 0.24)) { progress = 1 }
            }
    }
}

private struct SeamShape: Shape {
    let start: CGPoint
    let end: CGPoint
    let solid: Bool
    let pitch: CGFloat
    let reach: CGFloat

    func path(in _: CGRect) -> Path {
        var path = Path()
        let length = hypot(end.x - start.x, end.y - start.y)
        guard length > 0 else { return path }
        if solid {
            path.move(to: start)
            path.addLine(to: end)
            return path
        }
        // A zigzag stitch across the seam.
        let (ux, uy) = ((end.x - start.x) / length, (end.y - start.y) / length)
        let (nx, ny) = (-uy, ux)
        let count = max(1, Int(length / pitch))
        path.move(to: start)
        for i in 1...count {
            let t = CGFloat(i) / CGFloat(count) * length
            let side: CGFloat = i.isMultiple(of: 2) ? -1 : 1
            let point = CGPoint(
                x: start.x + ux * t + nx * reach * (i == count ? 0 : side),
                y: start.y + uy * t + ny * reach * (i == count ? 0 : side))
            path.addLine(to: point)
        }
        return path
    }
}

/// Sends files dropped on a computer that can receive them.
private struct FileDrop: DropDelegate {
    let frames: [(id: String, frame: CGRect)]
    let canReceive: (String) -> Bool
    @Binding var active: Bool
    @Binding var target: String?
    let send: ([URL], String) -> Void

    func validateDrop(info: DropInfo) -> Bool {
        info.hasItemsConforming(to: [.fileURL])
    }

    func dropEntered(info: DropInfo) {
        active = true
        aim(info)
    }

    func dropUpdated(info: DropInfo) -> DropProposal? {
        aim(info)
        return DropProposal(operation: target == nil ? .forbidden : .copy)
    }

    func dropExited(info _: DropInfo) {
        active = false
        target = nil
    }

    func performDrop(info: DropInfo) -> Bool {
        defer {
            active = false
            target = nil
        }
        guard let target else { return false }
        let providers = info.itemProviders(for: [.fileURL])
        Task { @MainActor in
            let urls = await Self.urls(from: providers)
            if !urls.isEmpty { send(urls, target) }
        }
        return true
    }

    private func aim(_ info: DropInfo) {
        let hit = frames.first { $0.frame.insetBy(dx: -4, dy: -4).contains(info.location) }?.id
        target = hit.flatMap { canReceive($0) ? $0 : nil }
    }

    @MainActor
    private static func urls(from providers: [NSItemProvider]) async -> [URL] {
        var urls: [URL] = []
        for provider in providers {
            let url = await withCheckedContinuation { (done: CheckedContinuation<URL?, Never>) in
                _ = provider.loadObject(ofClass: URL.self) { url, _ in done.resume(returning: url) }
            }
            if let url { urls.append(url) }
        }
        return urls
    }
}

/// How a computer looks while files are dragged over the canvas: those
/// that can receive them rise, the one under the pointer most; the others
/// fade, saying why.
private struct DropHighlight: ViewModifier {
    enum State: Equatable {
        case idle, receivable, target
        case unavailable(String?)
    }

    let state: State
    let compact: Bool
    @Environment(\.accessibilityReduceMotion) private var reduceMotion

    func body(content: Content) -> some View {
        content
            .scaleEffect(state == .target ? 1.06 : 1)
            .shadow(color: .black.opacity(state == .receivable || state == .target ? 0.25 : 0), radius: 6, y: 2)
            .opacity(isUnavailable ? 0.35 : 1)
            .overlay {
                if state == .target {
                    RoundedRectangle(cornerRadius: 6)
                        .strokeBorder(Color.accentColor, lineWidth: 2)
                        .padding(-3)
                }
            }
            .overlay {
                if let label {
                    Text(label)
                        .font(compact ? .caption2.weight(.semibold) : .caption.weight(.semibold))
                        .lineLimit(1)
                        .padding(.horizontal, 6)
                        .padding(.vertical, 2)
                        .background(.regularMaterial, in: Capsule())
                }
            }
            .animation(reduceMotion ? nil : .easeOut(duration: 0.15), value: state)
    }

    private var isUnavailable: Bool {
        if case .unavailable = state { return true }
        return false
    }

    private var label: String? {
        switch state {
        case .receivable, .target: String(localized: "Drop to send")
        case .unavailable(let reason): reason
        case .idle: nil
        }
    }
}

/// The canvas's dot grid.
private struct DotGrid: View {
    let spacing: CGFloat

    var body: some View {
        Canvas { context, size in
            var y = spacing / 2
            while y < size.height {
                var x = spacing / 2
                while x < size.width {
                    context.fill(Path(ellipseIn: CGRect(x: x - 1, y: y - 1, width: 2, height: 2)), with: .color(.canvasGrid))
                    x += spacing
                }
                y += spacing
            }
        }
        .allowsHitTesting(false)
    }
}

/// A short horizontal shake, for a refused move or a wrong code.
struct Shake: GeometryEffect {
    var amount: CGFloat

    var animatableData: CGFloat {
        get { amount }
        set { amount = newValue }
    }

    func effectValue(size _: CGSize) -> ProjectionTransform {
        ProjectionTransform(CGAffineTransform(translationX: 6 * sin(amount * .pi * 4), y: 0))
    }
}

extension Array {
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}
