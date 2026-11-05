import AppKit
import UserNotifications

/// Posts system notifications through `Notifier` and answers the ones the
/// user clicks.
@MainActor
protocol NotificationSource: AnyObject {
    /// The categories of this source's notifications, with their actions.
    var categories: [UNNotificationCategory] { get }
    func respond(_ response: Notifier.Response)
}

/// The app's system notifications: files from
/// `TransferNotifier`, problems from `ProblemNotifier`. This is the one
/// notification center delegate: it registers every source's categories,
/// posts once `NotificationPermission` allows it, and hands each response
/// to the source whose category the notification has.
@MainActor
final class Notifier: NSObject {
    /// What the user did with a notification, read out of the response.
    struct Response: Sendable {
        let category: String
        /// The action's identifier, or `UNNotificationDefaultActionIdentifier`
        /// when the notification itself was clicked.
        let action: String
        /// The notification's `userInfo` values that are strings, and those
        /// that are lists of strings.
        let strings: [String: String]
        let lists: [String: [String]]
    }

    private var sources: [String: any NotificationSource] = [:]
    private let permission: NotificationPermission

    init(permission: NotificationPermission) {
        self.permission = permission
        super.init()
    }

    /// Takes over notification responses. Called before launching
    /// finishes, so responses that launched the app are delivered.
    func activate(_ sources: [any NotificationSource]) {
        var categories: Set<UNNotificationCategory> = []
        for source in sources {
            for category in source.categories {
                self.sources[category.identifier] = source
                categories.insert(category)
            }
        }
        let center = UNUserNotificationCenter.current()
        center.delegate = self
        center.setNotificationCategories(categories)
    }

    /// Posts once notifications are allowed, asking the first time;
    /// otherwise runs `fallback`.
    func post(_ request: UNNotificationRequest, fallback: (@MainActor () -> Void)? = nil) {
        Task {
            guard await permission.allowed() else {
                fallback?()
                return
            }
            try? await UNUserNotificationCenter.current().add(request)
        }
    }

    /// Takes notifications back, shown or not yet shown.
    func withdraw(_ identifiers: [String]) {
        guard !identifiers.isEmpty else { return }
        let center = UNUserNotificationCenter.current()
        center.removeDeliveredNotifications(withIdentifiers: identifiers)
        center.removePendingNotificationRequests(withIdentifiers: identifiers)
    }

    fileprivate func respond(_ response: Response) {
        sources[response.category]?.respond(response)
    }
}

extension Notifier: UNUserNotificationCenterDelegate {
    nonisolated func userNotificationCenter(
        _: UNUserNotificationCenter, didReceive response: UNNotificationResponse
    ) async {
        let content = response.notification.request.content
        var strings: [String: String] = [:]
        var lists: [String: [String]] = [:]
        for case let (key as String, value) in content.userInfo {
            if let value = value as? String {
                strings[key] = value
            } else if let value = value as? [String] {
                lists[key] = value
            }
        }
        await respond(Response(
            category: content.categoryIdentifier, action: response.actionIdentifier,
            strings: strings, lists: lists))
    }

    /// Shown even while the app is in front: the panel may be open.
    nonisolated func userNotificationCenter(
        _: UNUserNotificationCenter, willPresent _: UNNotification
    ) async -> UNNotificationPresentationOptions {
        [.banner, .list, .sound]
    }
}
