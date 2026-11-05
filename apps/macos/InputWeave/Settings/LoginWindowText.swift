import Foundation

/// Why the background service refused to let other computers control this
/// Mac at the login window (`SetLoginWindowAccess`), for the General page.
enum LoginWindowText {
    static func refusal(_ code: String) -> String {
        switch code {
        case "not_installed":
            return String(localized: "The login window is set up by InputWeave's installer package, and this copy wasn't installed with it. Install InputWeave from the package to use this.")
        case "custom_paths":
            return String(localized: "InputWeave keeps its settings in another folder here, set in its configuration file, and the login window only reads the usual one.")
        case "no_lock_screen_input":
            return String(localized: "Other computers can't type on this Mac's lock screen, so they can't at the login window either.")
        case "unsupported":
            return String(localized: "This Mac can't be controlled at the login window.")
        default:
            return String(localized: "The setting could not be changed.")
        }
    }
}
