import AppKit

/// The License page's confirmations, alerts on which Return and Escape
/// both keep things as they are (`ConfirmationAlert`).
@MainActor
enum LicenseAlerts {
    /// Remove License….
    static func removal() -> NSAlert {
        ConfirmationAlert.make(
            title: String(localized: "Remove the license from this computer?"),
            message: String(localized: "This frees one of your 5 places. Your other computers keep their license. You can enter the key again later, or apply it to this computer from a licensed one."),
            destructive: String(localized: "Remove License"), keep: String(localized: "Cancel"))
    }

    /// Revoke License… on the computer called `name`.
    static func revoke(name: String) -> NSAlert {
        ConfirmationAlert.make(
            title: LicenseText.revokeTitle(name), message: LicenseText.revokeMessage(name),
            destructive: String(localized: "Revoke License"), keep: String(localized: "Keep"))
    }
}
