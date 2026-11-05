import Foundation

/// "1:58": what is left until a Unix time in milliseconds, never negative.
func countdown(until unixMilliseconds: UInt64, now: Date) -> String {
    let deadline = Date(timeIntervalSince1970: Double(unixMilliseconds) / 1000)
    let seconds = max(0, Int(deadline.timeIntervalSince(now).rounded(.up)))
    return String(format: "%d:%02d", seconds / 60, seconds % 60)
}

/// The six digits of a pairing code in two groups of three, as on screen.
func groupedCode(_ code: String) -> String {
    guard code.count == 6 else { return code }
    return "\(code.prefix(3)) \(code.suffix(3))"
}
