import Foundation

/// How long ago something happened, as the app says it: "just now" for
/// under a minute, then whole minutes, hours and days.
enum AgoText {
    /// The unit a time ago is told in, with how many of it went by.
    enum Span: Equatable {
        case justNow
        case minutes(Int)
        case hours(Int)
        case days(Int)
    }

    /// The span `elapsed` seconds fall in, counting whole units. A negative
    /// time, from a clock set back, is just now.
    static func span(_ elapsed: TimeInterval) -> Span {
        let seconds = max(0, elapsed)
        if seconds < 60 { return .justNow }
        if seconds < 60 * 60 { return .minutes(Int(seconds / 60)) }
        if seconds < 24 * 60 * 60 { return .hours(Int(seconds / (60 * 60))) }
        return .days(Int(seconds / (24 * 60 * 60)))
    }

    /// The words for `elapsed` seconds ago, such as "2 hours ago";
    /// `bundle` holds the translations (the tests pick a language with it).
    static func text(_ elapsed: TimeInterval, bundle: Bundle = .main) -> String {
        // One or many by the rules of the language the words are in, which
        // the user's own locale may not be: English words on a Mac set to
        // a language InputWeave does not speak.
        let locale = Locale(identifier: bundle.preferredLocalizations.first ?? "en")
        return switch span(elapsed) {
        case .justNow: String(localized: "just now", bundle: bundle, locale: locale)
        case .minutes(let count): String(localized: "\(count) minutes ago", bundle: bundle, locale: locale)
        case .hours(let count): String(localized: "\(count) hours ago", bundle: bundle, locale: locale)
        case .days(let count): String(localized: "\(count) days ago", bundle: bundle, locale: locale)
        }
    }

    /// The words for the time from `since` to `now`.
    static func text(since: Date, now: Date, bundle: Bundle = .main) -> String {
        text(now.timeIntervalSince(since), bundle: bundle)
    }
}
