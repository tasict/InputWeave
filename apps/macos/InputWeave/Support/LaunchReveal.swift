import CoreServices
import Foundation

/// What a cold launch does about a menu bar icon the user hid. Opening the
/// app brings the icon back, with the main window once setup is done (setup
/// opens its own window), as reopening a running app does; a launch at
/// login leaves it hidden, and so does a launch to perform a service
/// (Finder's Services › Send with InputWeave), which shows only its own
/// panel, and one by the background service, which shows only what its
/// status calls for.
struct LaunchReveal: Equatable {
    var showsIcon = false
    var showsMainWindow = false

    /// How the app was launched.
    enum Launch: Equatable {
        /// The user opened it: from the Finder, the Dock, Launchpad or `open`.
        case opened
        case atLogin
        /// To perform a service, or otherwise not a default launch.
        case forService
        /// By the background service, with `--background`, for what it
        /// needs the app for: the pairing code or a file offer, which show
        /// by themselves (`StatusPresentation`), or the license's work,
        /// which shows nothing.
        case inBackground
    }

    /// The argument the background service opens the app with.
    static let backgroundArgument = "--background"

    static func decide(launch: Launch, iconShown: Bool, finishedSetup: Bool) -> LaunchReveal {
        guard launch == .opened, !iconShown else { return LaunchReveal() }
        return LaunchReveal(showsIcon: true, showsMainWindow: finishedSetup)
    }

    /// How the app was launched, from `event`, the Apple event that opened
    /// it, and `isDefault`, `NSApplication.launchIsDefaultUserInfoKey` of
    /// the launch notification: false when the app was launched to perform
    /// a service (or to open a file, or to restore windows saved at quit,
    /// where windows show anyway); and `arguments`, the command line, which
    /// has `backgroundArgument` when the background service opened it.
    /// Without an event the user opened it.
    static func launch(_ event: NSAppleEventDescriptor?, isDefault: Bool, arguments: [String] = []) -> Launch {
        if arguments.dropFirst().contains(backgroundArgument) { return .inBackground }
        let reason: OSType? = if let event, event.eventClass == kCoreEventClass, event.eventID == kAEOpenApplication {
            event.paramDescriptor(forKeyword: keyAEPropData)?.enumCodeValue
        } else {
            nil
        }
        if reason == keyAELaunchedAsLogInItem { return .atLogin }
        if reason == keyAELaunchedAsServiceItem || !isDefault { return .forService }
        return .opened
    }
}
