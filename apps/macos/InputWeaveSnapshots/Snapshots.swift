import AppKit
import SwiftUI
import XCTest

/// Renders views into PNGs under $TMPDIR/InputWeaveSnapshots, to inspect.
@MainActor
final class Snapshots: XCTestCase {
    static let output = FileManager.default.temporaryDirectory.appending(path: "InputWeaveSnapshots")

    override class func setUp() {
        try? FileManager.default.createDirectory(at: output, withIntermediateDirectories: true)
    }

    func render(_ name: String, size: CGSize, dark: Bool = false, @ViewBuilder _ view: () -> some View) throws {
        // On the window's background, so the PNGs read on any page.
        let host = NSHostingView(
            rootView: view()
                .frame(width: size.width, height: size.height)
                .background(Color(nsColor: .windowBackgroundColor)))
        host.frame = CGRect(origin: .zero, size: size)
        let window = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        window.appearance = NSAppearance(named: dark ? .darkAqua : .aqua)
        window.contentView = host
        host.layoutSubtreeIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.6))
        let bitmap = try XCTUnwrap(host.bitmapImageRepForCachingDisplay(in: host.bounds))
        host.cacheDisplay(in: host.bounds, to: bitmap)
        let png = try XCTUnwrap(bitmap.representation(using: .png, properties: [:]))
        try png.write(to: Self.output.appending(path: "\(name).png"))
    }

    static func screen(_ id: String, _ w: Double, _ h: Double, x: Double = 0, y: Double = 0, primary: Bool = true, scale: Double = 2) -> Inputweave_App_V1_Screen {
        var s = Inputweave_App_V1_Screen()
        s.id = id; s.x = x; s.y = y; s.width = w; s.height = h; s.scale = scale; s.primary = primary
        return s
    }

    static func sampleTransfers() -> Transfers {
        func file(_ id: String, _ name: String, _ device: String, _ state: Inputweave_App_V1_TransferState,
                  outgoing: Bool = true, done: UInt64 = 0, rate: UInt64 = 0, reason: String = "") -> FileTransfer {
            var t = FileTransfer()
            t.transferID = id; t.name = name; t.deviceID = device; t.state = state; t.outgoing = outgoing
            t.size = 248_000_000; t.doneBytes = done; t.bytesPerSecond = rate; t.reason = reason
            t.path = "/Users/me/Downloads/InputWeave/\(name)"
            return t
        }
        var transfers = Transfers()
        transfers.receiveFolder = "/Users/me/Downloads/InputWeave"
        transfers.transfers = [
            // Waiting behind the files sent to the PC before it.
            file("6", "Fonts.zip", "pc", .queued),
            file("1", "Design.fig", "pc", .transferring, done: 158_000_000, rate: 41_000_000),
            file("2", "Quarterly report 2026.pdf", "pc", .waitingForAnswer, outgoing: false),
            file("3", "Photos.zip", "mini", .waitingForConnection, done: 60_000_000),
            file("4", "Notes.txt", "pc", .done, outgoing: false),
            file("5", "Video.mov", "pc", .declined, reason: "no_space"),
        ]
        transfers.transfers[5].missingBytes = 1_200_000_000
        return transfers
    }

    /// Folders going and done: one sent to the PC, part way through; one
    /// from the Mac mini that arrived with three files missing; and a file
    /// of the same batch.
    static func folderTransfers() -> Transfers {
        var going = FileTransfer()
        going.transferID = "f1"; going.name = "Photos 2026"; going.deviceID = "pc"; going.outgoing = true
        going.folder = true; going.state = .transferring
        going.size = 1_200_000_000; going.doneBytes = 450_000_000; going.bytesPerSecond = 38_000_000
        going.files = 312; going.filesDone = 117
        going.batchID = "b1"; going.batchItems = 1; going.batchFiles = 312; going.batchBytes = 1_200_000_000
        going.path = "/Users/me/Pictures/Photos 2026"

        var partly = FileTransfer()
        partly.transferID = "f2"; partly.name = "Project"; partly.deviceID = "mini"; partly.outgoing = false
        partly.folder = true; partly.state = .done
        partly.size = 86_000_000; partly.doneBytes = 84_500_000
        partly.files = 48; partly.filesDone = 45; partly.filesFailed = 3
        partly.batchID = "b2"; partly.batchIndex = 0; partly.batchItems = 2; partly.batchFiles = 49; partly.batchBytes = 86_400_000
        partly.path = "/Users/me/Downloads/InputWeave/Project"
        partly.failures = [
            ("Project/build/cache.db", "unreadable"), ("Project/assets/logo.psd", "file_changed"),
            ("Project/notes/old", "symlink"),
        ].map { path, reason in
            var failure = Inputweave_App_V1_FailedFile()
            failure.relativePath = path
            failure.reason = reason
            return failure
        }

        var file = FileTransfer()
        file.transferID = "f3"; file.name = "README.md"; file.deviceID = "mini"; file.outgoing = false
        file.state = .done; file.size = 400_000; file.doneBytes = 400_000; file.files = 1; file.filesDone = 1
        file.batchID = "b2"; file.batchIndex = 1; file.batchItems = 2; file.batchFiles = 49; file.batchBytes = 86_400_000
        file.path = "/Users/me/Downloads/InputWeave/README.md"

        var transfers = Transfers()
        transfers.receiveFolder = "/Users/me/Downloads/InputWeave"
        transfers.transfers = [going, partly, file]
        return transfers
    }

    /// A desktop picture made here, as the background service sends one: a
    /// 16:10 JPEG, 320 px wide, of a diagonal gradient with a soft glow.
    static func desktopPicture(_ from: NSColor, _ to: NSColor, glow: NSColor) -> Data {
        let (width, height) = (320, 200)
        let space = CGColorSpace(name: CGColorSpace.sRGB)!
        let context = CGContext(
            data: nil, width: width, height: height, bitsPerComponent: 8, bytesPerRow: 0, space: space,
            bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
        let gradient = CGGradient(colorsSpace: space, colors: [from.cgColor, to.cgColor] as CFArray, locations: nil)!
        context.drawLinearGradient(
            gradient, start: CGPoint(x: 0, y: height), end: CGPoint(x: width, y: 0), options: [])
        let center = CGPoint(x: Double(width) * 0.68, y: Double(height) * 0.62)
        let halo = CGGradient(
            colorsSpace: space, colors: [glow.cgColor, glow.withAlphaComponent(0).cgColor] as CFArray, locations: nil)!
        context.drawRadialGradient(
            halo, startCenter: center, startRadius: 0, endCenter: center, endRadius: Double(height) * 0.55, options: [])
        let rep = NSBitmapImageRep(cgImage: context.makeImage()!)
        return rep.representation(using: .jpeg, properties: [.compressionFactor: 0.7])!
    }

    /// A light picture for this Mac, and a dark one for the Mac mini. The
    /// Windows computer sends none, as Windows does not yet.
    static let lightPicture = desktopPicture(
        NSColor(srgbRed: 0.60, green: 0.78, blue: 0.95, alpha: 1), NSColor(srgbRed: 0.99, green: 0.85, blue: 0.72, alpha: 1),
        glow: NSColor(srgbRed: 1, green: 0.97, blue: 0.88, alpha: 0.9))
    static let darkPicture = desktopPicture(
        NSColor(srgbRed: 0.06, green: 0.08, blue: 0.22, alpha: 1), NSColor(srgbRed: 0.34, green: 0.15, blue: 0.46, alpha: 1),
        glow: NSColor(srgbRed: 0.96, green: 0.55, blue: 0.34, alpha: 0.85))

    static func sampleModel(
        controlling: Bool = false, missing: Bool = false, transfers: Bool = false, locked: Bool = false,
        pictures: Bool = false, folders: Bool = false, renamed: Bool = false, speed: UInt32 = 100,
        hotkeys: Hotkeys = ShortcutText.defaults, switchModifier: SwitchModifier = .none,
        license: LicenseRecord = LicenseRecord()
    ) -> AgentModel {
        var status = AgentStatus()
        status.license = license
        status.sharing = true
        status.locked = locked
        status.thisDeviceID = "mac"
        status.thisDeviceName = "MacBook Pro"
        status.activeDeviceID = controlling ? "pc" : "mac"
        status.capturing = !missing
        status.previewSharing = true
        status.hotkeys = hotkeys
        status.switchModifier = switchModifier
        status.relativeAuto = true
        status.relativeSupported = true
        var pc = DeviceStatus()
        pc.id = "pc"; pc.name = "Windows Desktop"; pc.os = "windows"; pc.paired = true; pc.state = .connected
        pc.swapGuiAndControl = true
        pc.rttMs = 3
        pc.reportedName = pc.name
        pc.jumpSlot = 3
        pc.speedPercent = speed
        if renamed {
            // Named on this Mac, with the clipboard turned off with it.
            pc.name = "Studio PC"
            pc.clipboardOff = true
        }
        var mini = DeviceStatus()
        mini.id = "mini"; mini.name = "Mac mini"; mini.os = "macos"; mini.paired = true; mini.state = .offline
        mini.reportedName = mini.name
        mini.jumpSlot = 2
        mini.speedPercent = 100
        status.devices = [pc, mini]
        var accessibility = Inputweave_App_V1_PermissionStatus()
        accessibility.permission = .accessibility
        accessibility.granted = !missing
        var monitoring = Inputweave_App_V1_PermissionStatus()
        monitoring.permission = .inputMonitoring
        monitoring.granted = true
        status.permissions = [accessibility, monitoring]

        var arrangement = Arrangement()
        arrangement.version = 4
        var mac = ArrangedDevice()
        mac.id = "mac"; mac.name = "MacBook Pro"; mac.os = "macos"; mac.placed = true; mac.thisDevice = true; mac.connected = true
        mac.x = 0; mac.y = 200; mac.jumpSlot = 1
        mac.screens = [screen("builtin", 1512, 982)]
        var pcArranged = ArrangedDevice()
        pcArranged.id = "pc"; pcArranged.name = pc.name; pcArranged.os = "windows"; pcArranged.placed = true; pcArranged.connected = true
        pcArranged.x = 1512; pcArranged.y = 0; pcArranged.jumpSlot = 3
        pcArranged.screens = [screen("main", 2560, 1440, scale: 1), screen("side", 1080, 1920, x: 2560, y: -300, primary: false, scale: 1)]
        var miniArranged = ArrangedDevice()
        miniArranged.id = "mini"; miniArranged.name = "Mac mini"; miniArranged.os = "macos"; miniArranged.placed = true
        miniArranged.x = 1512; miniArranged.y = -1080; miniArranged.jumpSlot = 2
        miniArranged.screens = [screen("ext", 1920, 1080)]
        if pictures {
            mac.screens[0].previewJpeg = lightPicture
            // Offline, it shows the last picture it sent.
            miniArranged.screens[0].previewJpeg = darkPicture
        }
        arrangement.devices = [mac, pcArranged, miniArranged]
        return AgentModel.sample(
            status: status, arrangement: arrangement,
            transfers: folders ? folderTransfers() : transfers ? sampleTransfers() : Transfers())
    }

    func testPanel() throws {
        let service = BackgroundService()
        XCTAssertEqual(PanelText.subtitle(Self.sampleModel()), "Cursor on MacBook Pro")
        XCTAssertNotEqual(PanelText.subtitle(Self.sampleModel(controlling: true)), "Cursor on MacBook Pro")
        let panels = [
            ("panel", Self.sampleModel()), ("panel-controlling", Self.sampleModel(controlling: true)),
            ("panel-permission", Self.sampleModel(missing: true)), ("panel-locked", Self.sampleModel(locked: true)),
        ]
        for (name, model) in panels {
            try render(name, size: CGSize(width: 340, height: 330)) {
                MenuPanel().environment(model).environment(service)
            }
        }
        try render("panel-dark", size: CGSize(width: 340, height: 330), dark: true) {
            MenuPanel().environment(Self.sampleModel()).environment(service)
        }
    }

    func testTransfers() throws {
        let service = BackgroundService()
        try render("panel-transfers", size: CGSize(width: 340, height: 560)) {
            MenuPanel().environment(Self.sampleModel(transfers: true)).environment(service)
        }
        try render("panel-transfers-dark", size: CGSize(width: 340, height: 560), dark: true) {
            MenuPanel().environment(Self.sampleModel(transfers: true)).environment(service)
        }
        try render("files", size: CGSize(width: 700, height: 720)) {
            FilesPage().environment(Self.sampleModel(transfers: true)).environment(service)
        }
    }

    /// A folder going, and one that arrived with files missing, in the
    /// panel and on the File Transfers page, and its failures shown.
    func testFolderTransfers() throws {
        let service = BackgroundService()
        let model = Self.sampleModel(folders: true)
        // Nothing dismissed yet in these renders.
        AppActions.navigation.dismissedTransfers = []
        try render("panel-folders", size: CGSize(width: 340, height: 520)) {
            MenuPanel().environment(model).environment(service)
        }
        try render("panel-folders-dark", size: CGSize(width: 340, height: 520), dark: true) {
            MenuPanel().environment(model).environment(service)
        }
        try render("files-folders", size: CGSize(width: 700, height: 640)) {
            FilesPage().environment(model).environment(service)
        }
        let partly = try XCTUnwrap(model.transfers.transfers.first { $0.transferID == "f2" })
        try render("folder-failures", size: CGSize(width: 340, height: 200)) {
            TransferRow(transfer: partly, dismissible: true, failuresShown: 3, showsFailures: true)
                .padding(12)
                .frame(maxHeight: .infinity, alignment: .top)
                .environment(model)
        }
    }

    func testArrangementPage() throws {
        try render("arrangement", size: CGSize(width: 900, height: 560)) {
            ArrangementPage().environment(Self.sampleModel()).environment(BackgroundService())
        }
        // The inspector shows this computer until another is selected.
        try render("arrangement-selected", size: CGSize(width: 900, height: 560)) {
            ArrangementPage(selection: "pc").environment(Self.sampleModel()).environment(BackgroundService())
        }
        try render("arrangement-dark", size: CGSize(width: 900, height: 560), dark: true) {
            ArrangementPage().environment(Self.sampleModel()).environment(BackgroundService())
        }
    }

    /// Another computer named on this Mac, with the clipboard turned off
    /// with it: the inspector's name field, the name the computer gives
    /// itself beneath it, and the two switches.
    func testRenamedDevice() throws {
        let service = BackgroundService()
        try render("arrangement-renamed", size: CGSize(width: 900, height: 640)) {
            ArrangementPage(selection: "pc").environment(Self.sampleModel(renamed: true)).environment(service)
        }
        try render("arrangement-renamed-dark", size: CGSize(width: 900, height: 640), dark: true) {
            ArrangementPage(selection: "pc").environment(Self.sampleModel(renamed: true)).environment(service)
        }
    }

    /// Another computer the cursor moves faster on: the cursor speed row
    /// with Reset, between the screens and the sharing switches.
    func testCursorSpeed() throws {
        let service = BackgroundService()
        try render("arrangement-speed", size: CGSize(width: 900, height: 760)) {
            ArrangementPage(selection: "pc").environment(Self.sampleModel(speed: 150)).environment(service)
        }
        try render("arrangement-speed-dark", size: CGSize(width: 900, height: 760), dark: true) {
            ArrangementPage(selection: "pc").environment(Self.sampleModel(speed: 150)).environment(service)
        }
    }

    /// What one screen shows while the screens are identified, over a
    /// desktop picture.
    func testScreenIdentifier() throws {
        let label = ScreenIdentity.Label(number: 1, width: 3024, height: 1964)
        for (name, dark, picture) in [
            ("screen-identifier", false, Self.lightPicture), ("screen-identifier-dark", true, Self.darkPicture),
        ] {
            let backdrop = try XCTUnwrap(NSImage(data: picture))
            try render(name, size: CGSize(width: 960, height: 600), dark: dark) {
                ZStack {
                    Image(nsImage: backdrop).resizable().scaledToFill()
                    ScreenIdentityView(name: "MacBook Pro", label: label, jump: "⌃⌥⇧1")
                }
                .clipped()
            }
        }
    }

    /// The edge lighting up where the cursor crossed, on each edge of a
    /// screen with a desktop picture, light and dark; and with Increase
    /// Contrast (a solid line).
    func testEdgeGlow() throws {
        let screen = CGRect(x: 0, y: 0, width: 480, height: 300)
        let edges: [(String, EdgeGlowGeometry.Edge, Double)] = [
            ("left", .left, 0.4), ("right", .right, 0.5), ("top", .top, 0.3), ("bottom", .bottom, 0.9),
        ]
        for (scheme, dark, picture) in [("", false, Self.lightPicture), ("-dark", true, Self.darkPicture)] {
            let backdrop = try XCTUnwrap(NSImage(data: picture))
            for (name, edge, fraction) in edges {
                for solid in [false, true] where !solid || name == "right" {
                    let thickness = solid ? EdgeGlowGeometry.solidDepth : EdgeGlowGeometry.depth
                    // AppKit's y goes up, SwiftUI's down.
                    let strip = EdgeGlowGeometry.strip(on: screen, edge: edge, thickness: thickness)
                    let file = "edge-glow-\(name)\(solid ? "-contrast" : "")\(scheme)"
                    try render(file, size: screen.size, dark: dark) {
                        ZStack(alignment: .topLeading) {
                            Image(nsImage: backdrop).resizable().scaledToFill()
                                .frame(width: screen.width, height: screen.height)
                            EdgeGlowView(edge: edge, solid: solid, spot: fraction)
                                .frame(width: strip.width, height: strip.height)
                                .position(x: strip.midX, y: screen.height - strip.midY)
                        }
                        .clipped()
                    }
                }
            }
        }
    }

    /// With desktop pictures behind the screens: light on this Mac, dark on
    /// the offline Mac mini, none from the Windows computer.
    func testDesktopPictures() throws {
        XCTAssertNotNil(DesktopPicture.decode(Self.lightPicture))
        XCTAssertNotNil(DesktopPicture.decode(Self.darkPicture))
        let service = BackgroundService()
        for (name, dark) in [("panel-pictures", false), ("panel-pictures-dark", true)] {
            try render(name, size: CGSize(width: 340, height: 330), dark: dark) {
                MenuPanel().environment(Self.sampleModel(pictures: true)).environment(service)
            }
        }
        // A wider window than the plain renders, so the badge on this Mac has
        // room for its words.
        for (name, dark) in [("arrangement-pictures", false), ("arrangement-pictures-dark", true)] {
            try render(name, size: CGSize(width: 1100, height: 640), dark: dark) {
                ArrangementPage().environment(Self.sampleModel(pictures: true)).environment(service)
            }
        }
    }

    /// The cursor locator over a desktop picture: the arrow at its own size,
    /// growing, at its peak, halfway back, and as it holds still with
    /// Reduce Motion. Its tip is on the same point in each.
    func testCursorLocator() throws {
        let moving = LocatorGeometry.motion(reduceMotion: false)
        let still = LocatorGeometry.motion(reduceMotion: true)
        let scales: [CGFloat] = [
            1, LocatorGeometry.scale(moving, at: moving.grow / 3), moving.peak,
            LocatorGeometry.scale(moving, at: moving.grow + moving.hold + moving.shrink / 2), still.peak,
        ]
        // The system cursors have no image until there is an application.
        _ = NSApplication.shared
        let arrow = NSCursor.arrow
        XCTAssertGreaterThan(arrow.image.size.width, 0)
        let tile = CGSize(width: 160, height: 200)
        let tip = CGPoint(x: 40, y: 30)
        for (name, dark, picture) in [
            ("cursor-locator", false, Self.lightPicture), ("cursor-locator-dark", true, Self.darkPicture),
        ] {
            let backdrop = try XCTUnwrap(NSImage(data: picture))
            try render(name, size: CGSize(width: tile.width * CGFloat(scales.count), height: tile.height), dark: dark) {
                HStack(spacing: 0) {
                    ForEach(Array(scales.enumerated()), id: \.offset) { _, scale in
                        // In the tile's own coordinates with y going up, as
                        // AppKit's screen has them.
                        let frame = LocatorGeometry.frame(
                            cursor: CGPoint(x: tip.x, y: tile.height - tip.y), size: arrow.image.size,
                            hotSpot: arrow.hotSpot, scale: scale)
                        ZStack(alignment: .topLeading) {
                            Image(nsImage: backdrop).resizable().scaledToFill()
                                .frame(width: tile.width, height: tile.height)
                            Image(nsImage: arrow.image).resizable()
                                .frame(width: frame.width, height: frame.height)
                                .offset(x: frame.minX, y: tile.height - frame.maxY)
                        }
                        .frame(width: tile.width, height: tile.height, alignment: .topLeading)
                        .clipped()
                    }
                }
            }
        }
    }

    func testPairing() throws {
        var showing = PairingState()
        showing.phase = .showCode
        showing.peerName = "MacBook Pro"
        showing.code = "382914"
        showing.expiresAtUnixMs = UInt64((Date().timeIntervalSince1970 + 118) * 1000)
        let responder = AgentModel.sample(status: AgentStatus(), pairing: showing)
        try render("pairing-code", size: CGSize(width: 420, height: 240)) {
            PairingCodePanelPreview().environment(responder)
        }

        var entering = PairingState()
        entering.phase = .enterCode
        entering.peerName = "Windows Desktop"
        entering.attemptsLeft = 2
        entering.error = "wrong_code"
        var nearby = NearbyDevice()
        nearby.id = "pc"; nearby.name = "Windows Desktop"; nearby.os = "windows"; nearby.pairable = true
        let initiator = AgentModel.sample(status: AgentStatus(), pairing: entering, nearby: [nearby])
        try render("add-device", size: CGSize(width: 480, height: 420)) {
            AddDeviceSheet().environment(initiator)
        }
    }

    func testOnboardingWelcome() throws {
        try render("onboarding", size: CGSize(width: 620, height: 520)) {
            OnboardingView().environment(Self.sampleModel(missing: true)).environment(BackgroundService())
                .environment(NotificationPermission(fixed: .notAsked))
        }
    }

    /// The steps with the most text. The permission lists are named
    /// differently from macOS 27 on.
    func testOnboardingSteps() throws {
        let service = BackgroundService()
        let notAsked = NotificationPermission(fixed: .notAsked)
        try render("onboarding-permissions", size: CGSize(width: 620, height: 520)) {
            OnboardingView(step: .permissions, macOSMajor: 27).environment(Self.sampleModel(missing: true)).environment(service)
                .environment(notAsked)
        }
        try render("onboarding-permissions-15", size: CGSize(width: 620, height: 520)) {
            OnboardingView(step: .permissions, macOSMajor: 15).environment(Self.sampleModel(missing: true)).environment(service)
                .environment(notAsked)
        }
        // Before Allow Access, and after: a folder not allowed.
        try render("onboarding-folders", size: CGSize(width: 620, height: 520)) {
            OnboardingView(step: .folders).environment(Self.folderModel(.unknown, .unknown, .unknown, asked: false))
                .environment(service).environment(notAsked)
        }
        try render("onboarding-folders-answered", size: CGSize(width: 620, height: 520)) {
            OnboardingView(step: .folders).environment(Self.folderModel(.granted, .granted, .denied, asked: true))
                .environment(service).environment(NotificationPermission(fixed: .allowed))
        }
        try render("onboarding-connect", size: CGSize(width: 620, height: 520)) {
            OnboardingView(step: .connect).environment(AgentModel.sample(status: AgentStatus())).environment(service)
                .environment(notAsked)
        }
    }

    /// The sample model, its background service having asked macOS for
    /// Downloads, Desktop and Documents with these results.
    static func folderModel(
        _ downloads: Inputweave_App_V1_FolderAccessState, _ desktop: Inputweave_App_V1_FolderAccessState,
        _ documents: Inputweave_App_V1_FolderAccessState, asked: Bool
    ) -> AgentModel {
        var status = sampleModel().status
        status.folderAccess = zip([Inputweave_App_V1_UserFolder.downloads, .desktop, .documents], [downloads, desktop, documents])
            .map { folder, state in
                var access = Inputweave_App_V1_FolderAccess()
                access.folder = folder
                access.state = state
                return access
            }
        status.folderAccessAsked = asked
        return AgentModel.sample(status: status)
    }

    /// The repository, to show files the app bundles.
    static let repository = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()

    func testAbout() throws {
        let icon = NSImage(contentsOf: Self.repository.appending(path: "docs/brand/app-icon-1024.png"))
        let trial = Self.licenseController(Self.sampleModel())
        let licensed = Self.licenseController(Self.sampleModel(license: Self.sampleLicense(.licensed)))
        for (name, dark, license) in [
            ("about", false, trial), ("about-dark", true, trial),
            ("about-licensed", false, licensed), ("about-licensed-dark", true, licensed),
        ] {
            try render(name, size: CGSize(width: 360, height: 320), dark: dark) {
                // The window's own background, which the view does not draw.
                AboutView(icon: icon, version: "1.0.0", build: "10000", copyright: "Copyright © 2026 InputWeave contributors.")
                    .environment(license)
                    .background(.background)
            }
        }
        let notices = try String(contentsOf: Self.repository.appending(path: "THIRD_PARTY_NOTICES.md"), encoding: .utf8)
        try render("acknowledgments", size: CGSize(width: 640, height: 560)) {
            AcknowledgmentsView(text: notices)
        }
    }

    /// A license record in a given state; licensed on this Mac and two
    /// paired computers, and when `synced`, taken from the PC.
    static func sampleLicense(_ state: Inputweave_App_V1_LicenseState, synced: Bool = false) -> LicenseRecord {
        var license = LicenseRecord()
        license.state = state
        guard state != .none, state != .unavailable else { return license }
        license.key = "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42"
        guard state == .licensed else { return license }
        license.activationID = "6b1e3f9a-2c4d-4e8f-9a1b-3c5d7e9f1a2b"
        license.customerEmail = "tasha@example.com"
        license.validatedAt = UInt64(Date().timeIntervalSince1970)
        var pc = Inputweave_App_V1_LicensedDevice()
        pc.id = "pc"; pc.name = "Windows Desktop"; pc.licensed = true
        var mini = Inputweave_App_V1_LicensedDevice()
        mini.id = "mini"; mini.name = "Mac mini"; mini.licensed = true
        license.devices = [pc, mini]
        if synced {
            license.sourceDeviceID = "pc"
            license.sourceDeviceName = "Windows Desktop"
        }
        return license
    }

    /// Never reaches Polar: the snapshots show fixed states.
    struct NoPolar: PolarTransport {
        func send(_: URLRequest) async throws -> PolarAnswer { throw URLError(.notConnectedToInternet) }
    }

    /// The app's own Info.plist, for the license's links and Polar.
    static let appInfo: [String: Any] = {
        let url = repository.appending(path: "apps/macos/InputWeave/Info.plist")
        let data = (try? Data(contentsOf: url)) ?? Data()
        return (try? PropertyListSerialization.propertyList(from: data, format: nil)) as? [String: Any] ?? [:]
    }()

    /// A controller holding still: the snapshots show fixed states, never
    /// what its lanes would make of an offline service.
    static func licenseController(_ model: AgentModel) -> LicenseController {
        let controller = LicenseController(
            agent: model,
            polar: PolarClient(config: .fromInfo(appInfo, version: "1.0.0") ?? .unconfigured, transport: NoPolar()),
            links: .fromInfo(appInfo), store: MemoryLicenseOutboxStore())
        controller.preview()
        return controller
    }

    func testLicense() throws {
        func page(_ name: String, _ license: LicenseRecord, height: CGFloat = 660,
                  _ configure: (LicenseController) -> Void = { _ in }) throws {
            let model = Self.sampleModel(license: license)
            let controller = Self.licenseController(model)
            configure(controller)
            for dark in [false, true] {
                try render(name + (dark ? "-dark" : ""), size: CGSize(width: 730, height: height), dark: dark) {
                    LicensePage().environment(model).environment(controller)
                }
            }
        }
        try page("license-trial", LicenseRecord())
        try page("license-seats-full", Self.sampleLicense(.seatsFull))
        try page("license-invalid", Self.sampleLicense(.invalid))
        try page("license-unavailable", Self.sampleLicense(.unavailable))
        // Removing: the first try, a removal the service missed, and a seat
        // Polar has not freed yet.
        func removal(_ ops: [LicenseOutbox.Op]) -> (LicenseController) -> Void {
            { $0.preview(outbox: LicenseOutbox(ops: ops)) }
        }
        let key = "INPUTWEAVE-7F3A2C1E-58D4-4B9A-A2E1-0C6D3F9B7E42"
        try page("license-removing", LicenseRecord(), removal([.init(kind: .remove, key: key)]))
        try page("license-removal-pending", LicenseRecord(), removal([.init(kind: .remove, key: key, tries: 1)]))
        try page("license-removed-offline", LicenseRecord(),
                 removal([.init(kind: .free, key: key, activationID: "a1", fromRemove: true, tries: 1)]))
        try page("license-licensed", Self.sampleLicense(.licensed), height: 520)
        try page("license-licensed-synced", Self.sampleLicense(.licensed, synced: true), height: 520)
        try page("license-removal-refused", Self.sampleLicense(.licensed), height: 580) { $0.preview(removalRefused: true) }

        let sheets: [(String, String, Bool, LicenseController.KeyError?)] = [
            ("enter", "", false, nil), ("activating", key, true, nil),
            ("not-a-key", "INPUTWEAVE-OOPS", false, .notAKey), ("seats-full", key, false, .seatsFull),
            ("no-longer-valid", key, false, .noLongerValid), ("unreachable", key, false, .polarUnreachable),
            ("failed", key, false, .failed), ("unavailable", key, false, .unavailable),
            ("service-away", key, false, .serviceNotResponding),
        ]
        for (name, text, activating, error) in sheets {
            let controller = Self.licenseController(Self.sampleModel())
            controller.preview(entering: true, activating: activating, keyError: error)
            for dark in [false, true] {
                try render("license-key-\(name)" + (dark ? "-dark" : ""), size: CGSize(width: 440, height: 210), dark: dark) {
                    EnterLicenseKeySheet(key: text).environment(controller)
                }
            }
        }

        for dark in [false, true] {
            try render("license-labels" + (dark ? "-dark" : ""), size: CGSize(width: 520, height: 120), dark: dark) {
                HStack(spacing: 24) {
                    WovenLabel(size: .small)
                    WovenLabel(size: .medium)
                    WovenLabel(size: .large)
                    WovenLabel(size: .large, increasedContrast: true)
                    TrialCapsule()
                }
                .padding(20)
                .frame(maxWidth: .infinity, maxHeight: .infinity)
                .background(Color.canvas)
            }
        }

        // The sidebar's License row, on the License page and elsewhere.
        let previous = AppActions.navigation.page
        defer { AppActions.navigation.page = previous }
        for (name, license, page) in [
            ("main-license-trial", LicenseRecord(), Page.license),
            ("main-general-licensed", Self.sampleLicense(.licensed), Page.general),
        ] {
            AppActions.navigation.page = page
            let model = Self.sampleModel(license: license)
            try render(name, size: CGSize(width: 960, height: 640)) {
                MainWindow().environment(model).environment(BackgroundService()).environment(Self.licenseController(model))
            }
        }
    }

    func testSettingsPages() throws {
        try render("troubleshooting", size: CGSize(width: 730, height: 900)) {
            TroubleshootingPage().environment(Self.sampleModel(missing: true)).environment(BackgroundService())
        }
        try render("troubleshooting-folders", size: CGSize(width: 730, height: 1100)) {
            TroubleshootingPage().environment(Self.folderModel(.granted, .denied, .missing, asked: true))
                .environment(BackgroundService())
        }
        // Without an update feed, as in these tests, General has no Updates
        // section; with one, it does.
        try render("general", size: CGSize(width: 730, height: 1360)) {
            GeneralPage(updater: nil).environment(Self.sampleModel()).environment(BackgroundService())
        }
        // Shortcuts changed, the jumps off, and switching only with ⇧ held.
        var custom = ShortcutText.defaults
        custom.takeBack = "control+gui+Escape"
        custom.lock = "control+alt+KeyK"
        custom.jumpModifiers = ""
        custom.relative = ""
        try render("general-shortcuts", size: CGSize(width: 730, height: 1360)) {
            GeneralPage(updater: nil)
                .environment(Self.sampleModel(hotkeys: custom, switchModifier: .shift))
                .environment(BackgroundService())
        }
        let updater = Updater(load: { true }, store: { _ in }, check: {})
        try render("general-updates", size: CGSize(width: 730, height: 1500)) {
            GeneralPage(updater: updater, version: "0.1.0").environment(Self.sampleModel()).environment(BackgroundService())
        }
        try render("devices", size: CGSize(width: 730, height: 520)) {
            DevicesPage().environment(Self.sampleModel()).environment(BackgroundService())
        }
    }

    /// The menu bar icon in every state, tinted as the system would on a
    /// light and a dark menu bar, at 1x and 2x (enlarged without smoothing,
    /// to judge the pixels) and at 6x.
    func testStatusIcon() throws {
        let states: [StatusIcon.State] = [.normal, .controlling, .controlled, .paused, .attention]
        for (scale, zoom) in [(CGFloat(1), 8), (2, 4), (6, 1)] {
            let cell = CGSize(width: 22 * scale, height: 16 * scale)
            let pad = 6 * scale
            let width = Int((cell.width + pad) * CGFloat(states.count) + pad)
            let height = Int((cell.height + pad) * 2 + pad)
            let sheet = try XCTUnwrap(NSBitmapImageRep(
                bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height, bitsPerSample: 8, samplesPerPixel: 4,
                hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0))
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: sheet)
            for (row, (background, ink)) in [(NSColor(white: 0.93, alpha: 1), NSColor.black), (NSColor(white: 0.16, alpha: 1), NSColor.white)].enumerated() {
                let y = pad + CGFloat(row) * (cell.height + pad)
                background.setFill()
                NSRect(x: 0, y: y - pad / 2, width: CGFloat(width), height: cell.height + pad).fill()
                for (column, state) in states.enumerated() {
                    let image = StatusIcon.image(state)
                    let tinted = NSImage(size: image.size, flipped: false) { rect in
                        image.draw(in: rect)
                        ink.set()
                        rect.fill(using: .sourceAtop)
                        return true
                    }
                    let x = pad + CGFloat(column) * (cell.width + pad)
                    tinted.draw(
                        in: NSRect(x: x, y: y, width: cell.width, height: cell.height), from: .zero,
                        operation: .sourceOver, fraction: 1, respectFlipped: true,
                        hints: [.interpolation: NSImageInterpolation.high])
                }
            }
            NSGraphicsContext.restoreGraphicsState()
            let enlarged = try XCTUnwrap(NSBitmapImageRep(
                bitmapDataPlanes: nil, pixelsWide: width * zoom, pixelsHigh: height * zoom, bitsPerSample: 8,
                samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0,
                bitsPerPixel: 0))
            NSGraphicsContext.saveGraphicsState()
            NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: enlarged)
            sheet.draw(
                in: NSRect(x: 0, y: 0, width: enlarged.pixelsWide, height: enlarged.pixelsHigh), from: .zero,
                operation: .copy, fraction: 1, respectFlipped: false, hints: [.interpolation: NSImageInterpolation.none])
            NSGraphicsContext.restoreGraphicsState()
            let png = try XCTUnwrap(enlarged.representation(using: .png, properties: [:]))
            try png.write(to: Self.output.appending(path: "status-icon-\(Int(scale))x.png"))
        }
    }
}
