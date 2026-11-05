import CoreServices
import Foundation

/// What a cold launch does about a menu bar icon the user hid. Opening the
/// app brings the icon back, with the main window once setup is done (setup
/// opens its own window), as reopening a running app does; a launch at
/// login leaves it hidden.
struct LaunchReveal: Equatable {
    var showsIcon = false
    var showsMainWindow = false

    static func decide(launchedAtLogin: Bool, iconShown: Bool, finishedSetup: Bool) -> LaunchReveal {
        guard !launchedAtLogin, !iconShown else { return LaunchReveal() }
        return LaunchReveal(showsIcon: true, showsMainWindow: finishedSetup)
    }

    /// Whether `event`, the Apple event that opened the app, says macOS
    /// opened it as a login item. Without one, the user opened it.
    static func launchedAtLogin(_ event: NSAppleEventDescriptor?) -> Bool {
        guard let event, event.eventClass == kCoreEventClass, event.eventID == kAEOpenApplication else {
            return false
        }
        return event.paramDescriptor(forKeyword: keyAEPropData)?.enumCodeValue == keyAELaunchedAsLogInItem
    }
}
