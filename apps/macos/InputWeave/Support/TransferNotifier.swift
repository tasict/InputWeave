import AppKit
import UserNotifications

/// System notifications for files: one once
/// the files from a computer have all arrived, and one asking about each
/// file when this computer asks before receiving. The background service
/// opens the app when it has to ask and no app is running, so the offer
/// notification may be the only thing the user sees.
@MainActor
final class TransferNotifier: NotificationSource {
    private enum Action {
        static let show = "show"
        static let accept = "accept"
        static let decline = "decline"
    }

    private enum Category {
        static let received = "received"
        static let offer = "offer"
    }

    /// Files from one computer are announced together once none has
    /// arrived for this long.
    private static let settle: Duration = .milliseconds(1500)

    private let model: AgentModel
    private unowned let notifier: Notifier
    private var states: [String: Inputweave_App_V1_TransferState] = [:]
    private var primed = false
    /// Items that arrived and are not announced yet, by sender.
    private var arrived: [String: [FileTransfer]] = [:]
    /// Files that failed in items that did not arrive, by sender and
    /// batch, to count with the batch's items that did.
    private var failed: [String: [String: Int]] = [:]
    private var announcing: [String: Task<Void, Never>] = [:]
    /// The batches asked about, once each: the answer is for all of it.
    private var asked: Set<String> = []

    init(model: AgentModel, notifier: Notifier) {
        self.model = model
        self.notifier = notifier
    }

    var categories: [UNNotificationCategory] {
        let show = UNNotificationAction(identifier: Action.show, title: String(localized: "Show in Finder"))
        let accept = UNNotificationAction(identifier: Action.accept, title: String(localized: "Accept"))
        let decline = UNNotificationAction(
            identifier: Action.decline, title: String(localized: "Decline"), options: [.destructive])
        return [
            UNNotificationCategory(identifier: Category.received, actions: [show], intentIdentifiers: []),
            UNNotificationCategory(identifier: Category.offer, actions: [accept, decline], intentIdentifiers: []),
        ]
    }

    /// Looks for transfers that changed; called whenever the model does.
    func update() {
        guard model.link == .connected else { return }
        // Files pasted from another device's clipboard are what the user
        // just asked for, not news.
        let incoming = model.transfers.transfers.filter { !$0.outgoing && !$0.clipboard }
        if !primed {
            // What finished before the app connected is not news, but a
            // file waiting for an answer is why the app was opened.
            primed = true
            for transfer in incoming {
                states[transfer.transferID] = transfer.state
                if StatusPresentation.asksAbout(transfer) { ask(transfer) }
            }
            return
        }
        for transfer in incoming {
            let before = states.updateValue(transfer.state, forKey: transfer.transferID)
            guard before != transfer.state else { continue }
            if before == .waitingForAnswer { withdrawOffer(transfer) }
            switch transfer.state {
            case .waitingForAnswer: ask(transfer)
            case .done: arrived[transfer.deviceID, default: []].append(transfer)
            case .failed:
                failed[transfer.deviceID, default: [:]][transfer.batchID, default: 0] += Int(max(1, transfer.filesFailed))
            default: break
            }
        }
        for device in arrived.keys {
            let coming = incoming.contains { $0.deviceID == device && !$0.state.isFinished }
            announcing[device]?.cancel()
            announcing[device] = nil
            if !coming {
                announcing[device] = Task { [weak self] in
                    try? await Task.sleep(for: Self.settle)
                    guard !Task.isCancelled else { return }
                    self?.announce(from: device)
                }
            }
        }
    }

    private func announce(from device: String) {
        announcing[device] = nil
        let failures = failed.removeValue(forKey: device) ?? [:]
        guard let files = arrived.removeValue(forKey: device), !files.isEmpty else { return }
        let batches = Set(files.map(\.batchID))
        let others = failures.filter { batches.contains($0.key) }.values.reduce(0, +)
        let content = UNMutableNotificationContent()
        content.title = TransferText.received(files, device: model.name(of: device), failed: others)
        content.body = files.map(\.name).formatted(.list(type: .and))
        content.categoryIdentifier = Category.received
        content.userInfo = ["paths": files.map(\.path).filter { !$0.isEmpty }]
        notifier.post(UNNotificationRequest(identifier: "received-\(UUID().uuidString)", content: content, trigger: nil))
    }

    private func ask(_ transfer: FileTransfer) {
        guard asked.insert(offerKey(transfer)).inserted else { return }
        let content = UNMutableNotificationContent()
        content.title = TransferText.offerTitle(transfer, device: model.name(of: transfer.deviceID))
        content.body = TransferText.offerBody(transfer)
        content.categoryIdentifier = Category.offer
        content.userInfo = ["transfer": transfer.transferID]
        content.interruptionLevel = .timeSensitive
        notifier.post(UNNotificationRequest(identifier: offerID(transfer), content: content, trigger: nil)) {
            // Without notifications the question goes to the window.
            AppActions.reopen(page: .files)
        }
    }

    private func withdrawOffer(_ transfer: FileTransfer) {
        guard asked.remove(offerKey(transfer)) != nil else { return }
        notifier.withdraw([offerID(transfer)])
    }

    /// What one question is about: the batch, or a file from an agent from
    /// before batches.
    private func offerKey(_ transfer: FileTransfer) -> String {
        transfer.batchID.isEmpty ? transfer.transferID : transfer.batchID
    }

    private func offerID(_ transfer: FileTransfer) -> String { "offer-\(offerKey(transfer))" }

    func respond(_ response: Notifier.Response) {
        let transfer = response.strings["transfer"]
        let paths = response.lists["paths"] ?? []
        switch response.action {
        case Action.accept, Action.decline:
            guard let transfer else { return }
            let accept = response.action == Action.accept
            Task { try? await model.answerFileOffer(transfer, accept: accept) }
        case Action.show:
            NSWorkspace.shared.activateFileViewerSelecting(paths.map { URL(fileURLWithPath: $0) })
        case UNNotificationDefaultActionIdentifier:
            if paths.isEmpty {
                AppActions.reopen(page: .files)
            } else {
                NSWorkspace.shared.activateFileViewerSelecting(paths.map { URL(fileURLWithPath: $0) })
            }
        default:
            break
        }
    }
}
