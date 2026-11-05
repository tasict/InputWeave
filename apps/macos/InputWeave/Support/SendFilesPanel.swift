import AppKit

/// "Send Files…": an open panel to choose files
/// and folders, with the computer to send them to underneath.
@MainActor
enum SendFilesPanel {
    static func present(model: AgentModel, to preselected: String? = nil) {
        let devices = model.fileReceivers
        guard !devices.isEmpty else { return }

        let popup = NSPopUpButton(frame: .zero, pullsDown: false)
        for device in devices {
            popup.addItem(withTitle: device.name)
            popup.lastItem?.representedObject = device.id
        }
        if let index = devices.firstIndex(where: { $0.id == preselected }) {
            popup.selectItem(at: index)
        }
        let row = NSStackView(views: [NSTextField(labelWithString: String(localized: "Send to:")), popup])
        row.edgeInsets = NSEdgeInsets(top: 10, left: 16, bottom: 10, right: 16)
        row.frame.size = row.fittingSize

        let panel = NSOpenPanel()
        panel.title = String(localized: "Send Files")
        panel.prompt = String(localized: "Send")
        panel.canChooseFiles = true
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = true
        panel.accessoryView = row
        panel.isAccessoryViewDisclosed = true

        NSApp.activate()
        guard panel.runModal() == .OK,
              let deviceID = popup.selectedItem?.representedObject as? String
        else { return }
        let urls = panel.urls
        Task {
            do {
                try await model.sendFiles(urls, to: deviceID)
            } catch {
                showFailure(error, device: model.name(of: deviceID))
            }
        }
    }

    static func showFailure(_ error: any Error, device: String) {
        let alert = NSAlert()
        alert.messageText = String(localized: "The files could not be sent to \(device).")
        switch (error as? AgentRefusal)?.code {
        case "too_many_items", "too_large":
            alert.informativeText = DragText.limits
        case let code? where ["not_a_file", "unreadable", "symlink"].contains(code):
            // Every item was one that cannot be sent; this is the first's.
            alert.informativeText = TransferText.failed(code)
        case "files_off":
            alert.informativeText = DeviceSharingText.filesOff(device)
        default:
            alert.informativeText = String(localized: "Check that \(device) is connected, then try again.")
        }
        alert.runModal()
    }
}
