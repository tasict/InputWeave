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

    /// An alert as it shows on a window: its content, laid out, on the
    /// window's background (the alert's own is see-through).
    func renderAlert(_ name: String, dark: Bool = false, _ alert: NSAlert) throws {
        let appearance = try XCTUnwrap(NSAppearance(named: dark ? .darkAqua : .aqua))
        alert.window.appearance = appearance
        alert.layout()
        let view = try XCTUnwrap(alert.window.contentView)
        view.layoutSubtreeIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.3))
        let size = view.bounds.size
        let content = try XCTUnwrap(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        appearance.performAsCurrentDrawingAppearance {
            view.cacheDisplay(in: view.bounds, to: content)
        }
        let rep = try XCTUnwrap(NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: content.pixelsWide, pixelsHigh: content.pixelsHigh, bitsPerSample: 8,
            samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0))
        rep.size = size
        var background = NSColor.white
        appearance.performAsCurrentDrawingAppearance {
            background = NSColor.windowBackgroundColor.usingColorSpace(.sRGB) ?? .white
        }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        background.setFill()
        NSRect(origin: .zero, size: size).fill()
        content.draw(
            in: NSRect(origin: .zero, size: size), from: .zero, operation: .sourceOver, fraction: 1,
            respectFlipped: false, hints: [.interpolation: NSImageInterpolation.high])
        NSGraphicsContext.restoreGraphicsState()
        let png = try XCTUnwrap(rep.representation(using: .png, properties: [:]))
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
        license: LicenseRecord = LicenseRecord(), wake: MiniWake? = nil, offlineMinutes: Double = 125,
        wakeOnPush: Bool = false
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
        if let wake {
            // A service that wakes computers, and how the Mac mini's waking
            // goes; it went offline `offlineMinutes` ago.
            status.wakeSupported = true
            status.wakeOnPush = wakeOnPush
            mini.canWake = wake != .cannot
            mini.wake = switch wake {
            case .waking: .waking
            case .notWoken: .notWoken
            case .asleep, .cannot: WakeState.none
            }
            mini.offlineSinceUnixMs = UInt64((Date.now.timeIntervalSince1970 - offlineMinutes * 60) * 1000)
        }
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

    /// How the Mac mini, offline, stands for waking.
    enum MiniWake {
        /// It can be woken.
        case asleep
        case waking
        /// It was woken and didn't wake up.
        case notWoken
        /// It never said how to wake it.
        case cannot
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

    /// The sample with a computer taken offline for now: the Windows
    /// computer, by this Mac, with it and the Mac mini, connected, still on
    /// InputWeave 1.0; or (`here`) this Mac, by the Windows computer, every
    /// other computer then offline.
    static func offlineModel(here: Bool = false) -> AgentModel {
        let sample = sampleModel(pictures: true)
        var status = sample.status
        var arrangement = sample.arrangement
        status.offlineSupported = true
        // As the service lists them: offline, nothing measured.
        status.devices[0].state = .offline
        status.devices[0].rttMs = 0
        if here {
            status.offlineForNow = true
            status.offlineByDeviceID = "pc"
            status.offlineByName = status.devices[0].name
        } else {
            status.devices[0].offlineForNow = true
            status.devices[0].offlineByDeviceID = "mac"
            status.devices[0].offlineByName = "MacBook Pro"
            status.devices[0].offlineUnaware = true
            status.devices[1].state = .connected
            status.devices[1].rttMs = 4
            status.devices[1].offlineUnaware = true
        }
        for index in arrangement.devices.indices where !arrangement.devices[index].thisDevice {
            let id = arrangement.devices[index].id
            arrangement.devices[index].connected = status.devices.first { $0.id == id }?.state == .connected
        }
        return AgentModel.sample(status: status, arrangement: arrangement)
    }

    /// The Windows computer taken offline for now here, which like the Mac
    /// mini still runs InputWeave 1.0: its inspector, with who did it, the
    /// warning about the Mac mini and Bring Back Online; the Mac mini's,
    /// with Take Offline; the slashed network after its name on the canvas,
    /// the mini map and the devices list. And this Mac taken offline for
    /// now: its panel, and another computer's inspector, plainly offline.
    func testOfflineForNow() throws {
        let service = BackgroundService()
        for dark in [false, true] {
            let suffix = dark ? "-dark" : ""
            try render("arrangement-offline\(suffix)", size: CGSize(width: 900, height: 1000), dark: dark) {
                ArrangementPage(selection: "pc").environment(Self.offlineModel()).environment(service)
            }
            try render("arrangement-offline-mini\(suffix)", size: CGSize(width: 900, height: 900), dark: dark) {
                ArrangementPage(selection: "mini").environment(Self.offlineModel()).environment(service)
            }
            try render("devices-offline\(suffix)", size: CGSize(width: 730, height: 1000), dark: dark) {
                DevicesPage().environment(Self.offlineModel()).environment(service)
            }
            try render("panel-offline\(suffix)", size: CGSize(width: 340, height: 330), dark: dark) {
                MenuPanel().environment(Self.offlineModel()).environment(service)
            }
            try render("panel-offline-here\(suffix)", size: CGSize(width: 340, height: 400), dark: dark) {
                MenuPanel().environment(Self.offlineModel(here: true)).environment(service)
            }
            try render("arrangement-offline-here\(suffix)", size: CGSize(width: 900, height: 900), dark: dark) {
                ArrangementPage(selection: "mini").environment(Self.offlineModel(here: true)).environment(service)
            }
        }
    }

    /// The sample with profiles, as on the Profiles page's prototype:
    /// Default (a wired network at home), Office (the Wi-Fi network this
    /// Mac is on, without the Mac mini, files off) and Other networks, the
    /// profile in use picked by Automatic. `inUse` other networks, on a
    /// network in no profile, paused (`otherPaused`) or not; `chosen` by
    /// the user; `wifiNames` names the network; `added`, a new profile;
    /// `resolved` false, just after joining the network.
    static func profilesModel(
        inUse: String = "office", otherPaused: Bool = false, chosen: Bool = false, wifiNames: Bool = true,
        added: Bool = false, resolved: Bool = true
    ) -> AgentModel {
        let sample = sampleModel(pictures: true)
        var status = sample.status
        var arrangement = sample.arrangement
        func network(_ id: String, name: String = "", kind: Inputweave_App_V1_NetworkKind, router: String) -> AgentNetwork {
            var network = AgentNetwork()
            network.id = id; network.name = name; network.kind = kind; network.router = router
            return network
        }
        func profile(
            _ id: String, _ kind: Inputweave_App_V1_ProfileKind, name: String = "", paused: Bool = false,
            off: [String] = [], files: Bool = true, networks: [AgentNetwork] = []
        ) -> AgentProfile {
            var profile = AgentProfile()
            profile.id = id; profile.kind = kind; profile.name = name; profile.paused = paused
            profile.offDeviceIds = off; profile.clipboard = true; profile.files = files; profile.networks = networks
            return profile
        }
        let office = network("n-office", name: wifiNames ? "Office-5G" : "", kind: .wifi, router: "192.168.1.1")
        status.profilesSupported = true
        status.offlineSupported = true
        status.wifiNames = wifiNames
        status.profiles = [
            profile("default", .default, networks: [network("n-home", kind: .wired, router: "172.16.0.1")]),
            profile("office", .custom, name: "Office", off: ["mini"], files: false, networks: [office]),
        ]
        if added {
            status.profiles.append(profile("new", .custom, name: ProfileText.newName()))
        }
        status.profiles.append(profile("other", .otherNetworks, paused: otherPaused))
        status.profileID = inUse
        status.profileChosen = chosen
        status.network = inUse == "other" ? network("n-cafe", kind: .wifi, router: "198.51.100.1") : office
        status.network.resolved = resolved
        if inUse == "office" {
            // Office leaves the Mac mini out.
            status.devices[1].notInProfile = true
        }
        if ProfileText.paused(status) {
            // As the service lists them: offline, nothing measured, none
            // the keyboard and mouse are on.
            status.activeDeviceID = "mac"
            for index in status.devices.indices {
                status.devices[index].state = .offline
                status.devices[index].rttMs = 0
            }
        }
        for index in arrangement.devices.indices where !arrangement.devices[index].thisDevice {
            let id = arrangement.devices[index].id
            arrangement.devices[index].connected = status.devices.first { $0.id == id }?.state == .connected
        }
        return AgentModel.sample(status: status, arrangement: arrangement)
    }

    /// Renders in Chinese as well: then every string the views look up in
    /// the main bundle comes from this bundle's zh-Hant translations, as
    /// the test runner's own bundle has none.
    func renderBoth(_ name: String, size: CGSize, dark: Bool = false, @ViewBuilder _ view: () -> some View) throws {
        try render(name, size: size, dark: dark, view)
        _ = ChineseBundle.installed
        object_setClass(Bundle.main, ChineseBundle.self)
        defer { object_setClass(Bundle.main, Bundle.self) }
        try render("\(name)-zh", size: size, dark: dark) {
            view().environment(\.locale, Locale(identifier: "zh-Hant"))
        }
    }

    /// The Profiles page in the prototype's states: Automatic in Office,
    /// with Default selected; editing Office; Other networks paused, on a
    /// network in no profile; a profile just added; and networks known by
    /// their router alone, before the Wi-Fi name is read. Then the panel
    /// with the profile in use after its title, chosen or not, and while
    /// Automatic picked a paused Other networks; the Mac mini, left out of
    /// Office, on the Devices page and the arrangement.
    func testProfiles() throws {
        let service = BackgroundService()
        let size = CGSize(width: 760, height: 980)
        for dark in [false, true] {
            let suffix = dark ? "-dark" : ""
            try renderBoth("profiles\(suffix)", size: size, dark: dark) {
                ProfilesPage(selection: "default", wifiNames: nil).environment(Self.profilesModel())
            }
        }
        try renderBoth("profiles-office", size: size) {
            ProfilesPage(selection: "office", wifiNames: nil).environment(Self.profilesModel())
        }
        try renderBoth("profiles-other-paused", size: size) {
            ProfilesPage(selection: "other", wifiNames: nil)
                .environment(Self.profilesModel(inUse: "other", otherPaused: true))
        }
        try renderBoth("profiles-added", size: size) {
            ProfilesPage(selection: "new", wifiNames: nil).environment(Self.profilesModel(added: true))
        }
        // Renamed to a name another profile has: said under the field,
        // which shows the profile's name again.
        try renderBoth("profiles-name-taken", size: size) {
            ProfilesPage(
                selection: "office", wifiNames: nil,
                nameMessage: ProfileText.renameRefused("name_taken", typing: false).message
            )
            .environment(Self.profilesModel())
        }
        // Just after joining a network the service knows by its router
        // alone: adding it is off for now.
        try render("profiles-joining", size: size) {
            ProfilesPage(selection: "default", wifiNames: nil).environment(Self.profilesModel(resolved: false))
        }
        try render("panel-profile-paused-joining", size: CGSize(width: 340, height: 360)) {
            MenuPanel().environment(Self.profilesModel(inUse: "other", otherPaused: true, resolved: false))
                .environment(service)
        }
        // As narrow as the main window lets the page be.
        try renderBoth("profiles-narrow", size: CGSize(width: 550, height: 1040)) {
            ProfilesPage(selection: "office", wifiNames: nil).environment(Self.profilesModel())
        }
        try renderBoth("profiles-routers", size: size) {
            ProfilesPage(selection: "office", wifiNames: nil)
                .environment(Self.profilesModel(chosen: true, wifiNames: false))
        }
        for dark in [false, true] {
            let suffix = dark ? "-dark" : ""
            try renderBoth("panel-profile\(suffix)", size: CGSize(width: 340, height: 330), dark: dark) {
                MenuPanel().environment(Self.profilesModel()).environment(service)
            }
            try renderBoth("panel-profile-paused\(suffix)", size: CGSize(width: 340, height: 360), dark: dark) {
                MenuPanel().environment(Self.profilesModel(inUse: "other", otherPaused: true)).environment(service)
            }
        }
        try renderBoth("panel-profile-chosen-paused", size: CGSize(width: 340, height: 330)) {
            MenuPanel().environment(Self.profilesModel(inUse: "other", otherPaused: true, chosen: true)).environment(service)
        }
        try renderBoth("devices-not-in-profile", size: CGSize(width: 730, height: 700)) {
            DevicesPage().environment(Self.profilesModel()).environment(service)
        }
        try renderBoth("arrangement-not-in-profile", size: CGSize(width: 900, height: 760)) {
            ArrangementPage(selection: "mini").environment(Self.profilesModel()).environment(service)
        }
        try renderBoth("arrangement-profile-paused", size: CGSize(width: 900, height: 760)) {
            ArrangementPage(selection: "pc").environment(Self.profilesModel(inUse: "other", otherPaused: true))
                .environment(service)
        }
    }

    /// The Mac mini offline and asleep: in the inspector, as it can be woken,
    /// is being woken, didn't wake up, and can't be woken yet; the moon after
    /// its name on the canvas, the mini map and the devices list.
    func testWaking() throws {
        let service = BackgroundService()
        // Offline 59 minutes, the longest of the English times.
        for (name, wake, minutes) in [
            ("asleep", MiniWake.asleep, 125.0), ("waking", .waking, 125), ("not-woken", .notWoken, 59.5),
            ("cannot", .cannot, 125),
        ] {
            try render("arrangement-wake-\(name)", size: CGSize(width: 900, height: 640)) {
                ArrangementPage(selection: "mini")
                    .environment(Self.sampleModel(wake: wake, offlineMinutes: minutes))
                    .environment(service)
            }
        }
        for (name, wake) in [("asleep", MiniWake.asleep), ("not-woken", .notWoken)] {
            try render("arrangement-wake-\(name)-dark", size: CGSize(width: 900, height: 640), dark: true) {
                ArrangementPage(selection: "mini").environment(Self.sampleModel(wake: wake)).environment(service)
            }
        }
        try render("devices-wake", size: CGSize(width: 730, height: 520)) {
            DevicesPage().environment(Self.sampleModel(wake: .asleep)).environment(service)
        }
        try render("devices-wake-dark", size: CGSize(width: 730, height: 520), dark: true) {
            DevicesPage().environment(Self.sampleModel(wake: .waking)).environment(service)
        }
        try render("panel-wake", size: CGSize(width: 340, height: 330)) {
            MenuPanel().environment(Self.sampleModel(wake: .asleep)).environment(service)
        }
        try render("panel-wake-dark", size: CGSize(width: 340, height: 330), dark: true) {
            MenuPanel().environment(Self.sampleModel(wake: .asleep)).environment(service)
        }
        // General › Switching with the switch for waking by pushing the
        // cursor, on.
        try render("general-wake", size: CGSize(width: 730, height: 1520)) {
            GeneralPage(updater: nil).environment(Self.sampleModel(wake: .asleep, wakeOnPush: true)).environment(service)
        }
        try render("general-wake-dark", size: CGSize(width: 730, height: 1520), dark: true) {
            GeneralPage(updater: nil).environment(Self.sampleModel(wake: .asleep, wakeOnPush: true)).environment(service)
        }
    }

    /// Finder's Services › Send with InputWeave: the panel asking where two
    /// items go, with the Mac mini offline and a computer files are turned
    /// off with listed but off; the panel when the only computer connected
    /// can't take them; the panel with a computer offline for now; and the
    /// notice when none is connected.
    func testSendChooser() throws {
        let model = Self.sampleModel()
        var status = model.status
        var studio = DeviceStatus()
        studio.id = "studio"; studio.name = "Mac Studio"; studio.os = "macos"; studio.paired = true
        studio.state = .connected; studio.rttMs = 5
        var media = DeviceStatus()
        media.id = "media"; media.name = "Living Room PC"; media.os = "windows"; media.paired = true
        media.state = .connected; media.rttMs = 7; media.filesOff = true
        status.devices += [studio, media]
        let sending = AgentModel.sample(status: status)
        let urls = [URL(fileURLWithPath: "/Users/me/Report.pdf"), URL(fileURLWithPath: "/Users/me/Photos", isDirectory: true)]
        let icon = NSImage(contentsOf: Self.repository.appending(path: "docs/brand/app-icon-1024.png"))
        // Connected, but none can take them: every computer off, and Send.
        status.devices = status.devices.map { device in
            var device = device
            if device.id != "media" { device.state = .offline; device.rttMs = 0 }
            return device
        }
        let noneCan = AgentModel.sample(status: status)
        try render("send-chooser-none-can", size: CGSize(width: 320, height: 300)) {
            SendChooserView(model: noneCan, items: SendItems(urls), icon: icon, cancel: {}, send: { _ in })
        }
        try render("send-chooser-offline", size: CGSize(width: 320, height: 300)) {
            SendChooserView(model: Self.offlineModel(), items: SendItems(urls), icon: icon, cancel: {}, send: { _ in })
        }
        for dark in [false, true] {
            try render("send-chooser\(dark ? "-dark" : "")", size: CGSize(width: 320, height: 300), dark: dark) {
                SendChooserView(model: sending, items: SendItems(urls), icon: icon, cancel: {}, send: { _ in })
            }
            try render("send-nowhere\(dark ? "-dark" : "")", size: CGSize(width: 300, height: 110), dark: dark) {
                SendNoticeView(message: String(localized: "No connected computer can receive files."), icon: icon) {}
            }
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

    /// On trial, with the Windows computer licensed, the Mac mini on
    /// InputWeave 1.0 licensed too (never named).
    static func trialWithLicensedPC() -> LicenseRecord {
        var license = LicenseRecord()
        license.applySupported = true
        var mini = Inputweave_App_V1_LicensedDevice()
        mini.id = "mini"; mini.name = "Mac mini"; mini.use = .otherLicense; mini.legacy = true
        var pc = Inputweave_App_V1_LicensedDevice()
        pc.id = "pc"; pc.name = "Windows Desktop"; pc.use = .otherLicense
        license.devices = [mini, pc]
        return license
    }

    /// A licensed Mac whose list shows every standing: the Windows computer
    /// licensed, the Mac mini on trial and offline, a Linux laptop
    /// activating, a studio PC waiting offline, an iMac that found the
    /// places full, a Linux desktop on another license, an old MacBook on
    /// 1.0, and a PC taken offline for now, on trial. With `full`, the five
    /// places are taken; without, only the first three computers are
    /// paired, the laptop activating a key entered there.
    static func licenseComputersModel(full: Bool) -> AgentModel {
        let sample = sampleModel(license: sampleLicense(.licensed))
        var status = sample.status
        status.offlineSupported = true
        status.license.applySupported = true
        func device(_ id: String, _ name: String, _ os: String, connected: Bool = true) -> DeviceStatus {
            var device = DeviceStatus()
            device.id = id; device.name = name; device.os = os; device.paired = true
            device.state = connected ? .connected : .offline
            device.rttMs = 3
            return device
        }
        func entry(
            _ id: String, _ name: String, _ use: LicenseUse, activation: String = "", legacy: Bool = false,
            canTakeBack: Bool = false
        ) -> LicensedDevice {
            var entry = LicensedDevice()
            entry.id = id; entry.name = name; entry.use = use; entry.licensed = use == .licensed
            entry.activationID = activation; entry.legacy = legacy; entry.canTakeBack = canTakeBack
            return entry
        }
        var lounge = device("lounge", "Living Room PC", "windows", connected: false)
        lounge.offlineForNow = true
        let devices = [
            status.devices[0], status.devices[1], device("laptop", "Linux Laptop", "linux"),
            device("studio", "Studio PC", "windows", connected: false), device("imac", "Office iMac", "macos"),
            device("desk", "Linux Desktop", "linux"), device("old", "Old MacBook", "macos"), lounge,
        ]
        let entries = [
            entry("pc", "Windows Desktop", .licensed, activation: "act-pc"),
            // Applied from here, so it can be taken back until it is
            // activated; with places left, a key its own user entered.
            entry("laptop", "Linux Laptop", .activating, canTakeBack: full),
            entry("studio", "Studio PC", .waiting, canTakeBack: true),
            entry("imac", "Office iMac", .noPlaces), entry("desk", "Linux Desktop", .otherLicense),
            entry("old", "Old MacBook", .licensed, legacy: true),
        ]
        // With places left, the Mac mini, on InputWeave 1.0, did not take the
        // license applied to it.
        var refused = entry("mini", "Mac mini", .none, legacy: true)
        refused.legacyRefused = true
        status.devices = full ? devices : Array(devices.prefix(3))
        status.license.devices = full ? entries : Array(entries.prefix(2)) + [refused]
        return AgentModel.sample(status: status, arrangement: sample.arrangement)
    }

    /// Remove Device's confirmation: with the checkbox that revokes the
    /// license, for a computer on InputWeave 1.0, and without the license;
    /// and the notices when the license could not be revoked; and the
    /// License page's two confirmations. Return and Escape keep things as
    /// they are; the destructive button only looks it.
    func testRemoveDeviceAlert() throws {
        @MainActor final class Clicks: NSObject {
            var titles: [String] = []
            @objc func click(_ sender: NSButton) { titles.append(sender.title) }
        }
        // Every confirmation of something that can't simply be undone:
        // Remove Device in its three forms, and the License page's Remove
        // License and Revoke License.
        let confirmations = [LicenseText.RemoveOffer.revoke, .legacy, .none].map {
            RemoveDeviceAlert.confirmation(name: "Windows Desktop", offer: $0).alert
        } + [LicenseAlerts.removal(), LicenseAlerts.revoke(name: "Windows Desktop")]
        for alert in confirmations {
            alert.layout()
            let (destructive, keep) = (alert.buttons[0], alert.buttons[1])
            XCTAssertTrue(destructive.hasDestructiveAction, alert.messageText)
            XCTAssertEqual(destructive.keyEquivalent, "", "Return never chooses it: \(alert.messageText)")
            XCTAssertEqual(keep.keyEquivalent, "\r", alert.messageText)
            let clicks = Clicks()
            for button in alert.buttons {
                button.target = clicks
                button.action = #selector(Clicks.click(_:))
            }
            func key(_ characters: String, _ code: UInt16, _ modifiers: NSEvent.ModifierFlags = []) throws -> NSEvent {
                try XCTUnwrap(NSEvent.keyEvent(
                    with: .keyDown, location: .zero, modifierFlags: modifiers, timestamp: 0,
                    windowNumber: alert.window.windowNumber, context: nil, characters: characters,
                    charactersIgnoringModifiers: characters, isARepeat: false, keyCode: code))
            }
            XCTAssertTrue(alert.window.performKeyEquivalent(with: try key("\r", 36)))
            XCTAssertEqual(clicks.titles, [keep.title], "Return chose \(keep.title)")
            // Escape is the alert's while it shows (`ConfirmationAlert`), as
            // the keeping button's key is Return: it takes Escape alone.
            XCTAssertTrue(ConfirmationAlert.isEscape(try key("\u{1b}", 53)))
            XCTAssertFalse(ConfirmationAlert.isEscape(try key("\r", 36)))
            XCTAssertFalse(ConfirmationAlert.isEscape(try key("\u{1b}", 53, [.option])))
        }
        XCTAssertEqual(confirmations[3].buttons.map(\.title), ["Remove License", "Cancel"])
        XCTAssertEqual(confirmations[4].buttons.map(\.title), ["Revoke License", "Keep"])
        XCTAssertEqual(confirmations[4].messageText, "Revoke the license on Windows Desktop?")
        let portal = URL(string: "https://polar.sh/portal")
        XCTAssertEqual(RemoveDeviceAlert.notRevoked(name: "PC", notice: .notTold, portal: portal).buttons.map(\.title), ["OK"],
                       "its place is free: nothing to manage")
        XCTAssertEqual(RemoveDeviceAlert.notRevoked(name: "PC", notice: .notDeactivated, portal: portal).buttons.map(\.title),
                       ["OK", "Manage Computers…"])
        for dark in [false, true] {
            let suffix = dark ? "-dark" : ""
            try renderAlert("remove-device-revoke\(suffix)", dark: dark,
                            RemoveDeviceAlert.confirmation(name: "Windows Desktop", offer: .revoke).alert)
            try renderAlert("remove-device-legacy\(suffix)", dark: dark,
                            RemoveDeviceAlert.confirmation(name: "Old MacBook", offer: .legacy).alert)
            try renderAlert("remove-device\(suffix)", dark: dark,
                            RemoveDeviceAlert.confirmation(name: "Windows Desktop", offer: .none).alert)
            try renderAlert("remove-device-not-revoked\(suffix)", dark: dark,
                            RemoveDeviceAlert.notRevoked(
                                name: "Windows Desktop", notice: .notDeactivated, portal: URL(string: "https://polar.sh/portal")))
            try renderAlert("remove-device-out-of-reach\(suffix)", dark: dark,
                            RemoveDeviceAlert.notRevoked(
                                name: "Studio PC", notice: .outOfReach, portal: URL(string: "https://polar.sh/portal")))
            try renderAlert("remove-device-not-told\(suffix)", dark: dark,
                            RemoveDeviceAlert.notRevoked(
                                name: "Windows Desktop", notice: .notTold, portal: URL(string: "https://polar.sh/portal")))
            try renderAlert("license-remove-confirm\(suffix)", dark: dark, LicenseAlerts.removal())
            try renderAlert("license-revoke-confirm\(suffix)", dark: dark, LicenseAlerts.revoke(name: "Windows Desktop"))
        }
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
        // On trial, with a paired computer to apply the license from; and
        // a key applied from another computer that found every place
        // taken, plainly on trial.
        try page("license-trial-licensed-elsewhere", Self.trialWithLicensedPC())
        var applied = Self.sampleLicense(.seatsFull)
        applied.sourceDeviceID = "pc"
        applied.sourceDeviceName = "Windows Desktop"
        try page("license-seats-full-applied", applied)
        // Every place was taken when the key was entered here, and the
        // license waits to be applied to the Windows computer and the Mac
        // mini, offline, once this computer is licensed again.
        var waiting = Self.sampleLicense(.seatsFull)
        waiting.applySupported = true
        waiting.devices = [("pc", "Windows Desktop"), ("mini", "Mac mini")].map { id, name in
            var entry = LicensedDevice()
            entry.id = id; entry.name = name; entry.use = .waiting; entry.canTakeBack = true
            entry.waitingToApply = true
            return entry
        }
        try page("license-waiting-to-apply", waiting, height: 840)
        // This computer removes its own license, which still waits to be
        // applied to the two.
        var removing = Self.sampleLicense(.licensed)
        removing.applySupported = true
        removing.devices = waiting.devices
        try page("license-removing-waiting", removing, height: 840, removal([.init(kind: .remove, key: key)]))
        // A license revoked here from another computer: plainly on trial,
        // while its removal runs unseen.
        var revoked = Self.sampleLicense(.licensed)
        revoked.state = .revoked
        try page("license-revoked", revoked, removal([
            .init(kind: .free, key: key, activationID: "a1", fromRemove: true, revoked: true, tries: 1),
            .init(kind: .remove, key: key, activationID: "a1", revoked: true, tries: 1),
        ]))

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

        // Computers on this license, in every standing, every place taken;
        // and with places left.
        for (name, full, height) in [("license-computers", true, CGFloat(900)), ("license-computers-room", false, 560)] {
            let model = Self.licenseComputersModel(full: full)
            let controller = Self.licenseController(model)
            for dark in [false, true] {
                try render(name + (dark ? "-dark" : ""), size: CGSize(width: 730, height: height), dark: dark) {
                    LicensePage().environment(model).environment(controller)
                }
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
        try render("general", size: CGSize(width: 730, height: 1440)) {
            GeneralPage(updater: nil).environment(Self.sampleModel()).environment(BackgroundService())
        }
        // Shortcuts changed, taking back on F13 alone or Control twice, the
        // jumps off, and switching only with ⇧ held.
        var custom = ShortcutText.defaults
        custom.takeBack = "F13"
        custom.takeBackDouble = ShortcutText.doubleControl
        custom.lock = "control+alt+KeyK"
        custom.jumpModifiers = ""
        custom.relative = ""
        try render("general-shortcuts", size: CGSize(width: 730, height: 1440)) {
            GeneralPage(updater: nil)
                .environment(Self.sampleModel(hotkeys: custom, switchModifier: .shift))
                .environment(BackgroundService())
        }
        let updater = Updater(load: { true }, store: { _ in }, check: {})
        try render("general-updates", size: CGSize(width: 730, height: 1580)) {
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

/// The main bundle while a snapshot renders in Chinese: every way Foundation
/// and SwiftUI look up a string goes to the snapshot bundle's zh-Hant
/// translations. The lookups Swift does not let a subclass override are
/// added to it at run time.
private final class ChineseBundle: Bundle, @unchecked Sendable {
    static let translations: Bundle = {
        let own = Bundle(for: Snapshots.self)
        return own.path(forResource: "zh-Hant", ofType: "lproj").flatMap(Bundle.init(path:)) ?? own
    }()

    private static func string(_ key: NSString?, _ value: NSString?, _ table: NSString?) -> NSString {
        translations.localizedString(forKey: (key ?? "") as String, value: value as String?, table: table as String?) as NSString
    }

    override func localizedString(forKey key: String, value: String?, table tableName: String?) -> String {
        Self.string(key as NSString, value as NSString?, tableName as NSString?) as String
    }

    override var preferredLocalizations: [String] { ["zh-Hant"] }

    /// Adds the other lookups, once.
    static let installed: Void = {
        typealias Three = @convention(block) (AnyObject, NSString?, NSString?, NSString?) -> AnyObject
        typealias Four = @convention(block) (AnyObject, NSString?, NSString?, NSString?, AnyObject?) -> AnyObject
        typealias Six = @convention(block) (AnyObject, NSString?, NSString?, NSString?, AnyObject?, AnyObject?, ObjCBool) -> AnyObject
        let plain: [(String, Any)] = [
            ("localizedStringForKey:value:table:localization:", { _, k, v, t, _ in string(k, v, t) } as Four),
            ("localizedStringForKey:value:table:localizations:", { _, k, v, t, _ in string(k, v, t) } as Four),
            ("_localizedStringForKey:value:table:localizations:", { _, k, v, t, _ in string(k, v, t) } as Four),
            (
                "_localizedStringForKey:value:table:bundleLocalizations:localization:preventMarkdownParsing:",
                { _, k, v, t, _, _, _ in string(k, v, t) } as Six
            ),
            ("localizedAttributedStringForKey:value:table:", { _, k, v, t in NSAttributedString(string: string(k, v, t) as String) } as Three),
            (
                "localizedAttributedStringForKey:value:table:localization:",
                { _, k, v, t, _ in NSAttributedString(string: string(k, v, t) as String) } as Four
            ),
        ]
        for (name, block) in plain {
            let selector = NSSelectorFromString(name)
            guard let method = class_getInstanceMethod(Bundle.self, selector) else { continue }
            class_addMethod(ChineseBundle.self, selector, imp_implementationWithBlock(block), method_getTypeEncoding(method))
        }
    }()
}
