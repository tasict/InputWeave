import Observation

/// Sparkle's updater as the views see it, so only InputWeaveApp.swift
/// imports Sparkle (the snapshot tests do not link it). `AppActions.updater`
/// holds one when the build names an update feed.
@MainActor @Observable
final class Updater {
    /// Whether Sparkle looks for a new version on its own. Sparkle keeps
    /// the choice, and asks for it itself on the app's second launch, so
    /// General reads it again when it appears (`reload`).
    private(set) var checksAutomatically: Bool

    @ObservationIgnored private let load: @MainActor () -> Bool
    @ObservationIgnored private let store: @MainActor (Bool) -> Void
    @ObservationIgnored private let check: @MainActor () -> Void

    init(
        load: @escaping @MainActor () -> Bool,
        store: @escaping @MainActor (Bool) -> Void,
        check: @escaping @MainActor () -> Void
    ) {
        self.load = load
        self.store = store
        self.check = check
        checksAutomatically = load()
    }

    func setChecksAutomatically(_ on: Bool) {
        store(on)
        checksAutomatically = load()
    }

    func reload() {
        let current = load()
        if current != checksAutomatically { checksAutomatically = current }
    }

    /// Looks for a new version now, showing Sparkle's window.
    func checkForUpdates() {
        check()
    }
}
