import Foundation
import Observation

/// What the inspector's cursor speed row offers for another computer
/// (`SetDeviceSpeed`): how fast the cursor moves there while this Mac
/// controls it, in percent of how the mouse moves it here.
enum CursorSpeed {
    static let normal = 100
    static let range: ClosedRange<Double> = 50...300
    static let step = 10.0
    /// The values marked under the slider.
    static let ticks = [50, 100, 200, 300]

    /// The speed to show for `device`, or nil where the row is not offered:
    /// this Mac, and a service from before cursor speeds, which says 0.
    static func shown(_ device: DeviceStatus?, thisDevice: Bool) -> Int? {
        guard !thisDevice, let device, device.speedPercent != 0 else { return nil }
        return Int(device.speedPercent)
    }

    /// The speed a position of the slider stands for: the nearest step, in
    /// range.
    static func snapped(_ value: Double) -> Int {
        let stepped = (value / step).rounded() * step
        return Int(min(max(stepped, range.lowerBound), range.upperBound))
    }

    /// The speed as shown beside the slider and read out, e.g. "150%".
    static func label(_ percent: Int) -> String {
        "\(percent)%"
    }

    /// Reset is offered unless the speed is the normal one.
    static func showsReset(_ percent: Int) -> Bool {
        percent != normal
    }

    /// Where `percent` sits along the slider, from 0 to 1.
    static func fraction(_ percent: Int) -> Double {
        (Double(percent) - range.lowerBound) / (range.upperBound - range.lowerBound)
    }

    static func description(_ device: String) -> String {
        String(localized: "While the cursor is on \(device), mouse movement is multiplied by this. It applies only when this computer controls it, and not to relative mouse for games.")
    }
}

/// The speed being chosen on the slider, shown in place of the service's
/// until the service answers for it. The service's value is what counts:
/// refused, the slider goes back to it. Each speed sent is numbered; until
/// the answer to the latest one comes, the slider shows the latest speed
/// sent, and a new choice is weighed against it, not against the service's
/// older speed, so choosing the service's speed again still goes. Answers
/// to earlier sends change nothing.
struct CursorSpeedDraft: Equatable {
    /// Chosen, and not sent yet (or still held).
    private(set) var value: Int?
    private(set) var editing = false
    /// The number of the latest send.
    private(set) var sent: UInt64 = 0
    /// The latest speed sent, until its answer comes.
    private(set) var inFlight: Int?

    /// What a new choice is weighed against: the latest speed on its way,
    /// or else the service's.
    private func baseline(_ service: Int) -> Int {
        inFlight ?? service
    }

    /// What the slider shows, the service having `service`.
    func shown(_ service: Int) -> Int {
        value ?? baseline(service)
    }

    /// The slider began moving under the pointer.
    mutating func began() {
        editing = true
    }

    /// The slider moved to `position`, the service having `service`.
    mutating func moved(to position: Double, service: Int) {
        value = CursorSpeed.snapped(position)
        settle(service: service)
    }

    /// The pointer let go of the slider: the speed to send, if it differs
    /// from what the service has or is getting.
    mutating func ended(service: Int) -> Int? {
        editing = false
        settle(service: service)
        return value
    }

    /// A speed chosen without the pointer (the keyboard, VoiceOver, Reset),
    /// to send unless the slider is still held.
    func pending(service: Int) -> Int? {
        guard !editing, let value, value != baseline(service) else { return nil }
        return value
    }

    /// `percent` is sent: the number its answer comes back with.
    mutating func sending(_ percent: Int) -> UInt64 {
        sent += 1
        inFlight = percent
        if !editing && value == percent {
            value = nil
        }
        return sent
    }

    /// The service answered send `number`, either way. Only the latest one
    /// counts: the slider shows the service's speed again, but for a speed
    /// chosen and not sent yet.
    mutating func answered(_ number: UInt64) {
        if number == sent {
            inFlight = nil
        }
    }

    /// The service has `service`: a speed equal to what it has or is
    /// getting, no longer held, is no longer a draft.
    mutating func settle(service: Int) {
        if !editing && value == baseline(service) {
            value = nil
        }
    }
}

/// Sends a speed for a computer, and returns once the service answered.
typealias CursorSpeedSend = @MainActor (_ deviceID: String, _ percent: Int) async -> Void

/// The cursor speed row's state: the computer it is for, the speed under
/// the hand and on its way for each computer, and a speed chosen with the
/// keys waiting to go. A reference, so an answer that comes after another
/// computer is shown still finds the slider of the computer it was for,
/// and leaves the other one's alone; coming back to that computer shows
/// the speed still on its way there.
@MainActor @Observable
final class CursorSpeedEditor {
    /// The computer the slider is for; none while the row is not shown.
    private(set) var deviceID: String?
    /// Each computer's draft, kept while another is shown.
    private(set) var drafts: [String: CursorSpeedDraft] = [:]
    /// Each computer's speed as the service last had it here.
    @ObservationIgnored private var services: [String: Int] = [:]
    /// How long the keys must leave the slider alone before its speed goes.
    @ObservationIgnored let delay: Duration
    @ObservationIgnored private var waiting: Task<Void, Never>?
    @ObservationIgnored private var waitingFor: (deviceID: String, percent: Int)?

    init(delay: Duration = .milliseconds(300)) {
        self.delay = delay
    }

    /// The draft of the computer the slider is for.
    var draft: CursorSpeedDraft {
        deviceID.flatMap { drafts[$0] } ?? CursorSpeedDraft()
    }

    /// What the slider shows for `id`, the service having `service`.
    func shown(_ service: Int, for id: String) -> Int {
        drafts[id]?.shown(service) ?? service
    }

    /// The row shows `id`: a speed still waiting for the computer shown
    /// before, or still held on the slider, goes now, for that computer,
    /// while it is among the computers `listed` (all of them if not given);
    /// one no longer listed (unpaired) drops it instead.
    func show(_ id: String, listed: Set<String>? = nil, send: @escaping CursorSpeedSend) {
        guard id != deviceID else { return }
        flush(listed: listed, send: send)
        deviceID = id
    }

    /// The row went away: what was waiting goes now, unless the computer is
    /// no longer among those `listed`.
    func leave(listed: Set<String>, send: @escaping CursorSpeedSend) {
        flush(listed: listed, send: send)
        deviceID = nil
    }

    /// The pointer pressed the slider: nothing goes until it lets go.
    func began(on id: String, send: @escaping CursorSpeedSend) {
        show(id, send: send)
        cancelWaiting()
        drafts[id, default: CursorSpeedDraft()].began()
    }

    /// The slider moved to `position`; moved without the pointer, the speed
    /// goes once the slider has been left alone for `delay`.
    func moved(to position: Double, service: Int, on id: String, send: @escaping CursorSpeedSend) {
        show(id, send: send)
        services[id] = service
        drafts[id, default: CursorSpeedDraft()].moved(to: position, service: service)
        if let percent = drafts[id]?.pending(service: service) {
            sendLater(percent, to: id, send: send)
        } else {
            cancelWaiting()
        }
    }

    /// The pointer let go of the slider: the speed it was let go at goes.
    func ended(service: Int, on id: String, send: @escaping CursorSpeedSend) {
        show(id, send: send)
        services[id] = service
        if let percent = drafts[id, default: CursorSpeedDraft()].ended(service: service) {
            cancelWaiting()
            self.send(percent, to: id, send: send)
        }
    }

    /// Reset: back to the normal speed, at once.
    func reset(service: Int, on id: String, send: @escaping CursorSpeedSend) {
        show(id, send: send)
        services[id] = service
        cancelWaiting()
        drafts[id, default: CursorSpeedDraft()].moved(to: Double(CursorSpeed.normal), service: service)
        if let percent = drafts[id]?.pending(service: service) {
            self.send(percent, to: id, send: send)
        }
    }

    /// Only the computers in `ids` are still trusted: the others lose what
    /// was kept for them, and a speed waiting for one of them goes nowhere,
    /// so paired again it shows the service's speed.
    func keep(_ ids: Set<String>) {
        drafts = drafts.filter { ids.contains($0.key) }
        services = services.filter { ids.contains($0.key) }
        if let waitingFor, !ids.contains(waitingFor.deviceID) {
            cancelWaiting()
        }
    }

    /// The service has `service` for `id` now.
    func settle(service: Int, for id: String) {
        services[id] = service
        drafts[id]?.settle(service: service)
    }

    private func send(_ percent: Int, to id: String, send: @escaping CursorSpeedSend) {
        let number = drafts[id, default: CursorSpeedDraft()].sending(percent)
        Task {
            await send(id, percent)
            // That computer's slider, whichever is shown now.
            drafts[id]?.answered(number)
        }
    }

    private func sendLater(_ percent: Int, to id: String, send: @escaping CursorSpeedSend) {
        cancelWaiting()
        waitingFor = (id, percent)
        let delay = delay
        waiting = Task { [weak self] in
            try? await Task.sleep(for: delay)
            guard !Task.isCancelled, let self else { return }
            self.waiting = nil
            self.waitingFor = nil
            self.send(percent, to: id, send: send)
        }
    }

    private func flush(listed: Set<String>?, send: @escaping CursorSpeedSend) {
        if let id = deviceID, let listed, !listed.contains(id) {
            // Unpaired: nothing is sent for it, and nothing is kept.
            cancelWaiting()
            drafts[id] = nil
            services[id] = nil
            return
        }
        if let waitingFor {
            cancelWaiting()
            self.send(waitingFor.percent, to: waitingFor.deviceID, send: send)
        } else if let id = deviceID, drafts[id]?.editing == true, let service = services[id],
                  let held = drafts[id]?.ended(service: service) {
            // Held when another computer was chosen: what it was dragged to
            // counts, as if let go.
            self.send(held, to: id, send: send)
        }
    }

    private func cancelWaiting() {
        waiting?.cancel()
        waiting = nil
        waitingFor = nil
    }
}
