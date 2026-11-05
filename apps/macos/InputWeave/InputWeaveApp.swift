import AppKit
import Observation
import Sparkle
import SwiftUI

struct InputWeaveApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @AppStorage(Preferences.showsMenuBarIcon) private var showsMenuBarIcon = true

    var body: some Scene {
        MenuBarExtra(isInserted: $showsMenuBarIcon) {
            MenuPanel()
                .environment(delegate.model)
                .environment(delegate.service)
        } label: {
            StatusLabel()
                .environment(delegate.model)
                .environment(delegate.service)
        }
        .menuBarExtraStyle(.window)

        Window("InputWeave", id: "main") {
            MainWindow()
                .environment(delegate.model)
                .environment(delegate.service)
                .environment(delegate.license)
                .background(DockPresence.Marker())
        }
        .defaultSize(width: 920, height: 600)
        .windowResizability(.contentMinSize)
        .commands { AboutCommand() }

        Window("Set Up InputWeave", id: "onboarding") {
            OnboardingView()
                .environment(delegate.model)
                .environment(delegate.service)
                .environment(delegate.notificationPermission)
                .background(DockPresence.Marker())
        }
        .windowResizability(.contentSize)
        .defaultPosition(.center)

        Window("About InputWeave", id: "about") {
            AboutView()
                .environment(delegate.license)
                .background(DockPresence.Marker())
        }
        .windowResizability(.contentSize)
        .defaultPosition(.center)

        Window("Acknowledgments", id: "acknowledgments") {
            AcknowledgmentsView()
                .background(DockPresence.Marker())
        }
        .defaultSize(width: 640, height: 560)
        .windowResizability(.contentMinSize)
        .defaultPosition(.center)
    }
}

/// The app menu, shown while a window puts the app in the Dock, opens the
/// same About window as the panel.
private struct AboutCommand: Commands {
    @Environment(\.openWindow) private var openWindow

    var body: some Commands {
        CommandGroup(replacing: .appInfo) {
            Button("About InputWeave") { AppActions.showAbout(openWindow) }
        }
    }
}

/// Puts the app in the Dock and the ⌘-Tab switcher while one of its
/// windows is open, so a window behind other apps can be found again, and
/// takes it out once they are all closed. Only windows marked with
/// `Marker` count: the menu bar panel, the HUD, the cursor locator, the
/// pairing code panel and open and save panels do not.
@MainActor
final class DockPresence: NSObject {
    static let shared = DockPresence()
    private let windows = NSHashTable<NSWindow>.weakObjects()

    func start() {
        let center = NotificationCenter.default
        center.addObserver(self, selector: #selector(windowDidBecomeKey(_:)), name: NSWindow.didBecomeKeyNotification, object: nil)
        center.addObserver(self, selector: #selector(windowWillClose(_:)), name: NSWindow.willCloseNotification, object: nil)
    }

    private func track(_ window: NSWindow) {
        windows.add(window)
        // The window is usually ordered in after its content moves into it.
        Task { @MainActor in self.update() }
    }

    @objc private func windowDidBecomeKey(_ notification: Notification) {
        guard let window = notification.object as? NSWindow, windows.contains(window) else { return }
        update()
    }

    @objc private func windowWillClose(_ notification: Notification) {
        guard let window = notification.object as? NSWindow, windows.contains(window) else { return }
        // The closing window still counts as visible here.
        update(closing: window)
    }

    private func update(closing: NSWindow? = nil) {
        let open = windows.allObjects.contains { $0 !== closing && ($0.isVisible || $0.isMiniaturized) }
        let policy: NSApplication.ActivationPolicy = open ? .regular : .accessory
        guard NSApp.activationPolicy() != policy else { return }
        NSApp.setActivationPolicy(policy)
        if open {
            NSApp.activate()
        }
    }

    /// Marks the window this view is in as one that puts the app in the
    /// Dock.
    struct Marker: NSViewRepresentable {
        func makeNSView(context _: Context) -> NSView { MarkerView() }
        func updateNSView(_: NSView, context _: Context) {}
    }

    private final class MarkerView: NSView {
        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if let window { DockPresence.shared.track(window) }
        }
    }
}

/// The menu bar icon. It also opens windows the app needs on its own:
/// setup on first launch, and the main window when the app is reopened.
private struct StatusLabel: View {
    @Environment(AgentModel.self) private var model
    @Environment(BackgroundService.self) private var service
    @Environment(\.openWindow) private var openWindow
    /// Moved on once the service has been out of reach long enough to
    /// count as not responding, so the icon looks again.
    @State private var now = Date.now

    var body: some View {
        let state = StatusIcon.State(model, serviceEnabled: service.state == .enabled, now: now)
        Image(nsImage: StatusIcon.image(state))
            .accessibilityLabel(Text(StatusIcon.description(state)))
            .task(id: model.unreachableSince) {
                guard let since = model.unreachableSince else { return }
                let wait = since.addingTimeInterval(AgentModel.unresponsiveAfter).timeIntervalSinceNow
                try? await Task.sleep(for: .seconds(max(0, wait) + 0.5))
                if !Task.isCancelled { now = .now }
            }
            .task {
                if !UserDefaults.standard.bool(forKey: Preferences.finishedSetup) {
                    openWindow(id: "onboarding")
                    NSApp.activate()
                } else if AppActions.navigation.mainWindowPending {
                    // Asked for while the icon was hidden (`AppActions.reopen`).
                    AppActions.showMain(openWindow)
                }
            }
            .onReceive(NotificationCenter.default.publisher(for: .showMainWindow)) { _ in
                AppActions.showMain(openWindow)
            }
    }
}

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    let model = AgentModel()
    let service = BackgroundService()
    let notificationPermission = NotificationPermission()
    private lazy var hud = HUDController(model: model)
    private lazy var locator = CursorLocator(model: model)
    private lazy var identifier = ScreenIdentifier(model: model)
    private lazy var edgeGlow = EdgeGlow()
    /// Activates and confirms the license with Polar; only the settings'
    /// License page and About show it.
    lazy var license = LicenseController(agent: model, polar: PolarClient(config: .fromBundle() ?? .unconfigured))
    private lazy var codePanel = PairingCodePanel(model: model)
    private lazy var notifier = Notifier(permission: notificationPermission)
    private lazy var transferNotifier = TransferNotifier(model: model, notifier: notifier)
    private lazy var problemNotifier = ProblemNotifier(model: model, service: service, notifier: notifier)
    private lazy var dragStrips = DragStrips(model: model, hud: hud)
    private lazy var dragPromises = DragPromises(model: model, hud: hud)
    /// The transfer list was seen since the app started.
    private var sawTransfers = false
    private var restartedOutdatedService = false
    /// The agent this app carries, to tell whether the service runs it.
    private let bundledAgentDigest = OutdatedService.bundledDigest(plist: BackgroundService.plist)
    private var registeredAgain = false
    private var updaterController: SPUStandardUpdaterController?

    func applicationWillFinishLaunching(_: Notification) {
        // The uninstaller runs the app this way, as the user, to take the
        // background service out of Login Items; nothing else happens.
        if CommandLine.arguments.contains("--unregister-service") {
            exit(BackgroundService.unregisterForUninstaller() ? 0 : 1)
        }
        notifier.activate([transferNotifier, problemNotifier])
        DockPresence.shared.start()
    }

    func applicationDidFinishLaunching(_: Notification) {
        revealHiddenIcon()
        // Coming back after "Quit InputWeave" or a login: sharing resumes.
        if UserDefaults.standard.bool(forKey: Preferences.finishedSetup) && service.state == .notRegistered {
            service.register()
        }
        model.onCursorCrossed = { [weak self] crossed in
            self?.edgeGlow.light(crossed)
        }
        model.onDrag = { [weak self] news in
            guard let self else { return }
            switch news {
            case .arrived(let arrived):
                self.dragPromises.arrived(arrived)
            case .ended(let ended):
                self.dragStrips.ended(ended)
                self.dragPromises.ended(ended)
            }
        }
        model.start()
        startUpdater()
        observe { [weak self] in
            self?.hud.update()
            self?.locator.update()
            self?.identifier.update()
            self?.codePanel.update()
            self?.transferNotifier.update()
            self?.problemNotifier.update()
            self?.keepDismissedTransfers()
            self?.dragStrips.update()
            self?.dragPromises.update()
            self?.restartOutdatedService()
        }
        license.start()
        // Login Items does not tell the app when the user turns the service
        // off there, but launchd then stops it: look while it is out of
        // reach, so the problem notification can tell. Runs as long as the
        // app does.
        Task { [weak self] in
            while true {
                try? await Task.sleep(for: .seconds(2))
                guard let self else { return }
                if self.model.link != .connected { self.service.refresh() }
            }
        }
        // Only around launch, which is when an update or another copy of
        // the app leaves launchd unable to start the service.
        Task { [weak self] in
            for _ in 0..<30 {
                try? await Task.sleep(for: .seconds(2))
                guard let self, !self.registeredAgain else { return }
                await self.registerStuckServiceAgain()
            }
        }
    }

    /// Opening the app, rather than logging in, brings back a hidden menu
    /// bar icon, as reopening a running app does (see
    /// `applicationShouldHandleReopen`). The Apple event that opened the app
    /// is current only here, not in `applicationWillFinishLaunching`.
    private func revealHiddenIcon() {
        let defaults = UserDefaults.standard
        let reveal = LaunchReveal.decide(
            launchedAtLogin: LaunchReveal.launchedAtLogin(NSAppleEventManager.shared().currentAppleEvent),
            iconShown: defaults.object(forKey: Preferences.showsMenuBarIcon) as? Bool ?? true,
            finishedSetup: defaults.bool(forKey: Preferences.finishedSetup))
        if reveal.showsMainWindow {
            AppActions.reopen()
        } else if reveal.showsIcon {
            defaults.set(true, forKey: Preferences.showsMenuBarIcon)
        }
    }

    /// A service that is registered but has not answered for a while is
    /// registered again, once per launch: after an update launchd may
    /// refuse to start it (see `BackgroundService.registerAgain`).
    private func registerStuckServiceAgain() async {
        guard !registeredAgain, service.state == .enabled, model.isUnresponsive(at: .now) else { return }
        registeredAgain = true
        await service.registerAgain()
    }

    /// Sparkle, when this build names an update feed and the key its updates
    /// are signed with. After an update the new app restarts the service
    /// (see `restartOutdatedService`).
    private func startUpdater() {
        let info = Bundle.main.infoDictionary
        let feed = info?["SUFeedURL"] as? String ?? ""
        let key = info?["SUPublicEDKey"] as? String ?? ""
        guard !feed.isEmpty, !key.isEmpty else { return }
        let controller = SPUStandardUpdaterController(startingUpdater: true, updaterDelegate: nil, userDriverDelegate: nil)
        updaterController = controller
        let sparkle = controller.updater
        AppActions.updater = Updater(
            load: { sparkle.automaticallyChecksForUpdates },
            store: { sparkle.automaticallyChecksForUpdates = $0 },
            check: { controller.checkForUpdates(nil) })
    }

    /// After an update, launchd may still run the previous service. It is
    /// restarted once, from the new app's bundle, when it speaks another
    /// IPC version, or reports another version or build than the agent
    /// this app carries (`OutdatedService`). Only once per launch, so a
    /// service that does not come back is not restarted over and over.
    private func restartOutdatedService() {
        guard !restartedOutdatedService, service.state == .enabled else { return }
        let outdated = OutdatedService.isOutdated(
            link: model.link, agentVersion: model.agentVersion, appVersion: Bundle.main.shortVersion,
            agentDigest: model.agentDigest, bundledDigest: bundledAgentDigest)
        guard outdated else { return }
        restartedOutdatedService = true
        service.restart()
    }

    /// Keeps the problems dismissed from the panel to those still in the
    /// history, and counts those there when the app first sees the list as
    /// dismissed, so they do not come back after a restart.
    private func keepDismissedTransfers() {
        guard model.link == .connected else { return }
        let navigation = AppActions.navigation
        let kept = TransferText.dismissed(
            navigation.dismissedTransfers, listed: model.transfers.transfers, firstLook: !sawTransfers)
        sawTransfers = true
        if kept != navigation.dismissedTransfers { navigation.dismissedTransfers = kept }
    }

    /// Opening the app again while it runs shows its window, and the menu
    /// bar icon if it was hidden.
    func applicationShouldHandleReopen(_: NSApplication, hasVisibleWindows _: Bool) -> Bool {
        AppActions.reopen()
        return true
    }

    /// Runs `body` now and again whenever an observable value it read
    /// changes.
    private func observe(_ body: @escaping @MainActor () -> Void) {
        withObservationTracking {
            body()
        } onChange: { [weak self] in
            Task { @MainActor in self?.observe(body) }
        }
    }
}
