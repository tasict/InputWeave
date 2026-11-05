import SwiftUI

/// Adding a device from this computer: pick a
/// nearby computer or type its address, then type the code it shows.
struct PairingFlow: View {
    @Environment(AgentModel.self) private var model
    /// Called when the pairing succeeded and the user moves on.
    var onPaired: () -> Void = {}
    /// Whether this flow started the pairing in progress, so a pairing
    /// shown here was not left over from before.
    @State private var started = false
    @State private var address = ""
    @State private var startError: String?

    var body: some View {
        let phase = started ? model.pairing.phase : .idle
        Group {
            switch phase {
            case .connecting:
                ProgressView("Connecting…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            case .enterCode, .verifying:
                CodeEntry()
            case .paired:
                VStack(spacing: 12) {
                    Image(systemName: "checkmark.circle.fill").font(.system(size: 44)).foregroundStyle(.green)
                    Text("Paired with \(model.pairing.peerName)").font(.title3)
                    Button("Continue") { started = false; onPaired() }
                        .keyboardShortcut(.defaultAction)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            case .failed:
                VStack(spacing: 12) {
                    Image(systemName: "xmark.circle").font(.system(size: 44)).foregroundStyle(.secondary)
                    Text(PairingText.failure(model.pairing)).multilineTextAlignment(.center)
                    Button("Start Over") { started = false }
                        .keyboardShortcut(.defaultAction)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            default:
                chooser
            }
        }
    }

    private var chooser: some View {
        VStack(alignment: .leading, spacing: 12) {
            let candidates = model.nearby.filter { !$0.paired }
            if candidates.isEmpty {
                ContentUnavailableView {
                    Label("No Other Computers Found", systemImage: "antenna.radiowaves.left.and.right")
                } description: {
                    Text("Install and open InputWeave on another computer on this network, and it appears here.")
                }
            } else {
                List(candidates, id: \.id) { device in
                    HStack {
                        Image(systemName: OSLabel(os: device.os).symbol)
                            .foregroundStyle(.secondary)
                            .frame(width: 22)
                        VStack(alignment: .leading) {
                            Text(device.name)
                            Text(OSLabel(os: device.os).name).font(.caption).foregroundStyle(.secondary)
                        }
                        Spacer()
                        if device.pairable {
                            Button("Pair") { start { try await model.startPairing(deviceID: device.id) } }
                        } else {
                            Text("Not accepting new devices").font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
                .frame(minHeight: 140)
            }
            HStack {
                TextField("IP address", text: $address, prompt: Text(verbatim: "192.168.1.20"))
                    .onSubmit(pairByAddress)
                Button("Add by IP Address", action: pairByAddress)
                    .disabled(address.trimmingCharacters(in: .whitespaces).isEmpty)
            }
            if let startError {
                Text(startError).foregroundStyle(.red).font(.callout)
            }
        }
    }

    private func pairByAddress() {
        let address = address.trimmingCharacters(in: .whitespaces)
        guard !address.isEmpty else { return }
        start { try await model.startPairing(address: address) }
    }

    private func start(_ request: @escaping () async throws -> Void) {
        Task {
            do {
                try await request()
                startError = nil
                started = true
            } catch let refusal as AgentRefusal {
                startError = PairingText.startFailure(refusal.code)
            } catch {
                startError = String(localized: "The background service is not responding.")
            }
        }
    }
}

/// Six boxes for the code shown on the other computer. The sixth digit
/// submits it.
private struct CodeEntry: View {
    @Environment(AgentModel.self) private var model
    @Environment(\.accessibilityReduceMotion) private var reduceMotion
    @State private var digits = ""
    @State private var shake: CGFloat = 0
    @FocusState private var focused: Bool

    var body: some View {
        VStack(spacing: 16) {
            Text("Enter the code shown on “\(model.pairing.peerName)”")
                .font(.title3)
                .multilineTextAlignment(.center)
            ZStack {
                TextField("", text: $digits)
                    .focused($focused)
                    .opacity(0.01)
                    .frame(width: 1, height: 1)
                    .onChange(of: digits) { _, new in
                        let cleaned = String(new.filter(\.isNumber).prefix(6))
                        if cleaned != new { digits = cleaned }
                        if cleaned.count == 6 { submit(cleaned) }
                    }
                    .accessibilityLabel(Text("Pairing code"))
                HStack(spacing: 8) {
                    ForEach(0..<6, id: \.self) { index in
                        let character = index < digits.count
                            ? String(digits[digits.index(digits.startIndex, offsetBy: index)]) : ""
                        Text(character)
                            .font(.system(size: 28, weight: .medium).monospacedDigit())
                            .frame(width: 40, height: 52)
                            .background(RoundedRectangle(cornerRadius: 8).fill(.background))
                            .overlay(
                                RoundedRectangle(cornerRadius: 8)
                                    .strokeBorder(index == digits.count && focused ? Color.accentColor : Color.secondary.opacity(0.4), lineWidth: index == digits.count && focused ? 2 : 1))
                        if index == 2 { Spacer().frame(width: 10) }
                    }
                }
                .contentShape(Rectangle())
                .onTapGesture { focused = true }
                .accessibilityHidden(true)
            }
            .modifier(Shake(amount: shake))
            if model.pairing.phase == .verifying {
                ProgressView().controlSize(.small)
            } else if model.pairing.error == "wrong_code" {
                Text("That code is not right. You can try \(model.pairing.attemptsLeft) more times.")
                    .foregroundStyle(.red)
                    .font(.callout)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .onAppear { focused = true }
    }

    private func submit(_ code: String) {
        Task {
            let state = try? await model.submitPairingCode(code)
            if state?.phase == .enterCode {
                digits = ""
                withAnimation(reduceMotion ? nil : .linear(duration: 0.35)) { shake += 1 }
            }
        }
    }
}

/// The sheet from Devices or the panel.
struct AddDeviceSheet: View {
    @Environment(AgentModel.self) private var model
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            Text("Add Device").font(.title2.weight(.semibold)).padding([.top, .horizontal], 20)
            PairingFlow {
                AppActions.navigation.page = .arrangement
                dismiss()
            }
            .padding(20)
            Divider()
            HStack {
                Spacer()
                Button("Cancel") {
                    Task { try? await model.cancelPairing() }
                    dismiss()
                }
                .keyboardShortcut(.cancelAction)
            }
            .padding(12)
        }
        .frame(width: 480, height: 420)
    }
}

/// What to tell the user about a pairing that did not work.
enum PairingText {
    static func failure(_ state: PairingState) -> String {
        let name = state.peerName.isEmpty ? String(localized: "The other computer") : state.peerName
        return switch state.error {
        case "too_many_attempts": String(localized: "Pairing was cancelled. Start again.")
        case "expired": String(localized: "The code has expired. Start again.")
        case "rejected": String(localized: "\(name) declined the pairing.")
        case "not_allowed": String(localized: "\(name) does not accept new devices.")
        case "busy": String(localized: "\(name) is pairing with another computer. Try again in a moment.")
        case "id_conflict": String(localized: "\(name) uses the same device ID as a computer you already have.")
        case "incompatible": String(localized: "\(name) runs a version of InputWeave that cannot pair with this one. Update both.")
        case "unreachable": String(localized: "\(name) cannot be reached. Check that both computers are on the same network.")
        case "cancelled": String(localized: "Pairing was cancelled.")
        default: String(localized: "Pairing did not work. Start again.")
        }
    }

    static func startFailure(_ code: String) -> String {
        switch code {
        case "busy": String(localized: "Another pairing is in progress.")
        case "invalid_address": String(localized: "That address cannot be found.")
        case "unreachable", "unknown_device": String(localized: "That computer cannot be reached.")
        default: String(localized: "Pairing could not start.")
        }
    }
}
