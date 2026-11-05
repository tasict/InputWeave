import AppKit
import SwiftUI

/// The About window (⋯ › About InputWeave): the version, whether this
/// computer is licensed, the copyright and the licenses, with the
/// third-party notices one click away.
struct AboutView: View {
    @Environment(\.openWindow) private var openWindow
    /// The values come from the app's bundle; snapshots pass their own.
    var icon: NSImage?
    var version = Bundle.main.shortVersion
    var build = Bundle.main.buildNumber
    var copyright = Bundle.main.copyright

    var body: some View {
        VStack(spacing: 6) {
            Image(nsImage: icon ?? NSApp.applicationIconImage)
                .resizable()
                .frame(width: 96, height: 96)
                .accessibilityHidden(true)
            Text(verbatim: "InputWeave")
                .font(.title2.weight(.semibold))
            Text("Version \(version) (\(build))")
                .foregroundStyle(.secondary)
                .textSelection(.enabled)
            AboutLicenseLine()
                .padding(.top, 6)
            VStack(spacing: 2) {
                Text(verbatim: copyright)
                Text("Free and open source under the GNU GPL, version 3 or later")
                    // Wraps rather than ending in an ellipsis at this width.
                    .fixedSize(horizontal: false, vertical: true)
            }
            .font(.callout)
            .foregroundStyle(.secondary)
            .padding(.top, 8)
            Button("Acknowledgments…") { openWindow(id: "acknowledgments") }
                .padding(.top, 10)
        }
        .multilineTextAlignment(.center)
        .padding(24)
        .frame(width: 360)
    }
}

/// The notices of the third-party material InputWeave includes, as
/// bundled in THIRD_PARTY_NOTICES.md. Shown as plain text, since the
/// license texts are preformatted.
struct AcknowledgmentsView: View {
    var text = AcknowledgmentsView.bundledNotices

    var body: some View {
        ScrollView {
            Text(verbatim: text ?? String(localized: "The notices are missing from this copy of InputWeave. Reinstall it to restore them."))
                .font(.system(.callout, design: .monospaced))
                .textSelection(.enabled)
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(20)
        }
        .frame(minWidth: 520, minHeight: 360)
    }

    static var bundledNotices: String? {
        Bundle.main.url(forResource: "THIRD_PARTY_NOTICES", withExtension: "md")
            .flatMap { try? String(contentsOf: $0, encoding: .utf8) }
    }
}

extension Bundle {
    var buildNumber: String {
        infoDictionary?["CFBundleVersion"] as? String ?? "0"
    }

    var copyright: String {
        infoDictionary?["NSHumanReadableCopyright"] as? String ?? ""
    }
}
