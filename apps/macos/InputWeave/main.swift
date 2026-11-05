import Foundation

// scripts/package.sh checks that a build gets this far, past dyld loading
// its frameworks. It stops before the app starts: the app asks macOS about
// the background service at once, and macOS then takes this copy for the
// one the service belongs to, a build product about to be deleted.
if CommandLine.arguments.contains("--version") {
    print(Bundle.main.shortVersion)
    exit(0)
}
// Settings from before the bundle identifier changed (v0.1.0), once. Not
// for the uninstaller (--unregister-service, which only unregisters) and
// not under XCTest, so neither writes the app's settings.
if !CommandLine.arguments.contains("--unregister-service")
    && ProcessInfo.processInfo.environment["XCTestConfigurationFilePath"] == nil
{
    EarlierPreferences.copy()
}
InputWeaveApp.main()
