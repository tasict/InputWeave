import AppKit
import SwiftUI

/// The main window's pages, in the order its sidebar lists them; License
/// is the last, in a section of its own (`LicenseSidebarRow`).
enum Page: String, CaseIterable, Identifiable {
    case general, devices, arrangement, files, troubleshooting, license

    var id: String { rawValue }

    /// The pages of the sidebar's first section.
    static let listed: [Page] = [.general, .devices, .arrangement, .files, .troubleshooting]

    var title: LocalizedStringKey {
        switch self {
        case .arrangement: "Arrangement"
        case .devices: "Devices"
        case .files: "File Transfers"
        case .general: "General"
        case .troubleshooting: "Troubleshooting"
        case .license: "License"
        }
    }

    var symbol: String {
        switch self {
        case .arrangement: "rectangle.3.group"
        case .devices: "desktopcomputer"
        case .files: "arrow.up.arrow.down.circle"
        case .general: "gearshape"
        case .troubleshooting: "stethoscope"
        case .license: "key"
        }
    }
}

/// Navigation shared by the panel, the main window and the controllers.
@MainActor @Observable
final class Navigation {
    var page: Page = .general
    /// Show the Add Device sheet when the main window appears.
    var addingDevice = false
    /// The computer whose name field the inspector should put the cursor
    /// in ("Rename…" on the arrangement page); cleared once it has.
    var renamingDevice: String?
    /// Transfer problems the user dismissed from the panel, kept across
    /// launches while the history lists them (`TransferText.dismissed`).
    var dismissedTransfers: Set<String> = Set(UserDefaults.standard.stringArray(forKey: Preferences.dismissedTransfers) ?? []) {
        didSet {
            guard dismissedTransfers != oldValue else { return }
            UserDefaults.standard.set(dismissedTransfers.sorted(), forKey: Preferences.dismissedTransfers)
        }
    }

    func dismissTransfer(_ id: String) {
        dismissedTransfers.insert(id)
    }
    /// The main window was asked for from outside the app's own UI while
    /// the menu bar icon, whose label opens windows, was not there; the
    /// label opens it once it appears (see `AppActions.reopen`).
    var mainWindowPending = false
}

@MainActor
enum AppActions {
    static let navigation = Navigation()
    /// Sparkle's updater; nil when this build has no update feed.
    static var updater: Updater?

    static func showMain(_ openWindow: OpenWindowAction, page: Page? = nil, addDevice: Bool = false) {
        if let page { navigation.page = page }
        if addDevice { navigation.addingDevice = true }
        navigation.mainWindowPending = false
        openWindow(id: "main")
        NSApp.activate()
    }

    /// Shows the main window from outside the app's own UI: the app was
    /// opened again, or a notification was clicked. A hidden menu bar icon
    /// comes back, as its label is what opens windows.
    static func reopen(page: Page? = nil) {
        if let page { navigation.page = page }
        navigation.mainWindowPending = true
        UserDefaults.standard.set(true, forKey: Preferences.showsMenuBarIcon)
        NotificationCenter.default.post(name: .showMainWindow, object: nil)
    }

    static func showAbout(_ openWindow: OpenWindowAction) {
        openWindow(id: "about")
        NSApp.activate()
    }

    /// Stops sharing until the next login or the next time the app opens:
    /// the background service is unregistered, so launchd stops it.
    static func quit(service: BackgroundService) {
        Task {
            try? await service.stopUntilReopened()
            NSApp.terminate(nil)
        }
    }

    static func hideMenuBarIcon() {
        UserDefaults.standard.set(false, forKey: Preferences.showsMenuBarIcon)
    }
}

extension Notification.Name {
    /// Asks the menu bar label, which can open windows, to show the main
    /// window.
    static let showMainWindow = Notification.Name("cc.ctsite.inputweave.showMainWindow")
}
