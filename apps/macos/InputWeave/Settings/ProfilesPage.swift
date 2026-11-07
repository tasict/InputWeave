import SwiftUI

/// The Profiles page: the profile in use and the network this Mac is on;
/// every profile in a list, the selected one's settings beside it; and
/// recognizing networks by their Wi-Fi name too. Every change goes to the
/// background service at once, which answers with how things stand; none
/// asks to confirm, as each is easily changed back.
struct ProfilesPage: View {
    @Environment(AgentModel.self) private var model
    /// Reads the Wi-Fi name once the user lets it; nil in snapshots.
    var wifiNames: WifiNameReporter? = AppActions.wifiNames
    @State private var selection: String?
    /// The profile whose name field gets the cursor as it shows: one just
    /// added.
    @State private var naming: String?
    @FocusState private var nameFocused: Bool
    /// The message the selected profile's name field starts with; for
    /// snapshots.
    private let nameMessage: String?

    init(selection: String? = nil, wifiNames: WifiNameReporter? = AppActions.wifiNames, nameMessage: String? = nil) {
        _selection = State(initialValue: selection)
        self.wifiNames = wifiNames
        self.nameMessage = nameMessage
    }

    var body: some View {
        let status = model.status
        // The first while none is selected, or the selected one is gone.
        let selected = status.profiles.first { $0.id == selection } ?? status.profiles.first
        Form {
            Section {
                ProfileInUseRow()
                CurrentNetworkRow()
            }
            Section {
                HStack(alignment: .top, spacing: 16) {
                    ProfileList(
                        selection: Binding(get: { selected?.id }, set: { selection = $0 }),
                        removable: selected.map { !ProfileText.isFixed($0) } ?? false,
                        add: add, remove: { if let selected { remove(selected) } })
                    if let selected {
                        ProfileSettings(
                            profile: selected, nameFocused: $nameFocused, focusOnAppear: naming == selected.id,
                            focused: { naming = nil }, nameMessage: nameMessage)
                            // Its own name field for each profile.
                            .id(selected.id)
                    }
                }
                .padding(.vertical, 4)
            }
            Section {
                Toggle(isOn: Binding(
                    get: { status.wifiNames || wifiNames?.asking == true },
                    set: { on in
                        if on { wifiNames?.turnOn() } else { wifiNames?.turnOff() }
                    }
                )) {
                    Text("Also recognize networks by Wi-Fi name")
                    Text("InputWeave normally recognizes a network by its router. Turn this on to match Wi-Fi names too; macOS asks for location access, used only to read the Wi-Fi name.")
                }
                // Drawn again when turning it on was refused, so it shows off.
                .id(wifiNames?.refusals ?? 0)
            }
        }
        .formStyle(.grouped)
    }

    /// Adds a profile and selects it, with the cursor in its name field.
    private func add() {
        model.changeProfiles("Adding a profile") { model in
            guard let id = try await model.addProfile(named: ProfileText.newName()) else { return }
            naming = id
            selection = id
        }
    }

    /// Removes one of the user's own profiles and selects the one before it.
    private func remove(_ profile: AgentProfile) {
        guard !ProfileText.isFixed(profile) else { return }
        let profiles = model.status.profiles
        let index = profiles.firstIndex { $0.id == profile.id } ?? 0
        selection = profiles[max(0, index - 1)].id
        model.changeProfiles("Removing a profile") { try await $0.removeProfile(profile.id) }
    }
}

/// "Profile in use": Automatic, which picks it by the network, or one the
/// user chose, which stays whatever the network.
private struct ProfileInUseRow: View {
    @Environment(AgentModel.self) private var model

    var body: some View {
        let status = model.status
        Picker(selection: Binding(
            get: { status.profileChosen ? status.profileID : "" },
            set: { id in model.changeProfiles("Choosing a profile") { try await $0.selectProfile(id) } }
        )) {
            Text(ProfileText.automaticNow(status)).tag("")
            Divider()
            ForEach(status.profiles, id: \.id) { profile in
                Text(ProfileText.name(profile)).tag(profile.id)
            }
        } label: {
            Text("Profile in use")
            Text("Automatic picks the profile from the network you’re on. A profile you choose stays until you choose Automatic again.")
        }
        .pickerStyle(.menu)
    }
}

/// The network this Mac is on, and the profile to add it to, the one that
/// has it checked.
private struct CurrentNetworkRow: View {
    @Environment(AgentModel.self) private var model

    var body: some View {
        let status = model.status
        LabeledContent {
            Menu("Add to Profile") {
                Picker("Add to Profile", selection: Binding(
                    get: { ProfileText.holder(of: status.network.id, in: status)?.id ?? "" },
                    set: { id in
                        guard id != ProfileText.holder(of: status.network.id, in: status)?.id else { return }
                        model.changeProfiles("Adding the network to a profile") { try await $0.addNetworkToProfile(id) }
                    }
                )) {
                    ForEach(ProfileText.addable(status), id: \.id) { profile in
                        Text(ProfileText.name(profile)).tag(profile.id)
                    }
                }
                .pickerStyle(.inline)
                .labelsHidden()
            }
            .fixedSize()
            .disabled(!ProfileText.canAddNetwork(status))
        } label: {
            if let current = ProfileText.current(status) {
                NetworkLabelView(label: current)
            } else {
                Text(ProfileText.noNetwork()).foregroundStyle(.secondary)
            }
        }
    }
}

/// A network: its symbol, what to call it and, while it has a name of its
/// own, its router beneath.
private struct NetworkLabelView: View {
    let label: ProfileText.NetworkLabel

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: label.symbol)
                .foregroundStyle(.secondary)
                .frame(width: 18)
                .accessibilityHidden(true)
            VStack(alignment: .leading, spacing: 1) {
                Text(label.title)
                    .monospacedDigit()
                if let subtitle = label.subtitle {
                    Text(subtitle)
                        .font(.callout)
                        .foregroundStyle(.secondary)
                        .monospacedDigit()
                }
            }
        }
        .accessibilityElement(children: .combine)
    }
}

/// Every profile in a bordered list, with + and − beneath; Default and
/// Other networks cannot be removed.
private struct ProfileList: View {
    @Environment(AgentModel.self) private var model
    @Binding var selection: String?
    let removable: Bool
    let add: () -> Void
    let remove: () -> Void

    var body: some View {
        VStack(spacing: 0) {
            List(selection: $selection) {
                ForEach(model.status.profiles, id: \.id) { profile in
                    Text(ProfileText.name(profile))
                        .lineLimit(1)
                        .tag(profile.id)
                        .listRowSeparator(.hidden)
                }
            }
            .listStyle(.plain)
            .scrollContentBackground(.hidden)
            .frame(height: 168)
            .accessibilityLabel(Text("Profiles"))
            Divider()
            HStack(spacing: 0) {
                Button(action: add) {
                    Image(systemName: "plus")
                        .frame(width: 24, height: 20)
                        .contentShape(Rectangle())
                }
                .accessibilityLabel(Text("Add Profile"))
                .help(Text("Add Profile"))
                Divider()
                    .frame(height: 14)
                Button(action: remove) {
                    Image(systemName: "minus")
                        .frame(width: 24, height: 20)
                        .contentShape(Rectangle())
                }
                .disabled(!removable)
                .accessibilityLabel(Text("Remove Profile"))
                .help(Text("Remove Profile"))
                Spacer()
            }
            .buttonStyle(.borderless)
            .padding(.horizontal, 2)
            .padding(.vertical, 1)
        }
        .frame(width: 180)
        .background(Color(nsColor: .controlBackgroundColor))
        .clipShape(RoundedRectangle(cornerRadius: 6))
        .overlay(RoundedRectangle(cornerRadius: 6).strokeBorder(Color(nsColor: .separatorColor)))
    }
}

/// One profile's settings: its name, sharing or paused, the computers it
/// connects, the clipboard and files, and the networks it is used on.
/// Paused, the computers, clipboard and files are dimmed and off; the
/// networks can still change.
private struct ProfileSettings: View {
    @Environment(AgentModel.self) private var model
    let profile: AgentProfile
    var nameFocused: FocusState<Bool>.Binding
    /// Puts the cursor in the name field as it shows.
    let focusOnAppear: Bool
    let focused: () -> Void
    /// The name as typed; sent when the user presses Return or leaves the
    /// field, not with each key.
    @State private var name = ""
    /// The name last sent, until the service answers: leaving the field
    /// and the profile at once sends it once.
    @State private var sentName: String?
    /// Why the service refused the name, under the field, and the name the
    /// field showed again then; gone once the user edits it, or leaves the
    /// profile or the page.
    @State private var nameMessage: String?
    @State private var restoredName = ""

    init(
        profile: AgentProfile, nameFocused: FocusState<Bool>.Binding, focusOnAppear: Bool,
        focused: @escaping () -> Void, nameMessage: String? = nil
    ) {
        self.profile = profile
        self.nameFocused = nameFocused
        self.focusOnAppear = focusOnAppear
        self.focused = focused
        _nameMessage = State(initialValue: nameMessage)
        _restoredName = State(initialValue: ProfileText.name(profile))
    }

    var body: some View {
        let status = model.status
        let fixed = ProfileText.isFixed(profile)
        let paused = profile.paused
        VStack(alignment: .leading, spacing: 10) {
            HStack(alignment: .firstTextBaseline) {
                Text("Name")
                Spacer()
                VStack(alignment: .leading, spacing: 4) {
                    TextField("Name", text: $name)
                        .labelsHidden()
                        .textFieldStyle(.roundedBorder)
                        .disabled(fixed)
                        .focused(nameFocused)
                        .onSubmit(commitName)
                    if let nameMessage {
                        Text(nameMessage)
                            .font(.callout)
                            .foregroundStyle(.red)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                .frame(maxWidth: 220)
            }
            Divider()
            HStack(alignment: .firstTextBaseline, spacing: 12) {
                VStack(alignment: .leading, spacing: 2) {
                    Text("Sharing")
                    if paused {
                        Text("In this profile this computer doesn’t show up on the network or connect to any computer. Pairings are kept.")
                            .font(.callout)
                            .foregroundStyle(.secondary)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                }
                Spacer(minLength: 0)
                Picker("Sharing", selection: Binding(
                    get: { profile.paused },
                    set: { on in edit("Pausing a profile") { try await $0.editProfile(profile.id, paused: on) } }
                )) {
                    Text("Share").tag(false)
                    Text("Pause").tag(true)
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .fixedSize()
            }
            Divider()
            Group {
                Text("Computers to connect")
                    .font(.headline)
                if status.devices.isEmpty {
                    Text("No other computers yet.")
                        .foregroundStyle(.secondary)
                } else {
                    // Two columns where there is room, one where names would
                    // be cut short.
                    LazyVGrid(
                        columns: [GridItem(.adaptive(minimum: 180), alignment: .leading)], alignment: .leading, spacing: 6
                    ) {
                        ForEach(status.devices, id: \.id) { device in
                            Toggle(isOn: Binding(
                                get: { !profile.offDeviceIds.contains(device.id) },
                                set: { on in
                                    edit("Choosing the computers of a profile") {
                                        try await $0.editProfile(profile.id, devices: [(id: device.id, connect: on)])
                                    }
                                }
                            )) {
                                Label(device.name, systemImage: OSLabel(os: device.os).symbol)
                                    .lineLimit(1)
                            }
                            .toggleStyle(.checkbox)
                        }
                    }
                }
                note("Unchecked computers are treated as offline in this profile, and they see this computer as offline too.")
                Divider()
                switchRow("Clipboard sync", isOn: Binding(
                    get: { profile.clipboard },
                    set: { on in edit("Turning a profile's clipboard on or off") { try await $0.editProfile(profile.id, clipboard: on) } }))
                switchRow("Send and receive files", isOn: Binding(
                    get: { profile.files },
                    set: { on in edit("Turning a profile's files on or off") { try await $0.editProfile(profile.id, files: on) } }))
                note("If clipboard sync is off on the General page, no profile syncs the clipboard.")
            }
            .disabled(paused)
            .opacity(paused ? 0.5 : 1)
            Divider()
            Text("Networks")
                .font(.headline)
            if profile.kind == .otherNetworks {
                Text("Networks not added to any profile use this one.")
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            } else {
                if profile.networks.isEmpty {
                    Text("No networks in this profile yet.")
                        .foregroundStyle(.secondary)
                }
                ForEach(profile.networks, id: \.id) { network in
                    let label = ProfileText.label(network)
                    HStack {
                        NetworkLabelView(label: label)
                        Spacer()
                        Button {
                            edit("Removing a network") { try await $0.removeNetwork(network.id) }
                        } label: {
                            Image(systemName: "xmark.circle.fill")
                                .foregroundStyle(.secondary)
                        }
                        .buttonStyle(.borderless)
                        .accessibilityLabel(Text("Remove \(label.title)"))
                        .help(Text("Remove \(label.title)"))
                    }
                }
                HStack {
                    Spacer()
                    Button("Add Current Network") {
                        edit("Adding the network to a profile") { try await $0.addNetworkToProfile(profile.id) }
                    }
                    .disabled(!ProfileText.canAddCurrent(to: profile, status: status))
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .onAppear { name = ProfileText.name(profile) }
        .task {
            guard focusOnAppear else { return }
            // Once the field is in the window.
            try? await Task.sleep(for: .milliseconds(50))
            nameFocused.wrappedValue = true
            focused()
        }
        // Another name from the service shows unless the user is typing one.
        .onChange(of: ProfileText.name(profile)) { _, new in
            if !nameFocused.wrappedValue { name = new }
        }
        .onChange(of: nameFocused.wrappedValue) { _, focused in
            if !focused { commitName() }
        }
        .onChange(of: name) { _, typed in
            if nameMessage != nil && !ProfileText.keepsRenameMessage(typed: typed, restored: restoredName) {
                nameMessage = nil
            }
        }
        .onDisappear(perform: commitName)
    }

    private func note(_ text: LocalizedStringKey) -> some View {
        Text(text)
            .font(.callout)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
    }

    private func switchRow(_ title: LocalizedStringKey, isOn: Binding<Bool>) -> some View {
        HStack {
            Text(title)
            Spacer()
            Toggle(title, isOn: isOn)
                .labelsHidden()
                .toggleStyle(.switch)
        }
    }

    private func edit(_ what: String, _ command: @escaping @MainActor (AgentModel) async throws -> Void) {
        model.changeProfiles(what, command)
    }

    /// Sends the name typed, when it changed; an empty one goes back to the
    /// profile's name, which the service would refuse anyway.
    private func commitName() {
        guard !ProfileText.isFixed(profile) else { return }
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else {
            name = profile.name
            return
        }
        guard trimmed != profile.name, trimmed != sentName else { return }
        let id = profile.id
        let before = profile.name
        sentName = trimmed
        Task {
            do {
                try await model.editProfile(id, name: trimmed)
            } catch {
                let code = (error as? AgentRefusal)?.code
                ProfileText.log.notice("Renaming a profile failed: \(code ?? error.localizedDescription, privacy: .public)")
                let refused = ProfileText.renameRefused(code, typing: nameFocused.wrappedValue)
                if refused.restoresName {
                    restoredName = before
                    name = before
                }
                if let message = refused.message {
                    nameMessage = message
                    AccessibilityNotification.Announcement(message).post()
                }
            }
            sentName = nil
        }
    }
}
