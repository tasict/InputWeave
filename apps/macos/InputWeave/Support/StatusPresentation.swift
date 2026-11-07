import Foundation

/// What the app puts on screen by itself, from the background service's
/// status, however the app was launched: the service opens it with
/// `--background` whenever it needs it (`LaunchReveal`, which decides only
/// the menu bar icon and the main window). The pairing code panel shows
/// while another computer pairs with this one (`PairingCodePanel`), and a
/// notification asks about each file waiting for an answer
/// (`TransferNotifier`), including one waiting when the app connects,
/// which is why the service opened it. The license's work shows nothing.
enum StatusPresentation {
    static func showsPairingCode(_ pairing: PairingState) -> Bool {
        pairing.phase == .showCode
    }

    /// A file offered to this computer that waits for the user's answer.
    static func asksAbout(_ transfer: FileTransfer) -> Bool {
        !transfer.outgoing && !transfer.clipboard && transfer.state == .waitingForAnswer
    }
}
