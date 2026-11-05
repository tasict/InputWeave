import Foundation

/// The app's own settings, in its user defaults.
enum Preferences {
    static let showsMenuBarIcon = "showsMenuBarIcon"
    static let finishedSetup = "finishedSetup"
    /// How many times the take-back hint has been shown after a crossing.
    static let takeBackHints = "takeBackHints"
    /// The ids of transfer problems dismissed from the panel.
    static let dismissedTransfers = "dismissedTransfers"
    /// Whether the edge lights up where the cursor crosses to or from
    /// another computer; on unless set to false.
    static let showsEdgeGlow = "showsEdgeGlow"
    /// Where development builds kept the license outbox before it moved to
    /// the keychain; only deleted (`KeychainLicenseOutboxStore`).
    static let licenseOutbox = "licenseOutbox"
}

/// Settings kept under the app's bundle identifier before 2026-10-27,
/// `com.inputweave.InputWeave` (v0.1.0). The first launch under the new
/// one copies those the app still uses that it has not set itself; the
/// old domain is left alone, and the uninstaller removes both.
enum EarlierPreferences {
    static let domain = "com.inputweave.InputWeave"
    /// Set once the copy is made, so it is made once.
    static let copied = "copiedEarlierPreferences"
    /// The app's settings and the user's choices about updates (Sparkle's).
    /// Not `finishedSetup`: to macOS the background service is a new
    /// program, whose permissions have to be granted again, so setup runs
    /// again to walk through them.
    static let keys = [
        Preferences.showsMenuBarIcon, Preferences.takeBackHints, Preferences.dismissedTransfers,
        "SUEnableAutomaticChecks", "SUAutomaticallyUpdate",
    ]

    /// What to copy of the old domain's `earlier` settings: the keys the
    /// app uses that are not set (`isSet`) yet.
    static func toCopy(earlier: [String: Any], isSet: (String) -> Bool) -> [String: Any] {
        earlier.filter { keys.contains($0.key) && !isSet($0.key) }
    }

    /// Copies them into `defaults` from `earlierDomain`, unless that was
    /// done before.
    static func copy(into defaults: UserDefaults = .standard, from earlierDomain: String = Self.domain) {
        guard defaults.object(forKey: copied) == nil else { return }
        let earlier = defaults.persistentDomain(forName: earlierDomain) ?? [:]
        for (key, value) in toCopy(earlier: earlier, isSet: { defaults.object(forKey: $0) != nil }) {
            defaults.set(value, forKey: key)
        }
        defaults.set(true, forKey: copied)
    }
}
