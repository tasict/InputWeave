import AppKit
import XCTest

/// The translations in one language, as the compiled string catalog has
/// them: a bundle of that language's folder alone.
private func language(_ code: String) throws -> Bundle {
    let folder = try XCTUnwrap(
        Bundle(for: ProfileTextTests.self).path(forResource: code, ofType: "lproj"), "no \(code) translations")
    return try XCTUnwrap(Bundle(path: folder))
}

private func network(_ id: String, name: String = "", kind: Inputweave_App_V1_NetworkKind = .wifi, router: String = "") -> AgentNetwork {
    var network = AgentNetwork()
    network.id = id
    network.name = name
    network.kind = kind
    network.router = router
    return network
}

private func profile(
    _ id: String, kind: Inputweave_App_V1_ProfileKind = .custom, name: String = "", paused: Bool = false,
    off: [String] = [], files: Bool = true, networks: [AgentNetwork] = []
) -> AgentProfile {
    var profile = AgentProfile()
    profile.id = id
    profile.kind = kind
    profile.name = name
    profile.paused = paused
    profile.offDeviceIds = off
    profile.clipboard = true
    profile.files = files
    profile.networks = networks
    return profile
}

private func device(
    _ id: String, state: Inputweave_App_V1_ConnectionState = .connected, notInProfile: Bool = false,
    offlineForNow: Bool = false
) -> DeviceStatus {
    var device = DeviceStatus()
    device.id = id
    device.name = id == "pc" ? "Windows Desktop" : id == "mini" ? "Mac mini" : "Linux Laptop"
    device.paired = true
    device.state = state
    device.rttMs = state == .connected ? 3 : 0
    device.notInProfile = notInProfile
    device.offlineForNow = offlineForNow
    return device
}

/// The sample group: Default, Office (with the Wi-Fi network this Mac is
/// on, leaving out the Linux laptop, no files) and Other networks.
private func status(
    inUse: String = "office", chosen: Bool = false, otherPaused: Bool = false, onNetwork: Bool = true, resolved: Bool = true,
    supported: Bool = true, devices: [DeviceStatus]? = nil
) -> AgentStatus {
    var status = AgentStatus()
    status.thisDeviceID = "mac"
    status.thisDeviceName = "MacBook Pro"
    status.sharing = true
    status.offlineSupported = true
    status.profilesSupported = supported
    let wifi = network("n-office", name: "Office-5G", router: "192.168.1.1")
    status.profiles = [
        profile("default", kind: .default, networks: [network("n-home", kind: .wired, router: "172.16.0.1")]),
        profile("office", name: "Office", off: ["laptop"], files: false, networks: [wifi]),
        profile("other", kind: .otherNetworks, paused: otherPaused),
    ]
    status.profileID = inUse
    status.profileChosen = chosen
    if onNetwork {
        status.network = inUse == "other" ? network("n-cafe", router: "198.51.100.1") : wifi
        status.network.resolved = resolved
    }
    status.devices = devices ?? [device("pc"), device("mini", state: .offline), device("laptop", state: .offline, notInProfile: true)]
    return status
}

/// What the app shows about profiles.
@MainActor
final class ProfileTextTests: XCTestCase {
    func testNamesTheFixedProfilesAndTheUsersOwn() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        let profiles = status().profiles
        XCTAssertEqual(profiles.map { ProfileText.name($0, bundle: en) }, ["Default", "Office", "Other networks"])
        XCTAssertEqual(profiles.map { ProfileText.name($0, bundle: zh) }, ["預設", "Office", "其他網路"])
        XCTAssertEqual(profiles.map(ProfileText.isFixed), [true, false, true])
        XCTAssertEqual(ProfileText.newName(bundle: en), "New Profile")
        XCTAssertEqual(ProfileText.newName(bundle: zh), "新的情境")
        XCTAssertEqual(ProfileText.automatic(bundle: zh), "自動")
    }

    func testARenameToANameAnotherProfileHasIsSaidAndTakenBack() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        XCTAssertEqual(
            ProfileText.renameRefused("name_taken", typing: true, bundle: en),
            .init(message: "Another profile already has this name.", restoresName: true))
        XCTAssertEqual(ProfileText.renameRefused("name_taken", typing: false, bundle: zh).message, "已經有同名的情境。")
        // Anything else is only logged, and takes the name back once the
        // user left the field.
        XCTAssertEqual(
            ProfileText.renameRefused("invalid_name", typing: true, bundle: en), .init(message: nil, restoresName: false))
        XCTAssertEqual(
            ProfileText.renameRefused("invalid_name", typing: false, bundle: en), .init(message: nil, restoresName: true))
        XCTAssertEqual(ProfileText.renameRefused(nil, typing: false, bundle: en), .init(message: nil, restoresName: true))
        // The message stands until the user edits the name again.
        XCTAssertTrue(ProfileText.keepsRenameMessage(typed: "Office", restored: "Office"))
        XCTAssertFalse(ProfileText.keepsRenameMessage(typed: "Office ", restored: "Office"))
        XCTAssertFalse(ProfileText.keepsRenameMessage(typed: "", restored: "Office"))
    }

    func testShowsANetworkByItsNameItsRouterOrAsAVPN() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        // Named: its router beneath.
        let named = network("a", name: "Office-5G", router: "192.168.1.1")
        XCTAssertEqual(
            ProfileText.label(named, bundle: en),
            .init(title: "Office-5G", subtitle: "Router 192.168.1.1", symbol: "wifi"))
        XCTAssertEqual(ProfileText.label(named, bundle: zh).subtitle, "路由器 192.168.1.1")
        // No name: its router, alone.
        let wired = network("b", kind: .wired, router: "172.16.0.1")
        XCTAssertEqual(
            ProfileText.label(wired, bundle: en),
            .init(title: "Router 172.16.0.1", subtitle: nil, symbol: "cable.connector.horizontal"))
        XCTAssertEqual(ProfileText.label(wired, bundle: zh).title, "路由器 172.16.0.1")
        // A VPN with neither.
        let vpn = network("c", kind: .vpn)
        XCTAssertEqual(ProfileText.label(vpn, bundle: en), .init(title: "VPN", subtitle: nil, symbol: "lock.shield"))
        XCTAssertEqual(ProfileText.label(vpn, bundle: zh).title, "VPN")
        // Neither a name nor a router: by its kind.
        XCTAssertEqual(ProfileText.label(network("e", kind: .wired), bundle: en).title, "Wired network")
        XCTAssertEqual(ProfileText.label(network("e", kind: .wired), bundle: zh).title, "有線網路")
        XCTAssertEqual(ProfileText.label(network("f", kind: .wifi), bundle: en).title, "Wi-Fi network")
        XCTAssertEqual(ProfileText.label(network("f", kind: .wifi), bundle: zh).title, "Wi-Fi 網路")
        XCTAssertNil(ProfileText.label(network("f", kind: .wifi), bundle: en).subtitle)
        // A kind not known: as a wired one, with its symbol.
        XCTAssertEqual(
            ProfileText.label(network("g", kind: .unspecified), bundle: en),
            .init(title: "Wired network", subtitle: nil, symbol: "cable.connector.horizontal"))
        XCTAssertEqual(ProfileText.label(network("g", kind: .unspecified), bundle: zh).title, "有線網路")
        // A named VPN without a router: no second line.
        XCTAssertEqual(ProfileText.label(network("d", name: "Work VPN", kind: .vpn), bundle: en).subtitle, nil)
        for kind in [Inputweave_App_V1_NetworkKind.wifi, .wired, .vpn, .unspecified] {
            XCTAssertNotNil(
                NSImage(systemSymbolName: ProfileText.symbol(kind), accessibilityDescription: nil), "\(kind)")
        }
        XCTAssertNotNil(NSImage(systemSymbolName: ProfileText.symbol, accessibilityDescription: nil))
    }

    func testTheCurrentNetworkOrNone() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        XCTAssertEqual(
            ProfileText.current(status(), bundle: en),
            .init(title: "Current network: Office-5G", subtitle: "Router 192.168.1.1", symbol: "wifi"))
        XCTAssertEqual(ProfileText.current(status(), bundle: zh)?.title, "目前的網路：Office-5G")
        XCTAssertEqual(
            ProfileText.current(status(inUse: "other"), bundle: en)?.title, "Current network: Router 198.51.100.1")
        XCTAssertNil(ProfileText.current(status(onNetwork: false)))
        XCTAssertEqual(ProfileText.noNetwork(bundle: en), "Not connected to a network.")
        XCTAssertEqual(ProfileText.noNetwork(bundle: zh), "沒有連上網路。")
    }

    func testAutomaticNamesTheProfileInUseOnlyWhileAutomatic() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        XCTAssertEqual(ProfileText.automaticNow(status(), bundle: en), "Automatic (now: Office)")
        XCTAssertEqual(ProfileText.automaticNow(status(inUse: "other"), bundle: zh), "自動（目前：其他網路）")
        // Chosen: which one Automatic would pick is not known here.
        XCTAssertEqual(ProfileText.automaticNow(status(chosen: true), bundle: en), "Automatic")
        XCTAssertEqual(ProfileText.automaticNow(status(supported: false), bundle: en), "Automatic")
    }

    func testWhereTheCurrentNetworkCanGo() {
        let office = status()
        XCTAssertEqual(ProfileText.holder(of: "n-office", in: office)?.id, "office")
        XCTAssertEqual(ProfileText.holder(of: "n-home", in: office)?.id, "default")
        XCTAssertNil(ProfileText.holder(of: "n-cafe", in: office))
        // Never to Other networks, which lists none.
        XCTAssertEqual(ProfileText.addable(office).map(\.id), ["default", "office"])
        let profiles = office.profiles
        XCTAssertTrue(ProfileText.canAddCurrent(to: profiles[0], status: office))
        XCTAssertFalse(ProfileText.canAddCurrent(to: profiles[1], status: office), "already there")
        XCTAssertFalse(ProfileText.canAddCurrent(to: profiles[2], status: office), "Other networks")
        XCTAssertFalse(ProfileText.canAddCurrent(to: profiles[0], status: status(onNetwork: false)), "no network")
    }

    func testTheCurrentNetworkIsAddedOnlyOnceTheServiceKnowsIt() {
        // Known by its router alone, just after joining it: every way to
        // add it is off.
        let joining = status(resolved: false)
        XCTAssertFalse(ProfileText.canAddNetwork(joining))
        XCTAssertFalse(ProfileText.canAddCurrent(to: joining.profiles[0], status: joining))
        let away = status(inUse: "other", otherPaused: true, resolved: false)
        XCTAssertTrue(ProfileText.offersAddingNetwork(away), "the offer stays, off")
        XCTAssertFalse(ProfileText.canAddNetwork(away))
        // Known: on again.
        XCTAssertTrue(ProfileText.canAddNetwork(status()))
        XCTAssertTrue(ProfileText.canAddCurrent(to: status().profiles[0], status: status()))
        XCTAssertTrue(ProfileText.canAddNetwork(status(inUse: "other", otherPaused: true)))
        // On none.
        XCTAssertFalse(ProfileText.canAddNetwork(status(onNetwork: false)))
    }

    func testTheProfileInUseItsComputersAndFiles() {
        let office = status()
        XCTAssertEqual(ProfileText.inUse(office)?.id, "office")
        XCTAssertFalse(ProfileText.paused(office))
        XCTAssertTrue(ProfileText.filesOff(office))
        XCTAssertFalse(ProfileText.filesOff(status(inUse: "default")))
        XCTAssertTrue(ProfileText.shows(office.devices[2], status: office))
        XCTAssertFalse(ProfileText.shows(office.devices[1], status: office), "just offline")
        XCTAssertTrue(ProfileText.keepsAway(office.devices[2], status: office))
        XCTAssertFalse(ProfileText.keepsAway(office.devices[0], status: office))
        XCTAssertFalse(ProfileText.keepsAway(nil, status: office))
        // Paused: every other computer is kept away, and none is said to be
        // not in it.
        let paused = status(inUse: "other", otherPaused: true, devices: [device("pc", state: .offline), device("mini", state: .offline)])
        XCTAssertTrue(ProfileText.paused(paused))
        XCTAssertTrue(paused.devices.allSatisfy { ProfileText.keepsAway($0, status: paused) })
        XCTAssertFalse(paused.devices.contains { ProfileText.shows($0, status: paused) })
        // A service from before says nothing of it.
        let old = status(supported: false)
        XCTAssertNil(ProfileText.inUse(old))
        XCTAssertFalse(ProfileText.shows(old.devices[2], status: old))
        XCTAssertFalse(ProfileText.filesOff(old))
    }

    func testOfflineForNowComesBeforeNotInThisProfile() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        XCTAssertEqual(ProfileText.notInProfile(bundle: en), "Not in this profile")
        XCTAssertEqual(ProfileText.notInProfile(bundle: zh), "不在這個情境")
        var group = status()
        // The service never says both; offline for now comes first anyway.
        group.devices.append(device("nas", state: .offline, offlineForNow: true))
        let nas = group.devices[3]
        XCTAssertTrue(OfflineText.shows(nas, status: group))
        XCTAssertEqual(SendTargets.unavailable(nas), .offlineForNow)
        XCTAssertEqual(SendTargets.unavailable(group.devices[2]), .notInProfile)
        var both = group.devices[2]
        both.offlineForNow = true
        XCTAssertEqual(SendTargets.unavailable(both), .offlineForNow)
        // Neither taken offline nor woken from here while left out, and no
        // problem.
        XCTAssertEqual(OfflineText.offer(group.devices[2], status: group), .hidden)
        XCTAssertEqual(OfflineText.offer(group.devices[1], status: group), .takeOffline)
        XCTAssertEqual(OfflineText.unreachable(group).map(\.id), ["mini"])
        // No symbol of its own after its name.
        XCTAssertNil(ConnectionText.nameSymbol(screenLocked: false, wakeable: false))
    }

    func testWhileTheProfileInUseIsPausedEveryComputerIsJustOffline() {
        var paused = status(inUse: "other", otherPaused: true, devices: [
            device("pc", state: .offline), device("mini", state: .offline, offlineForNow: true),
        ])
        XCTAssertFalse(OfflineText.shows(paused.devices[1], status: paused), "plainly offline")
        XCTAssertEqual(paused.devices.map { OfflineText.offer($0, status: paused) }, [.hidden, .hidden])
        XCTAssertEqual(OfflineText.unreachable(paused).map(\.id), [], "no problem")
        // Not paused: the Mac mini is offline for now again, and the PC a
        // problem.
        paused.profiles[2].paused = false
        XCTAssertTrue(OfflineText.shows(paused.devices[1], status: paused))
        XCTAssertEqual(OfflineText.unreachable(paused).map(\.id), ["pc"])
    }

    func testTheSendChoosersReasons() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        XCTAssertEqual(SendTargets.reason(.notInProfile, bundle: en), "Not in this profile")
        XCTAssertEqual(SendTargets.reason(.notInProfile, bundle: zh), "不在這個情境")
        let group = status().devices
        XCTAssertEqual(
            SendTargets.targets(group, bundle: zh).map { [$0.id, $0.status, "\($0.available)"] },
            [["pc", "已連線・3 ms", "true"], ["mini", "離線", "false"], ["laptop", "不在這個情境", "false"]])
        XCTAssertEqual(SendTargets.choice(group), .only("pc"))
        // Files off in the profile in use: every connected computer says so.
        XCTAssertEqual(SendTargets.unavailable(group[0], profileFilesOff: true), .filesOff)
        XCTAssertEqual(SendTargets.unavailable(group[2], profileFilesOff: true), .notInProfile)
        XCTAssertEqual(
            SendTargets.targets(group, profileFilesOff: true, bundle: en).map(\.status),
            ["File transfer with this computer is off", "Offline", "Not in this profile"])
        XCTAssertEqual(SendTargets.choice(group, profileFilesOff: true), .ask)
        let model = AgentModel.sample(status: status())
        XCTAssertEqual(model.fileReceivers.map(\.id), [], "Office sends no files")
        XCTAssertEqual(AgentModel.sample(status: status(inUse: "default")).fileReceivers.map(\.id), ["pc"])
    }

    func testThePanelsTitleCapsuleAndOffer() throws {
        let office = AgentModel.sample(status: status())
        XCTAssertEqual(PanelText.title(office), "Sharing keyboard and mouse")
        XCTAssertEqual(PanelText.capsule(office), "Office")
        XCTAssertEqual(PanelText.subtitle(office), "Cursor on MacBook Pro")
        XCTAssertFalse(office.profilePaused)

        // Automatic picked a paused Other networks for a network in no
        // profile: its title, no capsule, and the offer.
        let away = AgentModel.sample(status: status(inUse: "other", otherPaused: true))
        XCTAssertEqual(PanelText.title(away), "Other networks: sharing paused")
        XCTAssertNil(PanelText.capsule(away))
        XCTAssertEqual(PanelText.subtitle(away), "This network isn’t in any profile.")
        XCTAssertTrue(ProfileText.offersAddingNetwork(away.status))
        // Chosen by the user: only the title.
        let chosen = AgentModel.sample(status: status(inUse: "other", chosen: true, otherPaused: true))
        XCTAssertEqual(PanelText.title(chosen), "Other networks: sharing paused")
        XCTAssertNil(PanelText.subtitle(chosen))
        XCTAssertFalse(ProfileText.offersAddingNetwork(chosen.status))
        // On no network: nothing to add.
        let nowhere = AgentModel.sample(status: status(inUse: "other", otherPaused: true, onNetwork: false))
        XCTAssertFalse(ProfileText.offersAddingNetwork(nowhere.status))
        XCTAssertNil(PanelText.subtitle(nowhere))

        // The paused profile's title comes before this computer offline for
        // now; the capsule follows that one's title too.
        var offline = status(inUse: "other", otherPaused: true)
        offline.offlineForNow = true
        offline.offlineByDeviceID = "mac"
        XCTAssertEqual(PanelText.title(AgentModel.sample(status: offline)), "Other networks: sharing paused")
        offline.profiles[2].paused = false
        XCTAssertEqual(PanelText.title(AgentModel.sample(status: offline)), "This computer is offline for now")
        XCTAssertEqual(PanelText.capsule(AgentModel.sample(status: offline)), "Other networks")
        // Sharing paused too.
        var plain = status()
        plain.sharing = false
        XCTAssertEqual(PanelText.title(AgentModel.sample(status: plain)), "Sharing paused")
        XCTAssertEqual(PanelText.capsule(AgentModel.sample(status: plain)), "Office")
        // None until the service answers, nor from a service from before.
        XCTAssertNil(PanelText.capsule(AgentModel.sample(status: status(), link: .connecting)))
        XCTAssertNil(PanelText.capsule(AgentModel.sample(status: status(supported: false))))
    }

    func testThePanelsWordsInChinese() throws {
        let zh = try language("zh-Hant")
        let other = status(inUse: "other", otherPaused: true).profiles[2]
        XCTAssertEqual(ProfileText.pausedTitle(other, bundle: zh), "其他網路：已暫停共享")
        XCTAssertEqual(ProfileText.notInAnyProfile(bundle: zh), "這個網路沒有加到任何情境。")
        for key in ["Add This Network To…", "Profile", "Manage Profiles…", "Profiles", "Add to Profile"] {
            XCTAssertNotEqual(zh.localizedString(forKey: key, value: "missing", table: nil), "missing", key)
        }
        XCTAssertEqual(zh.localizedString(forKey: "Add This Network To…", value: nil, table: nil), "把這個網路加到…")
        XCTAssertEqual(zh.localizedString(forKey: "Manage Profiles…", value: nil, table: nil), "管理情境…")
    }

    func testAPausedProfileLooksPausedAndKeepsTheMenusFromSharing() {
        let paused = AgentModel.sample(status: status(inUse: "other", otherPaused: true))
        XCTAssertEqual(StatusIcon.State(paused, serviceEnabled: true, now: .now), .paused)
        XCTAssertEqual(StatusIcon.State(paused, serviceEnabled: false, now: .now), .attention)
        XCTAssertTrue(paused.profilePaused)
        XCTAssertFalse(paused.canToggleLock)
        // Every other computer's menu has only Rename….
        XCTAssertTrue(paused.status.devices.allSatisfy { paused.keptAway($0.id) })
        XCTAssertFalse(paused.keptAway("mac"))
        let office = AgentModel.sample(status: status())
        XCTAssertEqual(StatusIcon.State(office, serviceEnabled: true, now: .now), .normal)
        XCTAssertTrue(office.canToggleLock)
        XCTAssertEqual(office.status.devices.map { office.keptAway($0.id) }, [false, false, true])
        XCTAssertTrue(office.showsNotInProfile("laptop"))
    }

    private func switched(_ count: UInt64, to inUse: String = "office", paused: Bool = false, supported: Bool = true) -> AgentStatus {
        var changed = status(inUse: inUse, otherPaused: paused, supported: supported)
        changed.profileSwitches = count
        return changed
    }

    func testTheHUDTellsOfAProfileChangedByItselfButNotOfTheFirstStatus() {
        var watch = ProfileText.Watch()
        XCTAssertNil(watch.update(switched(4)))
        XCTAssertNil(watch.update(switched(4)))
        XCTAssertEqual(watch.update(switched(5, to: "other", paused: true))?.id, "other")
        XCTAssertEqual(watch.update(switched(6))?.id, "office")
        // Reconnected: only remembered.
        XCTAssertNil(watch.update(nil))
        XCTAssertNil(watch.update(switched(7, to: "default")))
        // A service that started over without the app noticing.
        XCTAssertNil(watch.update(switched(1)))
        XCTAssertEqual(watch.update(switched(2, to: "default"))?.id, "default")
        // A service from before.
        var old = ProfileText.Watch()
        XCTAssertNil(old.update(switched(0, supported: false)))
        XCTAssertNil(old.update(switched(1, supported: false)))
    }

    func testTheHUDsWordsInBothLanguages() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        let profiles = status(otherPaused: true).profiles
        XCTAssertEqual(ProfileText.switched(to: profiles[1], bundle: en), "Switched to Office.")
        XCTAssertEqual(ProfileText.switched(to: profiles[1], bundle: zh), "已切換到「Office」。")
        XCTAssertEqual(ProfileText.switched(to: profiles[2], bundle: en), "Switched to Other networks. Sharing is paused.")
        XCTAssertEqual(ProfileText.switched(to: profiles[2], bundle: zh), "已切換到「其他網路」，鍵鼠共享已暫停。")
    }
}

/// The HUD as the profile in use changes.
@MainActor
final class ProfileHUDTests: XCTestCase {
    private var defaults: UserDefaults!
    private var suite = ""

    override func setUp() async throws {
        suite = "ProfileHUDTests-\(UUID().uuidString)"
        defaults = UserDefaults(suiteName: suite)
    }

    override func tearDown() async throws {
        defaults.removePersistentDomain(forName: suite)
    }

    private func model(switches: UInt64, inUse: String = "office", returns: UInt64 = 0) -> AgentModel {
        var sample = status(inUse: inUse, otherPaused: true)
        sample.profileSwitches = switches
        sample.controlReturns = returns
        sample.lastReturn = returns == 0 ? .unspecified : .profileChanged
        sample.lastReturnDeviceID = returns == 0 ? "" : "pc"
        return AgentModel.sample(status: sample)
    }

    func testSaysWhenTheProfileChangedByItself() {
        let hud = HUDController(model: model(switches: 2), defaults: defaults)
        let now = Date()
        XCTAssertNil(hud.message(for: model(switches: 2), at: now))
        XCTAssertNil(hud.message(for: model(switches: 2), at: now))
        // To a paused one, which took the keyboard and mouse back: the
        // profile says it, not the return.
        XCTAssertEqual(
            hud.message(for: model(switches: 3, inUse: "other", returns: 1), at: now),
            "Switched to Other networks. Sharing is paused.")
        XCTAssertEqual(hud.message(for: model(switches: 4), at: now), "Switched to Office.")
        // Chosen by the user: not counted, nothing said, the return neither.
        XCTAssertNil(hud.message(for: model(switches: 4, inUse: "other", returns: 2), at: now))
        // After the service is out of reach, the first status is no news.
        XCTAssertNil(hud.message(for: AgentModel.sample(status: AgentStatus(), link: .connecting), at: now))
        XCTAssertNil(hud.message(for: model(switches: 9), at: now))
    }
}
