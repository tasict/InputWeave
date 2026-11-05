import Foundation

/// What the app says about relative mode, the movements sent as such to a
/// game on another computer (docs/architecture.md §6.4): what the HUD tells when it starts or ends, and
/// the panel's line while it is on. The background service says why it
/// ended (`Status.last_relative_end`); this only picks the words, and keeps
/// what begins or ends by itself from being told over and over.
enum RelativeText {
    typealias EndReason = Inputweave_App_V1_RelativeEndReason

    /// Relative mode as one status shows it.
    struct State: Equatable {
        /// The computer in relative mode; empty while it is off.
        var device = ""
        /// It began because an app there holds the pointer.
        var automatic = false
        /// How many times it ended, and where and why the last time.
        var ends: UInt64 = 0
        var endDevice = ""
        var endReason = EndReason.unspecified

        init(
            device: String = "", automatic: Bool = false, ends: UInt64 = 0, endDevice: String = "",
            endReason: EndReason = .unspecified
        ) {
            self.device = device
            self.automatic = automatic
            self.ends = ends
            self.endDevice = endDevice
            self.endReason = endReason
        }

        init(_ status: AgentStatus) {
            self.init(
                device: status.relativeMouse, automatic: status.relativeMouseAutomatic, ends: status.relativeEnds,
                endDevice: status.lastRelativeEnd.deviceID, endReason: status.lastRelativeEnd.reason)
        }
    }

    /// News about relative mode for the HUD.
    enum Note: Equatable {
        /// It began on `device`: by itself, as an app there holds the
        /// pointer, when `automatic`.
        case entered(device: String, automatic: Bool)
        /// It ended on `device`, for `reason`.
        case ended(device: String, reason: EndReason)

        var device: String {
            switch self {
            case .entered(let device, _), .ended(let device, _): device
            }
        }

        /// Whether it began or ended by itself, as an app took or let go of
        /// the pointer, rather than by the user's doing.
        var automatic: Bool {
            switch self {
            case .entered(_, let automatic): automatic
            case .ended(_, let reason): reason == .released
            }
        }

        /// Whether the HUD tells it: not when it ended as the keyboard and
        /// mouse came back or sharing was paused, which say so themselves.
        var told: Bool {
            switch self {
            case .entered: true
            case .ended(_, let reason): [.turnedOff, .autoOff, .jumped, .released].contains(reason)
            }
        }
    }

    /// What changed from `before` to `after`, if anything: having ended
    /// and begun again in between, it began. Fewer ends than before is a
    /// restarted service, only remembered.
    static func change(from before: State, to after: State) -> Note? {
        if after.ends < before.ends {
            return nil
        }
        let ended = after.ends > before.ends
        if !after.device.isEmpty && (after.device != before.device || ended) {
            return .entered(device: after.device, automatic: after.automatic)
        }
        if ended {
            return .ended(device: after.endDevice, reason: after.endReason)
        }
        return nil
    }

    /// Keeps notes about relative mode beginning or ending by itself from
    /// repeating, as a game takes and lets go of the pointer each time its
    /// menu opens and closes: for each computer the first of each kind is
    /// told, then at most one every `interval`. What the user did is always
    /// told.
    struct Quieting {
        static let interval: TimeInterval = 60

        private struct Told {
            var entered = false
            var released = false
            var at: Date?
        }

        private var told: [String: Told] = [:]

        /// Whether to tell `note` at `now`; if so, it counts as told.
        mutating func allows(_ note: Note, at now: Date) -> Bool {
            guard note.automatic else { return true }
            var told = self.told[note.device] ?? Told()
            let entered = if case .entered = note { true } else { false }
            let first = entered ? !told.entered : !told.released
            let due = told.at.map { now.timeIntervalSince($0) >= Self.interval } ?? true
            guard first || due else { return false }
            if entered {
                told.entered = true
            } else {
                told.released = true
            }
            told.at = now
            self.told[note.device] = told
            return true
        }
    }

    /// Follows relative mode from status to status for the HUD. The first
    /// status after connecting, to a restarted service too, is only
    /// remembered.
    struct News {
        private var last: State?
        private var quieting = Quieting()

        /// Takes in relative mode from the latest status, nil while the
        /// background service is out of reach, and returns what to tell.
        /// `preempted`: the HUD shows something else for this status, so
        /// nothing here counts as told.
        mutating func update(_ state: State?, at now: Date, preempted: Bool = false) -> Note? {
            defer { last = state }
            guard let state, let last, let note = change(from: last, to: state), note.told, !preempted,
                quieting.allows(note, at: now)
            else { return nil }
            return note
        }
    }

    /// The HUD's words for `note`, one it tells.
    @MainActor
    static func message(_ note: Note, model: AgentModel) -> String {
        switch note {
        case .entered(let device, _):
            let name = model.name(of: device)
            if let shortcut = model.relativeShortcut {
                return String(localized: "Relative mouse: the cursor stays on “\(name)”. Press \(shortcut) to end it.")
            }
            return String(localized: "Relative mouse: the cursor stays on “\(name)”. Press \(model.takeBackShortcut) to take back control.")
        case .ended(let device, let reason):
            if reason == .released {
                return String(localized: "“\(model.name(of: device))” let go of the mouse, so it moves as usual again.")
            }
            return String(localized: "Relative mouse ended.")
        }
    }

    /// The HUD's words for the relative mode hotkey reaching a computer
    /// whose InputWeave has no relative mode.
    static func unsupported(_ name: String) -> String {
        String(localized: "InputWeave on “\(name)” doesn't support relative mouse.")
    }
}
