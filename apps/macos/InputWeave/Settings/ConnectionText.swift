import Foundation

/// How a connected device is described in the device list and the
/// inspector.
enum ConnectionText {
    /// "Connected", with the round-trip time once the service has measured
    /// it; "Screen locked" instead while the device's screen is locked, and
    /// "Screen locked, can't take input" when input from here cannot reach
    /// it then, so the cursor stays off it. "Can't take input" when no
    /// input from here reaches it at all now, locked or not, as a Linux
    /// desktop that has not allowed remote control. "Paused sharing" when
    /// its user paused sharing there, which keeps the cursor off it too.
    static func connected(
        rttMs: UInt32, screenLocked: Bool = false, lockScreenInput: Bool = true, inputUnavailable: Bool = false,
        sharingPaused: Bool = false
    ) -> String {
        if sharingPaused {
            guard rttMs > 0 else { return String(localized: "Paused sharing") }
            return String(localized: "Paused sharing · \(Int(rttMs)) ms")
        }
        if inputUnavailable && !screenLocked {
            guard rttMs > 0 else { return String(localized: "Can't take input") }
            return String(localized: "Can't take input · \(Int(rttMs)) ms")
        }
        if screenLocked && (!lockScreenInput || inputUnavailable) {
            guard rttMs > 0 else { return String(localized: "Screen locked, can't take input") }
            return String(localized: "Screen locked, can't take input · \(Int(rttMs)) ms")
        }
        if screenLocked {
            guard rttMs > 0 else { return String(localized: "Screen locked") }
            return String(localized: "Screen locked · \(Int(rttMs)) ms")
        }
        guard rttMs > 0 else { return String(localized: "Connected") }
        return String(localized: "Connected · \(Int(rttMs)) ms")
    }

    /// The symbol beside it in the device list, by the same priority: a
    /// pause when its user paused sharing, a lock while its screen is
    /// locked, else the dot of a connected device.
    static func symbol(screenLocked: Bool, sharingPaused: Bool) -> String {
        if sharingPaused { return "pause.circle" }
        return screenLocked ? "lock.fill" : "circle.fill"
    }

    /// The symbol after a computer's name on the canvas and the mini map:
    /// a lock while its screen is locked, a moon while it is offline and
    /// can be woken (`WakeText.showsMoon`), else none. Only a connected
    /// computer has a locked screen, and only an offline one can be woken.
    static func nameSymbol(screenLocked: Bool, wakeable: Bool) -> String? {
        if screenLocked { return "lock.fill" }
        return wakeable ? WakeText.moon : nil
    }
}
