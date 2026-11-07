import CoreLocation
import CoreWLAN
import Foundation
import Observation

/// Reads the Wi-Fi name for the background service while it recognizes
/// networks by their Wi-Fi name too (`WifiNames` decides when). macOS lets
/// an app read it only with location access, so turning the switch on asks
/// for that first; the app asks for nothing before. The name is never
/// written to the log.
@MainActor @Observable
final class WifiNameReporter: NSObject {
    /// The user turned the switch on and is being asked for location
    /// access; the switch shows on meanwhile.
    private(set) var asking = false
    /// Counts the times turning the switch on was refused at once, as the
    /// user denied access before: the switch shows off again each time.
    private(set) var refusals = 0

    @ObservationIgnored private let model: AgentModel
    /// Made only once the setting is on or the user turns it on, so an app
    /// that never needs it does not show up in Location Services.
    @ObservationIgnored private var manager: CLLocationManager?
    /// What the service was last told, and when, since this app connected
    /// to it and the setting was turned on (`WifiNames.kept`).
    @ObservationIgnored private var sent: WifiNames.Sent?
    /// The last reading for the network the service says this Mac is on,
    /// with an empty name when it is not on Wi-Fi; nil until one worked.
    /// A read that fails keeps it.
    @ObservationIgnored private var reading: WifiNames.Report?
    /// The network the reading is for.
    @ObservationIgnored private var readFor: String?
    @ObservationIgnored private var reader: Task<Void, Never>?
    /// Turning the setting off is on its way.
    @ObservationIgnored private var turningOff = false
    /// The manager told the app's access once, as it does soon after it is
    /// made; until then the access it gives may not be the real one yet,
    /// and is not taken for taken away.
    @ObservationIgnored private var accessKnown = false
    /// The app asked for access since it opened, the user or not.
    @ObservationIgnored private var askedThisLaunch = false

    init(model: AgentModel) {
        self.model = model
        super.init()
    }

    private var access: WifiNames.Access {
        guard let manager else { return .unknown }
        return switch manager.authorizationStatus {
        case .authorizedAlways, .authorizedWhenInUse: .granted
        case .denied, .restricted: .denied
        default: .unknown
        }
    }

    private func makeManager() -> CLLocationManager {
        if let manager { return manager }
        let manager = CLLocationManager()
        manager.delegate = self
        self.manager = manager
        return manager
    }

    /// Takes in what the model says now: reads and reports while the
    /// setting is on and the app may, asks once since it opened while
    /// access is not decided, and turns the setting off once access was
    /// denied (`WifiNames.step`).
    func update() {
        let connected = model.link == .connected
        // Told again after connecting, a restarted service too, and after
        // the setting is turned off and on again.
        sent = WifiNames.kept(sent, status: model.status, connected: connected)
        guard connected else {
            stopReading()
            return
        }
        let status = model.status
        if status.profilesSupported && status.wifiNames {
            _ = makeManager()
        }
        guard accessKnown, let manager else { return }
        switch WifiNames.step(status, access: access, asking: asking, askedThisLaunch: askedThisLaunch) {
        case .read:
            break
        case .ask:
            askedThisLaunch = true
            stopReading()
            manager.requestWhenInUseAuthorization()
            return
        case .turnOff:
            stopReading()
            turnOffInService()
            return
        case .idle, .wait:
            stopReading()
            return
        }
        let network = status.hasNetwork ? status.network.id : ""
        if reader == nil || network != readFor {
            // On another network, its name is read at once.
            reading = nil
            readFor = network
            startReading(for: network)
        }
        report()
    }

    /// The user turned the switch on: on at once when the app may read the
    /// name, else once the user allows it.
    func turnOn() {
        let manager = makeManager()
        switch WifiNames.start(access) {
        case .turnOn:
            setInService(true)
        case .ask:
            asking = true
            askedThisLaunch = true
            manager.requestWhenInUseAuthorization()
        case .refuse:
            // macOS asks only once; the switch goes back off.
            asking = false
            refusals += 1
        }
    }

    func turnOff() {
        asking = false
        setInService(false)
    }

    private func answered() {
        accessKnown = true
        guard asking else {
            update()
            return
        }
        guard let on = WifiNames.answered(access, serviceOn: model.status.wifiNames) else {
            if access != .unknown { asking = false }
            return
        }
        asking = false
        setInService(on)
    }

    private func setInService(_ on: Bool) {
        model.changeProfiles("Turning Wi-Fi names \(on ? "on" : "off")") { try await $0.setWifiNames(on) }
    }

    private func turnOffInService() {
        guard !turningOff else { return }
        turningOff = true
        Task {
            defer { turningOff = false }
            do {
                try await model.setWifiNames(false)
            } catch {
                ProfileText.log.notice("Turning Wi-Fi names off failed: \(error.localizedDescription, privacy: .public)")
            }
        }
    }

    /// Reads the name for `network` now and every few seconds, and tells
    /// the service after each reading what there is to tell, if anything.
    private func startReading(for network: String) {
        reader?.cancel()
        reader = Task { [weak self] in
            while !Task.isCancelled {
                let sighting = await Task.detached { Self.sighting() }.value
                guard let self, !Task.isCancelled else { return }
                if let ssid = WifiNames.ssid(sighting) {
                    self.reading = WifiNames.Report(networkID: network, ssid: ssid)
                }
                self.report()
                try? await Task.sleep(for: WifiNames.interval)
            }
        }
    }

    private func stopReading() {
        reader?.cancel()
        reader = nil
        reading = nil
        readFor = nil
    }

    private func report() {
        let now = Date.now
        guard let report = WifiNames.report(model.status, access: access, reading: reading, sent: sent, now: now)
        else { return }
        sent = WifiNames.Sent(report: report, at: now)
        Task {
            do {
                try await model.reportWifiName(networkID: report.networkID, ssid: report.ssid)
            } catch let refusal as AgentRefusal {
                ProfileText.log.notice("Reporting the Wi-Fi name was refused: \(refusal.code, privacy: .public)")
            } catch {
                // Lost the connection: told again once connected.
            }
        }
    }

    /// What the Wi-Fi interface says now: whether it is on and joined to a
    /// network, and the name it gives, which macOS gives only with
    /// location access.
    private nonisolated static func sighting() -> WifiNames.Sighting {
        guard let interface = CWWiFiClient.shared().interface() else {
            return WifiNames.Sighting(powered: false, associated: false, ssid: nil)
        }
        return WifiNames.Sighting(
            powered: interface.powerOn(), associated: interface.interfaceMode() != .none, ssid: interface.ssid())
    }
}

extension WifiNameReporter: CLLocationManagerDelegate {
    /// Called once the manager is made, and whenever the user changes the
    /// app's location access, on the main thread, where it was made.
    nonisolated func locationManagerDidChangeAuthorization(_: CLLocationManager) {
        Task { @MainActor in self.answered() }
    }
}
