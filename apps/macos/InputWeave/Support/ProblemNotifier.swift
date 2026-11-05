import Foundation
import UserNotifications

/// System notifications for problems that stop sharing while the user may
/// not be looking at the panel: a permission
/// of the background service turned off, or the background service turned
/// off. `ProblemWatch` decides when; this posts them, and takes one back
/// once its problem is fixed. Clicking one opens Troubleshooting.
@MainActor
final class ProblemNotifier: NotificationSource {
    private enum Category {
        static let permission = "permission"
        static let serviceInLoginItems = "service-login-items"
        static let serviceOff = "service-off"
    }

    private enum Action {
        static let openSettings = "open-settings"
        static let openLoginItems = "open-login-items"
        static let turnOn = "turn-on"
    }

    private let model: AgentModel
    private let service: BackgroundService
    private unowned let notifier: Notifier
    private var watch = ProblemWatch()

    init(model: AgentModel, service: BackgroundService, notifier: Notifier) {
        self.model = model
        self.service = service
        self.notifier = notifier
    }

    var categories: [UNNotificationCategory] {
        func category(_ id: String, action: String, title: String) -> UNNotificationCategory {
            UNNotificationCategory(
                identifier: id, actions: [UNNotificationAction(identifier: action, title: title)],
                intentIdentifiers: [])
        }
        return [
            category(Category.permission, action: Action.openSettings, title: String(localized: "Open System Settings")),
            category(Category.serviceInLoginItems, action: Action.openLoginItems, title: String(localized: "Open Login Items")),
            category(Category.serviceOff, action: Action.turnOn, title: String(localized: "Turn On")),
        ]
    }

    /// Looks for problems that appeared or went away; called whenever the
    /// model or the service changes.
    func update() {
        let state: ProblemWatch.Service = switch service.state {
        case .enabled: .on
        case .requiresApproval: .needsApproval
        case .notRegistered: .off
        case .notFound: .missing
        }
        let change = watch.update(
            service: state,
            permissions: model.link == .connected ? model.status.permissions : nil,
            quiet: !UserDefaults.standard.bool(forKey: Preferences.finishedSetup) || service.unregistering)
        notifier.withdraw(change.fixed.map(identifier))
        for problem in change.raised {
            post(problem)
        }
    }

    /// One notification per kind of problem, so a newer one replaces it.
    private func identifier(_ problem: ProblemWatch.Problem) -> String {
        switch problem.kind {
        case .permission: "problem-permission"
        case .service: "problem-service"
        }
    }

    private func post(_ problem: ProblemWatch.Problem) {
        // The sentence is the body: a banner shows the title on one line,
        // and with no title it shows the app's name there instead.
        let content = UNMutableNotificationContent()
        switch problem {
        case .permissionTurnedOff(let permission):
            let pane: SystemSettings.Pane
            if permission == .accessibility {
                content.body = String(localized: "InputWeave's Accessibility permission was turned off, so sharing stopped.")
                pane = .accessibility
            } else {
                content.body = String(localized: "InputWeave's Input Monitoring permission was turned off, so sharing stopped.")
                pane = .inputMonitoring
            }
            content.categoryIdentifier = Category.permission
            content.userInfo = ["pane": pane.rawValue]
        case .serviceTurnedOff(let inLoginItems):
            content.body = String(localized: "The background service is turned off, so sharing is not working.")
            content.categoryIdentifier = inLoginItems ? Category.serviceInLoginItems : Category.serviceOff
        }
        notifier.post(UNNotificationRequest(identifier: identifier(problem), content: content, trigger: nil))
    }

    func respond(_ response: Notifier.Response) {
        switch response.action {
        case Action.openSettings:
            SystemSettings.open(response.strings["pane"].flatMap(SystemSettings.Pane.init(rawValue:)) ?? .accessibility)
        case Action.openLoginItems:
            service.openLoginItemsSettings()
        case Action.turnOn:
            service.register()
        case UNNotificationDefaultActionIdentifier:
            AppActions.reopen(page: .troubleshooting)
        default:
            break
        }
    }
}
