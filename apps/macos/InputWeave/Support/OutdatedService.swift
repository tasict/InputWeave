import Foundation

/// Whether the background service that answered is not the one this app
/// carries. Installing a new build replaces the app while the previous
/// service keeps running; the new app then restarts it, once per launch,
/// and launchd runs the agent in the new bundle.
enum OutdatedService {
    /// The key scripts/embed-agent.sh writes the bundled agent's SHA-256
    /// under, in the LaunchAgent plist.
    static let digestKey = "InputWeaveAgentDigest"

    /// Outdated when the service speaks another IPC version, reports
    /// another version than the app, or reports another build. Every
    /// development build has the same version, so the SHA-256 of the
    /// agent's executable tells them apart. A service too old to report a
    /// digest, or an app without one, is judged by the version alone.
    static func isOutdated(
        link: AgentModel.Link, agentVersion: String, appVersion: String,
        agentDigest: String, bundledDigest: String
    ) -> Bool {
        switch link {
        case .incompatible:
            return true
        case .connecting:
            return false
        case .connected:
            if !agentVersion.isEmpty && agentVersion != appVersion { return true }
            return !agentDigest.isEmpty && !bundledDigest.isEmpty
                && agentDigest.lowercased() != bundledDigest.lowercased()
        }
    }

    /// The bundled agent's digest from the LaunchAgent plist at `plist`,
    /// or "" without one.
    static func bundledDigest(plist: URL) -> String {
        NSDictionary(contentsOf: plist)?[digestKey] as? String ?? ""
    }
}
