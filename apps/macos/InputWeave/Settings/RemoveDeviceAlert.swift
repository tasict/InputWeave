import AppKit

/// Remove Device's confirmation, an alert. While this computer is
/// licensed, for a computer on its license it also offers to revoke the
/// license there (checked to begin with), or, for one on InputWeave 1.0,
/// says how to free its place instead. Return and Escape both cancel
/// (`ConfirmationAlert`). A notice follows when the license could not be
/// revoked.
@MainActor
enum RemoveDeviceAlert {
    /// How wide the license's part under the message is.
    static let accessoryWidth: CGFloat = 230

    /// The confirmation, and its checkbox when it has one.
    static func confirmation(name: String, offer: LicenseText.RemoveOffer) -> (alert: NSAlert, revoke: NSButton?) {
        let alert = ConfirmationAlert.make(
            title: String(localized: "Remove \(name)?"),
            message: String(localized: "This computer stops trusting it. To use it again, pair it again."),
            destructive: String(localized: "Remove"), keep: String(localized: "Cancel"))
        switch offer {
        case .none:
            return (alert, nil)
        case .revoke:
            let box = NSButton(checkboxWithTitle: String(localized: "Also revoke its license"), target: nil, action: nil)
            box.state = .on
            // Under the checkbox's title, not its box.
            let indent = box.cell.map { $0.titleRect(forBounds: CGRect(origin: .zero, size: box.fittingSize)).minX } ?? 20
            let line = note(String(localized: "Frees one of your 5 places; it goes back to trial."), width: accessoryWidth - indent)
            let indented = NSStackView(views: [line])
            indented.edgeInsets = NSEdgeInsets(top: 0, left: indent, bottom: 0, right: 0)
            alert.accessoryView = accessory([box, indented])
            return (alert, box)
        case .legacy:
            alert.accessoryView = accessory([note(LicenseText.legacyRemoveNote(name), width: accessoryWidth)])
            return (alert, nil)
        }
    }

    /// Asks; `remove` runs once the user chose Remove, told whether to
    /// revoke the license too.
    static func present(name: String, offer: LicenseText.RemoveOffer, remove: @escaping @MainActor (_ revoke: Bool) -> Void) {
        let (alert, box) = confirmation(name: name, offer: offer)
        ConfirmationAlert.ask(alert) { remove(box?.state == .on) }
    }

    /// The computer called `name` was removed, but its license could not
    /// be revoked, for `notice`.
    static func notRevoked(name: String, notice: LicenseText.RemoveNotice, portal: URL?) -> NSAlert {
        let alert = NSAlert()
        alert.messageText = LicenseText.notRevokedTitle(notice, name: name)
        alert.informativeText = LicenseText.notRevokedMessage(notice)
        alert.addButton(withTitle: String(localized: "OK"))
        if portal != nil, notice.offersPortal {
            alert.addButton(withTitle: String(localized: "Manage Computers…"))
        }
        return alert
    }

    static func presentNotRevoked(name: String, notice: LicenseText.RemoveNotice, portal: URL?) {
        ConfirmationAlert.present(notRevoked(name: name, notice: notice, portal: portal)) { response in
            if response == .alertSecondButtonReturn, let portal {
                NSWorkspace.shared.open(portal)
            }
        }
    }

    private static func note(_ text: String, width: CGFloat) -> NSTextField {
        let label = NSTextField(wrappingLabelWithString: text)
        label.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        label.textColor = .secondaryLabelColor
        label.preferredMaxLayoutWidth = width
        return label
    }

    private static func accessory(_ views: [NSView]) -> NSView {
        let stack = NSStackView(views: views)
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 4
        stack.frame.size = NSSize(width: accessoryWidth, height: stack.fittingSize.height)
        return stack
    }
}
