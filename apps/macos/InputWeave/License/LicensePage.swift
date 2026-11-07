import AppKit
import SwiftUI

/// The settings sidebar's License row, in a section of its own at the
/// end of the list: License, with "Trial" or the woven label, on every
/// page of the window. Apart from this row, the License page and About,
/// the apps say nothing of the license.
struct LicenseSidebarRow: View {
    @Environment(LicenseController.self) private var license

    var body: some View {
        Label {
            HStack(spacing: 4) {
                Text(Page.license.title)
                Spacer(minLength: 4)
                if license.isLicensed {
                    WovenLabel(size: .small)
                } else {
                    TrialCapsule()
                }
            }
        } icon: {
            Image(systemName: Page.license.symbol)
        }
    }
}

/// Settings › License.
struct LicensePage: View {
    @Environment(LicenseController.self) private var license

    var body: some View {
        @Bindable var license = license
        Form {
            if license.isLicensed {
                LicensedSections()
            } else {
                TrialSections()
            }
        }
        .formStyle(.grouped)
        .sheet(isPresented: $license.entering) {
            EnterLicenseKeySheet()
        }
        .onAppear { license.surfaceAppeared() }
        .onDisappear { license.surfaceDisappeared() }
    }
}

/// Opens one of the license's web pages; disabled in a build without it.
private struct LinkButton: View {
    @Environment(\.openURL) private var openURL
    let title: LocalizedStringKey
    let url: URL?
    var prominent = false

    var body: some View {
        if prominent {
            button.buttonStyle(.borderedProminent)
        } else {
            button
        }
    }

    private var button: some View {
        Button(title) { if let url { openURL(url) } }
            .disabled(url == nil)
    }
}

private struct TrialSections: View {
    @Environment(LicenseController.self) private var license
    @Environment(AgentModel.self) private var model

    var body: some View {
        let record = license.license
        let state = record.state
        let seatsFullHere = LicenseText.seatsFullHere(record)
        Section {
            VStack(alignment: .leading, spacing: 10) {
                Text("Trial")
                    .font(.title2.weight(.semibold))
                Text("Every feature works without a license, for as long as you like. A license replaces “Trial” with “Licensed” here and in About InputWeave.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if seatsFullHere {
                    Notice(symbol: "exclamationmark.triangle.fill", color: .orange,
                           text: Text("Your license is already used on 5 computers, so this computer stays in trial. Every feature still works. To license this one, free a computer you no longer use in Polar’s customer portal."))
                } else if state == .invalid {
                    Notice(symbol: "exclamationmark.triangle.fill", color: .orange,
                           text: Text("The license on this computer is no longer valid. It may have been refunded, or this computer was removed from it in Polar’s customer portal."))
                } else if state == .unavailable {
                    Notice(symbol: "exclamationmark.triangle.fill", color: .orange,
                           text: Text("License information can’t be read right now."))
                }
                if license.removalRefused {
                    Notice(symbol: "exclamationmark.triangle.fill", color: .orange,
                           text: Text("InputWeave couldn’t remove the license on this computer."))
                }
                if license.removing {
                    HStack(spacing: 8) {
                        ProgressView().controlSize(.small)
                        Text("Removing the license from this computer…").foregroundStyle(.secondary)
                    }
                }
                if license.removalPending {
                    Notice(symbol: "exclamationmark.triangle.fill", color: .orange,
                           text: Text("InputWeave couldn’t finish removing the license on this computer. It keeps trying."))
                }
                if license.seatNotFreedYet {
                    Notice(symbol: "info.circle.fill", color: .secondary,
                           text: Text("Polar couldn’t be reached, so this computer’s place on the license isn’t free yet. InputWeave frees it once Polar can be reached, or you can free it in Polar’s customer portal."))
                }
                Text("US$10, paid once, for up to 5 of your computers.")
                if let name = LicenseText.licensedElsewhere(record) {
                    Notice(symbol: "info.circle.fill", color: .blue, text: Text(verbatim: LicenseText.licensedElsewhereNote(name)))
                }
                HStack(spacing: 8) {
                    if seatsFullHere && !license.removalPending {
                        LinkButton(title: "Manage Computers…", url: license.links.customerPortal, prominent: true)
                        Button("Try Again") { license.tryAgain() }
                    } else if state != .unavailable {
                        LinkButton(title: "Buy a License…", url: license.links.checkout, prominent: true)
                        Button("Enter License Key…") { license.beginEntering() }
                        if license.seatNotFreedYet {
                            LinkButton(title: "Manage Computers…", url: license.links.customerPortal)
                        }
                    }
                }
                .padding(.top, 2)
            }
            .padding(.vertical, 6)
        }
        let waiting = LicenseText.waitingToApply(model.status)
        if !waiting.isEmpty {
            WaitingToApplySection(rows: waiting)
        }
        Section {
            Step(number: 1, text: Text("Buy a license from Polar and get a license key."))
            Step(number: 2, text: Text("Enter the key on one of your computers."))
            Step(number: 3, text: Text("On that computer’s License page, choose the other computers that use it, up to 5 in all."))
        } header: {
            Text("How the license works")
        } footer: {
            Text("Payment and receipts are handled by Polar.")
                .foregroundStyle(.secondary)
                .font(.callout)
        }
    }
}

private struct LicensedSections: View {
    @Environment(LicenseController.self) private var license
    @Environment(AgentModel.self) private var model

    var body: some View {
        let record = license.license
        let email = LicenseText.maskedEmail(record.customerEmail)
        Section {
            HStack(spacing: 18) {
                WovenLabel(size: .large, sews: license.justActivated, onSewn: { license.labelSewn() })
                VStack(alignment: .leading, spacing: 3) {
                    if email.isEmpty {
                        Text("Licensed").font(.headline)
                    } else {
                        Text("Licensed to \(email)").font(.headline)
                    }
                    if record.sourceDeviceName.isEmpty {
                        Text("Thank you for buying InputWeave.").foregroundStyle(.secondary)
                    } else {
                        Text("The license came from \(record.sourceDeviceName). Thank you for buying InputWeave.")
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .padding(.vertical, 6)
        }
        if record.applySupported {
            ComputersOnLicense()
        } else {
            EarlierComputersSection()
        }
        Section {
            if license.removalRefused {
                Notice(symbol: "exclamationmark.triangle.fill", color: .orange,
                       text: Text("InputWeave couldn’t remove the license on this computer."))
            }
            HStack(spacing: 8) {
                LinkButton(title: "Manage Computers…", url: license.links.customerPortal)
                Button("Remove License…") {
                    ConfirmationAlert.ask(LicenseAlerts.removal()) { license.remove() }
                }
            }
        }
    }
}

/// The computers on this license as a background service that does not
/// apply licenses lists them: this one and the paired ones licensed.
private struct EarlierComputersSection: View {
    @Environment(LicenseController.self) private var license
    @Environment(AgentModel.self) private var model

    var body: some View {
        let computers = LicenseText.computers(license.license, thisName: model.thisComputerName)
        Section {
            ForEach(computers) { computer in
                HStack(spacing: 10) {
                    Image(systemName: "desktopcomputer").foregroundStyle(.secondary).frame(width: 20)
                    Text(verbatim: computer.name)
                    Spacer()
                    Text(computer.thisComputer ? "This computer" : "Paired").foregroundStyle(.secondary)
                    Label("Licensed", systemImage: "checkmark")
                        .labelStyle(.titleAndIcon)
                        .foregroundStyle(.green)
                        .font(.callout)
                }
            }
        } header: {
            HStack {
                Text("Computers on this license")
                Spacer()
                Text("\(computers.count) of \(LicenseText.seats)").monospacedDigit()
            }
        } footer: {
            Text("Only computers paired with this one are listed. Polar’s customer portal lists all of them.")
                .foregroundStyle(.secondary)
                .font(.callout)
        }
    }
}

/// "Computers on this license": this computer and every computer it
/// trusts, each with how it stands on the license and what can be done:
/// apply the license there, revoke it, or take back applying it.
private struct ComputersOnLicense: View {
    @Environment(LicenseController.self) private var license
    @Environment(AgentModel.self) private var model

    var body: some View {
        let rows = LicenseText.rows(model.status, thisName: model.thisComputerName)
        let full = LicenseText.allPlacesUsed(rows)
        Section {
            ForEach(rows) { row in
                LicenseComputerRow(row: row, full: full) {
                    ConfirmationAlert.ask(LicenseAlerts.revoke(name: row.name)) {
                        Task { await license.revoke(row.id, activationID: row.activationID) }
                    }
                }
            }
        } header: {
            HStack {
                Text("Computers on this license")
                Spacer()
                Text("\(LicenseText.licensedCount(rows)) of \(LicenseText.seats)").monospacedDigit()
            }
        } footer: {
            VStack(alignment: .leading, spacing: 6) {
                if full {
                    Text("All 5 are in use. To apply the license to another computer, revoke it on one first.")
                }
                Text("Only computers you apply the license to here use it; newly paired computers don’t get it by themselves. Polar’s customer portal lists all of them.")
            }
            .foregroundStyle(.secondary)
            .font(.callout)
            .fixedSize(horizontal: false, vertical: true)
        }
        if LicenseText.noPlaces(rows) {
            Section {
                VStack(alignment: .leading, spacing: 10) {
                    Notice(symbol: "exclamationmark.triangle.fill", color: .orange,
                           text: Text("Polar says this license is already on 5 computers, maybe including some not paired with this one. Free one you no longer use in Polar’s customer portal, then apply again."))
                    LinkButton(title: "Manage Computers…", url: license.links.customerPortal)
                }
                .padding(.vertical, 4)
            }
        }
    }
}

/// A computer's icon and name on the License page, with what is said of it
/// under the name.
private struct ComputerName: View {
    let row: LicenseText.Row

    var body: some View {
        Image(systemName: OSLabel(os: row.os).symbol)
            .foregroundStyle(.secondary)
            .frame(width: 20)
            .accessibilityHidden(true)
        VStack(alignment: .leading, spacing: 2) {
            Text(verbatim: row.name)
            Group {
                switch row.reach {
                case .thisComputer: Text("This computer")
                case .offline: Text("Offline")
                case .offlineForNow: Text(OfflineText.status())
                case .connected: EmptyView()
                }
                if row.showsLegacyNote {
                    Text("InputWeave 1.0: you can revoke it here once it’s updated to 1.1. To free its place now, use Polar’s customer portal.")
                }
                if row.showsLegacyRefused {
                    Text("InputWeave 1.0 didn’t take the license. Update it to 1.1, then apply it again.")
                }
                if row.standing == .otherLicense {
                    Text("This computer uses a different license.")
                }
            }
            .font(.subheadline)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// While this computer is not licensed, the computers its license waits
/// to be applied to once it is licensed again; each can be taken back.
private struct WaitingToApplySection: View {
    @Environment(LicenseController.self) private var license
    let rows: [LicenseText.Row]

    var body: some View {
        Section("Waiting to Apply") {
            ForEach(rows) { row in
                HStack(spacing: 10) {
                    ComputerName(row: row)
                    Spacer(minLength: 12)
                    Text("Applies once this computer is licensed again")
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .fixedSize()
                    if row.action == .dontApply {
                        Button("Don’t Apply") { Task { await license.dontApply(row.id) } }
                            .disabled(license.applying.contains(row.id))
                            .fixedSize()
                    }
                }
                .padding(.vertical, 2)
            }
        }
    }
}

/// One computer on "Computers on this license".
private struct LicenseComputerRow: View {
    @Environment(LicenseController.self) private var license
    let row: LicenseText.Row
    /// Every place is taken or promised: Apply License is off.
    let full: Bool
    let revoke: () -> Void

    var body: some View {
        HStack(spacing: 10) {
            ComputerName(row: row)
            Spacer(minLength: 12)
            standing
                .font(.callout)
                .fixedSize()
            action
                .fixedSize()
        }
        .padding(.vertical, 2)
    }

    @ViewBuilder private var standing: some View {
        switch row.standing {
        case .thisComputer, .licensed:
            Label("Licensed", systemImage: "checkmark")
                .labelStyle(.titleAndIcon)
                .foregroundStyle(.green)
        case .trial:
            Text("Trial").foregroundStyle(.secondary)
        case .activating:
            HStack(spacing: 6) {
                ProgressView().controlSize(.small)
                Text("Activating…").foregroundStyle(.secondary)
            }
        case .waiting:
            Text("Offline. Applies when it connects").foregroundStyle(.secondary)
        case .noPlaces:
            Label("No places left", systemImage: "exclamationmark.triangle.fill")
                .labelStyle(.titleAndIcon)
                .foregroundStyle(.orange)
        case .otherLicense:
            Text("Another license").foregroundStyle(.secondary)
        }
    }

    @ViewBuilder private var action: some View {
        switch row.action {
        case .none:
            EmptyView()
        case .apply:
            Button("Apply License") { Task { await license.apply(row.id) } }
                .disabled(full || license.applying.contains(row.id))
        case .revoke:
            Button("Revoke License…", action: revoke)
                .disabled(license.revoking(row.id) || license.applying.contains(row.id))
        case .dontApply:
            Button("Don’t Apply") { Task { await license.dontApply(row.id) } }
                .disabled(license.applying.contains(row.id))
        }
    }
}

/// Enter License Key, a sheet over the License page.
struct EnterLicenseKeySheet: View {
    @Environment(LicenseController.self) private var license
    @State private var key = ""

    init(key: String = "") {
        _key = State(initialValue: key)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("Enter License Key")
                .font(.headline)
            TextField("License key", text: $key, prompt: Text(verbatim: "INPUTWEAVE-…"))
                .font(.body.monospaced())
                .textFieldStyle(.roundedBorder)
                .disabled(license.activating)
                .onSubmit(activate)
            Text("The key is on Polar’s order page and in the email Polar sent you.")
                .font(.callout)
                .foregroundStyle(.secondary)
            if let error = license.keyError {
                Label {
                    Text(Self.text(error))
                } icon: {
                    Image(systemName: "exclamationmark.circle.fill").foregroundStyle(.red)
                }
                .font(.callout)
                .fixedSize(horizontal: false, vertical: true)
            }
            HStack(spacing: 8) {
                if license.activating {
                    ProgressView().controlSize(.small)
                    Text("Activating…").foregroundStyle(.secondary)
                } else if license.keyError == .seatsFull {
                    LinkButton(title: "Manage Computers…", url: license.links.customerPortal)
                }
                Spacer()
                Button("Cancel", role: .cancel) { license.cancelEntering() }
                    .keyboardShortcut(.cancelAction)
                Button("Activate", action: activate)
                    .keyboardShortcut(.defaultAction)
                    .disabled(license.activating || key.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
            }
            .padding(.top, 6)
        }
        .padding(20)
        .frame(width: 440)
    }

    private func activate() {
        guard !license.activating, !key.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty else { return }
        license.submit(key)
    }

    static func text(_ error: LicenseController.KeyError) -> LocalizedStringKey {
        switch error {
        case .notAKey: "This isn’t an InputWeave license key. Check it against the email from Polar."
        case .seatsFull: "This license is already used on 5 computers. Free a computer you no longer use in Polar’s customer portal, then try again."
        case .noLongerValid: "This license is no longer valid. It may have been refunded."
        case .polarUnreachable: "Polar can’t be reached. Check this computer’s internet connection and try again."
        case .unavailable: "License information can’t be read right now."
        case .serviceNotResponding: "The background service is not responding."
        case .failed: "InputWeave couldn’t save the license. Try again."
        }
    }
}

/// The license line in About: "Trial" and Buy a License…, or the label.
struct AboutLicenseLine: View {
    @Environment(LicenseController.self) private var license
    @Environment(\.openURL) private var openURL

    var body: some View {
        Group {
            if license.isLicensed {
                WovenLabel(size: .medium)
            } else {
                HStack(spacing: 8) {
                    TrialCapsule()
                    if let checkout = license.links.checkout {
                        Button("Buy a License…") { openURL(checkout) }
                            .buttonStyle(.link)
                            .font(.callout)
                    }
                }
            }
        }
        .onAppear { license.surfaceAppeared() }
        .onDisappear { license.surfaceDisappeared() }
    }
}

private struct Notice: View {
    let symbol: String
    let color: Color
    let text: Text

    var body: some View {
        Label {
            text.fixedSize(horizontal: false, vertical: true)
        } icon: {
            Image(systemName: symbol).foregroundStyle(color)
        }
    }
}

/// One numbered step of "How the license works", which is a sequence.
private struct Step: View {
    let number: Int
    let text: Text

    var body: some View {
        HStack(spacing: 12) {
            Text(verbatim: "\(number)")
                .font(.callout.weight(.semibold))
                .foregroundStyle(.secondary)
                .frame(width: 22, height: 22)
                .background(Circle().fill(Color.secondary.opacity(0.15)))
            text
        }
    }
}
