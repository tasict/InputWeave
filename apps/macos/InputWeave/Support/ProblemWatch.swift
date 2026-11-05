import Foundation

/// Decides which problems get a system notification: a permission the background service had and no longer has,
/// or a background service that was on and is now off. Only a change is
/// news. What was already wrong when the app started shows in the panel and
/// in Troubleshooting instead, and so does a change made before setup
/// finished or while the app turns the service off itself.
struct ProblemWatch {
    /// The background service as Login Items has it.
    enum Service: Equatable {
        case on
        /// Turned off in System Settings › Login Items.
        case needsApproval
        case off
        /// Missing from the app: a broken install, not something the user
        /// turned off.
        case missing
    }

    enum Problem: Equatable {
        case permissionTurnedOff(Inputweave_App_V1_Permission)
        /// `inLoginItems` when Login Items has it turned off, so it is
        /// turned back on there; otherwise the app can turn it on.
        case serviceTurnedOff(inLoginItems: Bool)

        enum Kind: Equatable {
            case permission, service
        }

        /// Problems of one kind share a notification: a newer one replaces
        /// the older.
        var kind: Kind {
            switch self {
            case .permissionTurnedOff: .permission
            case .serviceTurnedOff: .service
            }
        }
    }

    struct Change: Equatable {
        /// Problems to notify about.
        var raised: [Problem] = []
        /// Problems notified about earlier that are fixed now, whose
        /// notifications can go.
        var fixed: [Problem] = []
    }

    /// The permissions that have a notification, in the order they are
    /// named when several are turned off at once.
    static let watched: [Inputweave_App_V1_Permission] = [.accessibility, .inputMonitoring]

    /// What the service last reported for each permission. Kept while the
    /// app cannot reach it: turning a permission off may restart it.
    private var granted: [Inputweave_App_V1_Permission: Bool] = [:]
    private var service: Service?
    private var notified: [Problem] = []

    /// Takes in the current state and returns what to notify about and
    /// which notifications to take back. The first state seen is only
    /// remembered.
    /// - Parameters:
    ///   - permissions: what the background service reports, or nil while
    ///     the app is not connected to it.
    ///   - quiet: remembers the state without raising anything: before
    ///     setup has finished, and while the app turns the service off
    ///     itself (to quit, or to register it again).
    mutating func update(
        service: Service, permissions: [Inputweave_App_V1_PermissionStatus]?, quiet: Bool
    ) -> Change {
        var turnedOff: [Inputweave_App_V1_Permission] = []
        for status in permissions ?? [] where Self.watched.contains(status.permission) {
            if granted[status.permission] == true && !status.granted {
                turnedOff.append(status.permission)
            }
            granted[status.permission] = status.granted
        }
        let serviceTurnedOff = self.service == .on && (service == .needsApproval || service == .off)
        self.service = service

        var change = Change()
        let granted = self.granted
        notified.removeAll { problem in
            let fixed = switch problem {
            case .permissionTurnedOff(let permission): granted[permission] == true
            case .serviceTurnedOff: service == .on
            }
            if fixed { change.fixed.append(problem) }
            return fixed
        }
        guard !quiet else { return change }
        // On macOS 27 one switch covers both permissions, so they can go
        // together; one notification is enough, and the panel lists both.
        if let first = Self.watched.first(where: turnedOff.contains) {
            change.raised.append(.permissionTurnedOff(first))
        }
        if serviceTurnedOff {
            change.raised.append(.serviceTurnedOff(inLoginItems: service == .needsApproval))
        }
        for problem in change.raised {
            notified.removeAll { $0.kind == problem.kind }
            notified.append(problem)
        }
        return change
    }
}
