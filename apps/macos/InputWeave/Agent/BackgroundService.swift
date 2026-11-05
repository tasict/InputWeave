import AppKit
import CoreServices
import Observation
import ServiceManagement

/// The background service as macOS sees it: the LaunchAgent the app
/// registers with SMAppService, and the app's own login item.
@MainActor @Observable
final class BackgroundService {
    enum State: Equatable {
        case enabled
        /// Registered, but the user has to allow it in System Settings.
        case requiresApproval
        case notRegistered
        /// The app bundle does not contain the service; a broken install.
        case notFound
    }

    static let label = "cc.ctsite.inputweave.agent"

    /// The LaunchAgent plist in this app, which also carries the digest of
    /// the agent bundled with it (scripts/embed-agent.sh).
    static var plist: URL {
        Bundle.main.bundleURL.appending(path: "Contents/Library/LaunchAgents/\(label).plist")
    }

    private(set) var state: State = .notRegistered
    private(set) var launchesAtLogin = false
    private(set) var lastError: String?
    /// Whether the app itself is taking the service out of Login Items, to
    /// quit or to register it again, so that is not taken for the user
    /// turning it off.
    private(set) var unregistering = false

    @ObservationIgnored private let agent = SMAppService.agent(plistName: "\(BackgroundService.label).plist")

    init() {
        refresh()
    }

    /// Reads the state again. Login Items does not say when the user
    /// changes it there, so the app asks while the service is out of reach.
    func refresh() {
        let state: State = switch agent.status {
        case .enabled: .enabled
        case .requiresApproval: .requiresApproval
        case .notFound: .notFound
        default: .notRegistered
        }
        let launchesAtLogin = SMAppService.mainApp.status == .enabled
        // Observers hear about every assignment, even of an equal value.
        if state != self.state { self.state = state }
        if launchesAtLogin != self.launchesAtLogin { self.launchesAtLogin = launchesAtLogin }
    }

    /// Lets the service run in the background from now on and at login.
    func register() {
        // launchd finds the service's program through the app with this
        // bundle identifier that Launch Services knows best, at every
        // launch. Make that this copy, not a build product or a disk image
        // that may be gone by then.
        LSRegisterURL(Bundle.main.bundleURL as CFURL, true)
        do {
            try agent.register()
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
        refresh()
    }

    /// Stops the service until the app registers it again, on its next
    /// launch.
    func stopUntilReopened() async throws {
        // Stays set: the app quits next.
        unregistering = true
        try await agent.unregister()
        refresh()
    }

    /// Registers the service again, for one launchd will not start. An ad
    /// hoc signed agent may run only with the code hash it was registered
    /// with; a rebuilt one starts once registered again, since its digest
    /// in the plist (scripts/embed-agent.sh) makes macOS take the new
    /// agent. launchd may also still look for the agent in a copy of the
    /// app that is gone.
    func registerAgain() async {
        unregistering = true
        defer { unregistering = false }
        try? await agent.unregister()
        register()
    }

    /// For the uninstaller, which runs `InputWeave --unregister-service` as
    /// the user who registered the service: only the app can take it out of
    /// Login Items. Unregistering stops it. Open at Login goes too. Returns
    /// false, with the reason on stderr, if the service is still
    /// registered.
    static func unregisterForUninstaller() -> Bool {
        let main = SMAppService.mainApp
        if main.status != .notRegistered && main.status != .notFound {
            try? main.unregister()
        }
        let agent = SMAppService.agent(plistName: "\(label).plist")
        var gone: Bool { agent.status == .notRegistered || agent.status == .notFound }
        if gone { return true }
        do {
            try agent.unregister()
        } catch where !gone {
            FileHandle.standardError.write(Data("cannot unregister the background service: \(error.localizedDescription)\n".utf8))
        } catch {}
        return gone
    }

    func openLoginItemsSettings() {
        SMAppService.openSystemSettingsLoginItems()
    }

    /// Restarts the service, e.g. after a permission was granted that only
    /// applies to new processes.
    func restart() {
        let process = Process()
        process.executableURL = URL(filePath: "/bin/launchctl")
        process.arguments = ["kickstart", "-k", "gui/\(getuid())/\(Self.label)"]
        try? process.run()
    }

    func setLaunchesAtLogin(_ on: Bool) {
        do {
            if on {
                try SMAppService.mainApp.register()
            } else {
                try SMAppService.mainApp.unregister()
            }
            lastError = nil
        } catch {
            lastError = error.localizedDescription
        }
        refresh()
    }
}
