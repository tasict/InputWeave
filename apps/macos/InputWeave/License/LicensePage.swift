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
    @State private var confirmingRemoval = false

    var body: some View {
        @Bindable var license = license
        Form {
            if license.isLicensed {
                LicensedSections(confirmingRemoval: $confirmingRemoval)
            } else {
                TrialSections()
            }
        }
        .formStyle(.grouped)
        .sheet(isPresented: $license.entering) {
            EnterLicenseKeySheet()
        }
        .confirmationDialog("Remove the license from this computer?", isPresented: $confirmingRemoval) {
            Button("Remove License", role: .destructive) {
                license.remove()
            }
        } message: {
            Text("This frees one of your 5 computers. Your other computers keep their license, and this one won’t pick it up from them again until you enter the key.")
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

    var body: some View {
        let state = license.license.state
        Section {
            VStack(alignment: .leading, spacing: 10) {
                Text("Trial")
                    .font(.title2.weight(.semibold))
                Text("Every feature works without a license, for as long as you like. A license replaces “Trial” with “Licensed” here and in About InputWeave.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                if state == .seatsFull {
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
                HStack(spacing: 8) {
                    if state == .seatsFull && !license.removalPending {
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
        Section {
            Step(number: 1, text: Text("Buy a license from Polar and get a license key."))
            Step(number: 2, text: Text("Enter the key on one of your computers."))
            Step(number: 3, text: Text("Your paired computers pick up the license by themselves, up to 5 in all."))
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
    @Binding var confirmingRemoval: Bool

    var body: some View {
        let record = license.license
        let email = LicenseText.maskedEmail(record.customerEmail)
        let computers = LicenseText.computers(record, thisName: model.thisComputerName)
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
        Section {
            if license.removalRefused {
                Notice(symbol: "exclamationmark.triangle.fill", color: .orange,
                       text: Text("InputWeave couldn’t remove the license on this computer."))
            }
            HStack(spacing: 8) {
                LinkButton(title: "Manage Computers…", url: license.links.customerPortal)
                Button("Remove License…") { confirmingRemoval = true }
            }
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
