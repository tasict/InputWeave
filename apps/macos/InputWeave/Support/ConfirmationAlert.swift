import AppKit

/// An alert that asks before something that can't simply be undone: the
/// destructive button first, red, and the button that keeps things as they
/// are second. Return and Escape both choose the second, so a key pressed
/// by accident changes nothing.
@MainActor
enum ConfirmationAlert {
    /// The alert, its destructive button without a key equivalent and its
    /// other button with Return. Escape is `present`'s, as a button has one
    /// key equivalent.
    static func make(title: String, message: String, destructive: String, keep: String) -> NSAlert {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = message
        let button = alert.addButton(withTitle: destructive)
        button.hasDestructiveAction = true
        button.keyEquivalent = ""
        alert.addButton(withTitle: keep).keyEquivalent = "\r"
        return alert
    }

    /// Asks with an alert from `make`; `confirmed` runs once the user chose
    /// its destructive button.
    static func ask(_ alert: NSAlert, confirmed: @escaping @MainActor () -> Void) {
        present(alert, escape: alert.buttons.last) { response in
            if response == .alertFirstButtonReturn { confirmed() }
        }
    }

    /// On the settings window, where the user chose what it asks about, or
    /// on its own when that is gone. While it is up, Escape in it clicks
    /// `escape`.
    static func present(
        _ alert: NSAlert, escape: NSButton? = nil, then handle: @escaping @MainActor (NSApplication.ModalResponse) -> Void
    ) {
        let monitor = escape.flatMap { button in
            NSEvent.addLocalMonitorForEvents(matching: .keyDown) { event in
                guard event.window === alert.window, isEscape(event) else { return event }
                button.performClick(nil)
                return nil
            }
        }
        let finish: @MainActor (NSApplication.ModalResponse) -> Void = { response in
            if let monitor { NSEvent.removeMonitor(monitor) }
            handle(response)
        }
        if let window = NSApp.keyWindow ?? NSApp.mainWindow, window.isVisible {
            alert.beginSheetModal(for: window) { response in
                MainActor.assumeIsolated { finish(response) }
            }
        } else {
            NSApp.activate()
            finish(alert.runModal())
        }
    }

    /// The Escape key alone (kVK_Escape), as `present` takes it.
    static func isEscape(_ event: NSEvent) -> Bool {
        event.type == .keyDown && event.keyCode == 53
            && event.modifierFlags.intersection(.deviceIndependentFlagsMask).isEmpty
    }
}
