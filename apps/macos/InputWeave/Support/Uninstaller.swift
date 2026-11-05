import AppKit

/// Uninstall InputWeave…: removes what the installer package's removal
/// choice removes, by running the bundled `uninstall.sh` with administrator
/// privileges, then quits.
@MainActor
enum Uninstaller {
    static func confirmAndRun() {
        let alert = NSAlert()
        alert.alertStyle = .warning
        alert.messageText = String(localized: "Uninstall InputWeave?")
        alert.informativeText = String(
            localized: "Sharing stops, and InputWeave, its background service, its permissions, logs and settings are removed from this Mac. Received files are kept. InputWeave then quits.")
        alert.showsSuppressionButton = true
        let deletePairings = alert.suppressionButton!
        deletePairings.title = String(localized: "Also delete pairings; other computers will have to pair with this Mac again")
        deletePairings.state = .off
        alert.addButton(withTitle: String(localized: "Uninstall")).hasDestructiveAction = true
        alert.addButton(withTitle: String(localized: "Cancel"))
        // Return cancels; the destructive button has to be clicked.
        alert.buttons[0].keyEquivalent = ""
        alert.buttons[1].keyEquivalent = "\r"
        NSApp.activate()
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        run(purge: deletePairings.state == .on)
    }

    private static func run(purge: Bool) {
        guard let script = Bundle.main.path(forResource: "uninstall", ofType: "sh") else {
            fail(String(localized: "uninstall.sh is missing from the app."))
            return
        }
        // The script leaves this process alone (--caller) and deletes this
        // bundle (--app), which may not be the one in /Applications.
        var command = [
            "/bin/sh", script, "--app", Bundle.main.bundlePath,
            "--caller", String(ProcessInfo.processInfo.processIdentifier),
        ]
        if purge { command.append("--purge") }
        let prompt = String(localized: "InputWeave wants to remove itself and its background service.")
        switch AdministratorShell.run(command, prompt: prompt) {
        case .done:
            break
        case .cancelled:
            return
        case .failed(let message):
            fail(message)
            return
        }
        if let id = Bundle.main.bundleIdentifier {
            UserDefaults.standard.removePersistentDomain(forName: id)
        }
        // Quit without writing preferences or window state back.
        exit(0)
    }

    private static func fail(_ message: String) {
        let alert = NSAlert()
        alert.alertStyle = .critical
        alert.messageText = String(localized: "InputWeave could not be uninstalled")
        alert.informativeText = message
        alert.runModal()
    }
}

/// Runs a command as root through AppleScript's `do shell script … with
/// administrator privileges`, which asks for an administrator's password.
@MainActor
enum AdministratorShell {
    enum Outcome {
        case done
        /// The password prompt was cancelled.
        case cancelled
        /// The command could not start or exited non-zero; its error output.
        case failed(String)
    }

    static func run(_ argv: [String], prompt: String) -> Outcome {
        let shell = argv.map { "'" + $0.replacingOccurrences(of: "'", with: "'\\''") + "'" }.joined(separator: " ")
        let source =
            "do shell script \(appleScriptString(shell)) with prompt \(appleScriptString(prompt)) with administrator privileges"
        var error: NSDictionary?
        NSAppleScript(source: source)?.executeAndReturnError(&error)
        guard let error else { return .done }
        // -128: the password prompt was cancelled.
        if error[NSAppleScript.errorNumber] as? Int == -128 { return .cancelled }
        return .failed(error[NSAppleScript.errorMessage] as? String ?? String(localized: "Unknown error."))
    }

    private static func appleScriptString(_ text: String) -> String {
        "\"" + text.replacingOccurrences(of: "\\", with: "\\\\").replacingOccurrences(of: "\"", with: "\\\"") + "\""
    }
}
