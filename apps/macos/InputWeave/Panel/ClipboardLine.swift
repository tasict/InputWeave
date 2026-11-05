import SwiftUI

/// One quiet line about the clipboard, shown for ten minutes after it last
/// moved or was held back.
struct ClipboardLine: View {
    @Environment(AgentModel.self) private var model

    var body: some View {
        TimelineView(.periodic(from: .now, by: 30)) { context in
            if let text = ClipboardText.line(model, now: context.date) {
                Label {
                    Text(text)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize(horizontal: false, vertical: true)
                } icon: {
                    Image(systemName: "doc.on.clipboard").foregroundStyle(.secondary)
                }
            }
        }
    }
}

enum ClipboardText {
    static let shownFor: TimeInterval = 10 * 60

    @MainActor
    static func line(_ model: AgentModel, now: Date) -> String? {
        guard model.status.clipboardSync, model.status.hasLastClipboard else { return nil }
        let activity = model.status.lastClipboard
        let at = Date(timeIntervalSince1970: Double(activity.atUnixMs) / 1000)
        guard now.timeIntervalSince(at) < shownFor else { return nil }
        switch activity.skipped {
        case "sensitive":
            return String(localized: "The clipboard came from a password manager, so it was not synced.")
        case "too_large":
            return String(localized: "The clipboard is over 8 MB, so it was not synced. Use Send Files instead.")
        case "":
            break
        default:
            return nil
        }
        let when = at.formatted(.relative(presentation: .named))
        let kind = kind(activity.kinds)
        if activity.toDeviceID == model.thisDeviceID {
            return String(localized: "Clipboard: synced from \(model.name(of: activity.fromDeviceID)) \(when) (\(kind))")
        }
        return String(localized: "Clipboard: sent to \(model.name(of: activity.toDeviceID)) \(when) (\(kind))")
    }

    static func kind(_ kinds: [String]) -> String {
        if kinds.contains("image") { return String(localized: "image") }
        if kinds.contains("rich_text") { return String(localized: "formatted text") }
        return String(localized: "text")
    }
}
