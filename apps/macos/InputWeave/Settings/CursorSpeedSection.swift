import SwiftUI

/// How fast the cursor moves on another computer while this Mac controls it
/// (`SetDeviceSpeed`), in the inspector. Only this Mac uses it; the other
/// computer is not told. The slider sends the speed it is let go at, or a
/// moment after the keys or VoiceOver moved it, and then shows the
/// service's again: a refused one goes back (`CursorSpeedEditor`).
struct CursorSpeedSection: View {
    @Environment(AgentModel.self) private var model
    let deviceID: String
    @State private var editor = CursorSpeedEditor()

    var body: some View {
        let thisDevice = deviceID == model.thisDeviceID
        if let service = CursorSpeed.shown(model.device(deviceID), thisDevice: thisDevice) {
            let percent = editor.shown(service, for: deviceID)
            Section {
                VStack(alignment: .leading, spacing: 4) {
                    HStack(spacing: 8) {
                        Text("Cursor speed")
                        Spacer()
                        if CursorSpeed.showsReset(percent) {
                            Button("Reset") {
                                editor.reset(service: service, on: deviceID, send: sendSpeed)
                            }
                            .buttonStyle(.link)
                        }
                        Text(verbatim: CursorSpeed.label(percent))
                            .monospacedDigit()
                            .frame(minWidth: 38, alignment: .trailing)
                    }
                    Slider(
                        value: Binding(
                            get: { Double(percent) },
                            set: { position in
                                editor.moved(to: position, service: service, on: deviceID, send: sendSpeed)
                            }),
                        in: CursorSpeed.range,
                        step: CursorSpeed.step
                    ) {
                        Text("Cursor speed")
                    } onEditingChanged: { editing in
                        if editing {
                            editor.began(on: deviceID, send: sendSpeed)
                        } else {
                            editor.ended(service: service, on: deviceID, send: sendSpeed)
                        }
                    }
                    .labelsHidden()
                    .accessibilityValue(Text(verbatim: CursorSpeed.label(percent)))
                    CursorSpeedTicks()
                    Text(CursorSpeed.description(model.name(of: deviceID)))
                        .foregroundStyle(.secondary)
                        .font(.callout)
                        .fixedSize(horizontal: false, vertical: true)
                }
            }
            .onAppear { editor.show(deviceID, send: sendSpeed) }
            // Another computer shown: a speed still waiting goes now, for
            // the one it was chosen for.
            .onChange(of: deviceID) { _, now in editor.show(now, listed: listed, send: sendSpeed) }
            .onChange(of: service) { _, now in editor.settle(service: now, for: deviceID) }
            .onChange(of: model.status.devices.map(\.id), initial: true) { _, ids in
                editor.keep(Set(ids))
            }
            .onDisappear { editor.leave(listed: listed, send: sendSpeed) }
        }
    }

    /// The computers the service lists now.
    private var listed: Set<String> {
        Set(model.status.devices.map(\.id))
    }

    /// Sends a speed; a refusal leaves the service's speed, which the slider
    /// shows again.
    private var sendSpeed: CursorSpeedSend {
        { [model] id, percent in try? await model.setDeviceSpeed(id, percent: percent) }
    }
}

/// The speeds `CursorSpeed.ticks` names, under the slider's own tick marks
/// (one a step) at theirs.
private struct CursorSpeedTicks: View {
    /// How far the slider keeps its knob's middle, and its first and last
    /// tick marks, from either end.
    static let inset: CGFloat = 10

    var body: some View {
        GeometryReader { geometry in
            let usable = geometry.size.width - 2 * Self.inset
            ForEach(CursorSpeed.ticks, id: \.self) { tick in
                Text(verbatim: CursorSpeed.label(tick))
                    .font(.caption)
                    .monospacedDigit()
                    .foregroundStyle(.secondary)
                    .fixedSize()
                    .position(x: Self.inset + usable * CursorSpeed.fraction(tick), y: 7)
            }
        }
        .frame(height: 14)
        .accessibilityHidden(true)
    }
}
