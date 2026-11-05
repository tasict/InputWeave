import AppKit
import Observation
import Synchronization
import UserNotifications

/// How the folders macOS protects (Files & Folders) read in setup and on the
/// Troubleshooting page: the background service
/// lists them when asked (`RequestFolderAccess`), so macOS asks about each
/// then rather than in the middle of a transfer.
enum FolderAccessText {
    static func name(_ folder: Inputweave_App_V1_UserFolder) -> String {
        switch folder {
        case .downloads: String(localized: "Downloads")
        case .desktop: String(localized: "Desktop")
        case .documents: String(localized: "Documents")
        default: String(localized: "Folder")
        }
    }

    /// The state's SF Symbol and words.
    static func state(_ state: Inputweave_App_V1_FolderAccessState) -> (symbol: String, text: String) {
        switch state {
        case .asking: ("clock", String(localized: "Waiting for your answer…"))
        case .granted: ("checkmark.circle.fill", String(localized: "Allowed"))
        case .denied: ("xmark.circle.fill", String(localized: "Not allowed"))
        case .missing: ("minus.circle", String(localized: "Not on this Mac"))
        case .failed: ("exclamationmark.triangle.fill", String(localized: "Could not be checked"))
        default: ("circle", String(localized: "Not asked yet"))
        }
    }

    /// `RequestFolderAccess` failed: nothing was asked.
    static var requestFailed: String {
        String(localized: "The background service could not ask for the folders. macOS will ask when a transfer needs one.")
    }

    static func color(_ state: Inputweave_App_V1_FolderAccessState) -> NSColor {
        switch state {
        case .granted: .systemGreen
        case .denied: .systemRed
        case .failed: .systemOrange
        default: .secondaryLabelColor
        }
    }
}

/// Whether the app may post notifications: the one place that asks macOS
/// and remembers the answer, for setup (which asks together with the
/// folders) and for `Notifier` (which asks the first time it posts, if
/// setup was skipped). Read again whenever the app becomes active, as the
/// user may have changed it in System Settings meanwhile. Snapshots and
/// tests give their own `read` and `ask`, and never reach the system.
@MainActor
@Observable
final class NotificationPermission {
    enum State {
        /// Not looked up yet.
        case unknown
        case notAsked
        case allowed
        case denied
    }

    private(set) var state: State = .unknown
    @ObservationIgnored private let read: @MainActor () async -> State
    @ObservationIgnored private let ask: @MainActor () async -> Void
    /// Removed with the object; deinit runs off the main actor, and only
    /// reads it.
    @ObservationIgnored nonisolated(unsafe) private var activation: (any NSObjectProtocol)?

    /// The system's.
    convenience init() {
        self.init(
            read: {
                switch await UNUserNotificationCenter.current().notificationSettings().authorizationStatus {
                case .notDetermined: .notAsked
                case .denied: .denied
                default: .allowed
                }
            },
            ask: {
                _ = try? await UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound])
            })
    }

    init(read: @escaping @MainActor () async -> State, ask: @escaping @MainActor () async -> Void) {
        self.read = read
        self.ask = ask
        activation = NotificationCenter.default.addObserver(
            forName: NSApplication.didBecomeActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
            Task { @MainActor in await self?.refresh() }
        }
    }

    deinit {
        if let activation {
            NotificationCenter.default.removeObserver(activation)
        }
    }

    /// A permission that stays `state`, for snapshots.
    convenience init(fixed state: State) {
        self.init(read: { state }, ask: {})
        self.state = state
    }

    /// Reads what the user decided, without asking.
    func refresh() async {
        state = await read()
    }

    /// Has macOS ask the user, if it has not.
    func request() async {
        await ask()
        await refresh()
    }

    /// Whether notifications may be posted, having macOS ask the user the
    /// first time.
    func allowed() async -> Bool {
        if state == .unknown {
            await refresh()
        }
        if state == .notAsked {
            await request()
        }
        return state == .allowed
    }

    /// The app's page in System Settings › Notifications.
    static func openSettings() {
        let id = Bundle.main.bundleIdentifier ?? "cc.ctsite.inputweave"
        if let url = URL(string: "x-apple.systempreferences:com.apple.Notifications-Settings.extension?id=\(id)") {
            NSWorkspace.shared.open(url)
        }
    }
}

extension AgentModel {
    /// Waits until `condition` holds, looking again whenever something it
    /// read from the model changes, rather than every so often.
    func wait(until condition: @escaping @MainActor () -> Bool) async {
        while !condition() {
            await withCheckedContinuation { (continuation: CheckedContinuation<Void, Never>) in
                let resume = ResumeOnce(continuation)
                let met = withObservationTracking {
                    condition()
                } onChange: {
                    resume()
                }
                if met {
                    resume()
                }
            }
        }
    }
}

/// Resumes a continuation the first time it is called: a change and the
/// condition already holding may both try.
private final class ResumeOnce: Sendable {
    private let continuation: Mutex<CheckedContinuation<Void, Never>?>

    init(_ continuation: CheckedContinuation<Void, Never>) {
        self.continuation = Mutex(continuation)
    }

    func callAsFunction() {
        let pending = continuation.withLock { pending in
            defer { pending = nil }
            return pending
        }
        pending?.resume()
    }
}
