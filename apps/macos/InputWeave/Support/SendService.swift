import AppKit

/// Finder's Services › Send with InputWeave (`NSServices` in Info.plist):
/// the files and folders chosen go the same way as with Send Files…
/// (`SendFiles`), at once to the only computer that can take them, or
/// after asking in a small panel near the pointer (`SendChooser`), which
/// lists those that can't too, with why; with no computer connected, the
/// panel says so. Items sent while the panel asks join it. A Share extension
/// would have to run in a sandbox, where the background service's socket
/// is out of reach; the service menu is the app's own.
@MainActor
final class SendService: NSObject {
    private let model: AgentModel
    private let hud: HUDController
    private let chooser = SendChooser()

    /// How long an app macOS just opened for the service waits for the
    /// background service before saying it is not responding.
    static let connectWait: Duration = .seconds(10)

    init(model: AgentModel, hud: HUDController) {
        self.model = model
        self.hud = hud
    }

    /// Called by macOS with the items chosen in Finder as file URLs; the
    /// name is the `NSMessage` in Info.plist.
    @objc func sendFiles(_ pasteboard: NSPasteboard, userData _: String?, error _: AutoreleasingUnsafeMutablePointer<NSString?>?) {
        let urls = pasteboard.readObjects(forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true]) as? [URL] ?? []
        guard !urls.isEmpty else { return }
        Task { await send(urls) }
    }

    func send(_ urls: [URL]) async {
        // Another request while the chooser asks joins it.
        if chooser.isChoosing {
            chooser.add(urls)
            return
        }
        // Opened for this, the app is still connecting.
        guard await model.connected(within: Self.connectWait) else {
            chooser.notice(String(localized: "The background service is not responding."))
            return
        }
        // An earlier request may have opened the chooser meanwhile.
        if chooser.isChoosing {
            chooser.add(urls)
            return
        }
        switch SendTargets.choice(model.status.devices, profileFilesOff: ProfileText.filesOff(model.status)) {
        case .nowhere:
            chooser.notice(String(localized: "No connected computer can receive files."))
        case .only(let id):
            await deliver(urls, to: id)
        case .ask:
            chooser.choose(urls, model: model) { [weak self] id, urls in
                Task { await self?.deliver(urls, to: id) }
            }
        }
    }

    /// Sends, says so in the HUD once the service took the files, or says
    /// why they could not go, as Send Files… does.
    private func deliver(_ urls: [URL], to id: String) async {
        do {
            try await model.sendFiles(urls, to: id)
            hud.note(SendTargets.sending(items: urls.count, to: model.name(of: id)))
        } catch {
            NSApp.activate()
            SendFilesPanel.showFailure(error, device: model.name(of: id))
        }
    }
}
