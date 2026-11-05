import AppKit
import SwiftUI

/// The panel's files going and waiting, and
/// problems until they are dismissed.
struct TransferList: View {
    @Environment(AgentModel.self) private var model
    @Environment(\.openWindow) private var openWindow
    private let limit = 4

    var body: some View {
        let rows = TransferText.panelRows(model.transfers.transfers, dismissed: AppActions.navigation.dismissedTransfers)
        if !rows.isEmpty {
            VStack(alignment: .leading, spacing: 8) {
                Text("Transfers")
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.secondary)
                ForEach(rows.prefix(limit), id: \.transferID) { transfer in
                    TransferRow(transfer: transfer, dismissible: true, failuresShown: 3)
                }
                if rows.count > limit {
                    Button("\(rows.count - limit) more…") { AppActions.showMain(openWindow, page: .files) }
                        .buttonStyle(.link)
                        .font(.callout)
                }
            }
        }
    }
}

/// One item, a file or a folder: its name and the other computer, what a
/// folder holds, progress or state, a folder's files that did not go, and
/// what can be done about it.
struct TransferRow: View {
    @Environment(AgentModel.self) private var model
    let transfer: FileTransfer
    /// Finished problems can be dismissed from the panel.
    var dismissible = false
    /// Finished transfers can be removed from the history on the File
    /// Transfers page.
    var removable = false
    /// How many of a folder's failures to name; the panel has room for few.
    var failuresShown = 100
    @State private var showsFailures: Bool

    init(transfer: FileTransfer, dismissible: Bool = false, removable: Bool = false, failuresShown: Int = 100, showsFailures: Bool = false) {
        self.transfer = transfer
        self.dismissible = dismissible
        self.removable = removable
        self.failuresShown = failuresShown
        _showsFailures = State(initialValue: showsFailures)
    }

    var body: some View {
        let device = model.name(of: transfer.deviceID)
        HStack(alignment: .top, spacing: 8) {
            Image(systemName: transfer.folder ? "folder" : transfer.outgoing ? "arrow.up.doc" : "arrow.down.doc")
                .foregroundStyle(.secondary)
                .frame(width: 18)
                .accessibilityLabel(transfer.outgoing ? Text("Sending") : Text("Receiving"))
            VStack(alignment: .leading, spacing: 3) {
                Text(verbatim: TransferText.title(transfer, device: device))
                    .lineLimit(1)
                    .truncationMode(.middle)
                if transfer.folder {
                    Text(verbatim: TransferText.folderSummary(transfer))
                        .font(.caption)
                        .monospacedDigit()
                        .foregroundStyle(.secondary)
                }
                if let fraction = TransferText.fraction(transfer) {
                    ProgressView(value: fraction)
                        .controlSize(.small)
                }
                Text(verbatim: TransferText.detail(transfer, device: device))
                    .font(.caption)
                    .monospacedDigit()
                    .foregroundStyle(TransferText.isProblem(transfer) ? AnyShapeStyle(Color.red) : AnyShapeStyle(.secondary))
                    .fixedSize(horizontal: false, vertical: true)
                if transfer.state.isFinished && transfer.filesFailed > 0 {
                    failures
                }
                if !transfer.outgoing && transfer.state == .waitingForAnswer {
                    HStack {
                        Button("Accept") { answer(true) }
                        Button("Decline") { answer(false) }
                    }
                    .controlSize(.small)
                    .padding(.top, 2)
                }
            }
            Spacer(minLength: 0)
            trailing
        }
    }

    /// The folder's files that did not go, and why, on request.
    private var failures: some View {
        DisclosureGroup(isExpanded: $showsFailures) {
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(transfer.failures.prefix(failuresShown).enumerated()), id: \.offset) { _, failure in
                    VStack(alignment: .leading, spacing: 1) {
                        Text(verbatim: failure.relativePath)
                            .lineLimit(1)
                            .truncationMode(.middle)
                        Text(verbatim: TransferText.reason(failure.reason, outgoing: transfer.outgoing))
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                let more = Int(transfer.filesFailed) - min(transfer.failures.count, failuresShown)
                if more > 0 {
                    Text("And \(more) more").foregroundStyle(.secondary)
                }
            }
            .font(.caption)
            .padding(.top, 2)
        } label: {
            Text("Details").font(.caption)
        }
    }

    @ViewBuilder
    private var trailing: some View {
        if !transfer.state.isFinished {
            Button {
                Task { try? await model.cancelTransfer(transfer.transferID) }
            } label: {
                Image(systemName: "xmark.circle.fill")
            }
            .buttonStyle(.borderless)
            .foregroundStyle(.secondary)
            .help(Text("Cancel"))
            .accessibilityLabel(Text("Cancel \(transfer.name)"))
        } else {
            // Files pasted here were fetched into a folder of the agent's;
            // where they were pasted is the app's to know.
            let showable = transfer.state == .done && !transfer.path.isEmpty
                && !(transfer.clipboard && !transfer.outgoing)
            HStack(spacing: 8) {
                if showable {
                    Button {
                        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: transfer.path)])
                    } label: {
                        Image(systemName: "magnifyingglass")
                    }
                    .buttonStyle(.borderless)
                    .foregroundStyle(.secondary)
                    .help(Text("Show in Finder"))
                    .accessibilityLabel(Text("Show \(transfer.name) in Finder"))
                }
                if removable {
                    Button(action: remove) {
                        Image(systemName: "xmark")
                    }
                    .buttonStyle(.borderless)
                    .foregroundStyle(.secondary)
                    .help(Text("Remove from History"))
                    .accessibilityLabel(Text("Remove \(transfer.name) from History"))
                }
                if dismissible && TransferText.isProblem(transfer) {
                    Button {
                        AppActions.navigation.dismissTransfer(transfer.transferID)
                    } label: {
                        Image(systemName: "xmark")
                    }
                    .buttonStyle(.borderless)
                    .foregroundStyle(.secondary)
                    .help(Text("Dismiss"))
                    .accessibilityLabel(Text("Dismiss"))
                }
            }
        }
    }

    /// Takes the transfer off the history; its file stays.
    private func remove() {
        Task { try? await model.removeTransfer(transfer.transferID) }
    }

    private func answer(_ accept: Bool) {
        Task { try? await model.answerFileOffer(transfer.transferID, accept: accept) }
    }
}
