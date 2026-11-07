import XCTest

/// The translations in one language, as the compiled string catalog has
/// them: a bundle of that language's folder alone.
private func language(_ code: String) throws -> Bundle {
    let folder = try XCTUnwrap(
        Bundle(for: OfflineTextTests.self).path(forResource: code, ofType: "lproj"), "no \(code) translations")
    return try XCTUnwrap(Bundle(path: folder))
}

/// What the app shows about computers taken offline for now.
final class OfflineTextTests: XCTestCase {
    private func device(
        _ id: String = "pc", state: Inputweave_App_V1_ConnectionState = .offline, offline: Bool = false,
        by: String = "", byName: String = "", unaware: Bool = false
    ) -> DeviceStatus {
        var device = DeviceStatus()
        device.id = id
        device.name = id == "pc" ? "Windows Desktop" : id == "mini" ? "Mac mini" : "Linux Laptop"
        device.paired = true
        device.state = state
        device.offlineForNow = offline
        device.offlineByDeviceID = by
        device.offlineByName = byName
        device.offlineUnaware = unaware
        return device
    }

    private func status(
        _ devices: [DeviceStatus], supported: Bool = true, here: Bool = false, by: String = "", byName: String = ""
    ) -> AgentStatus {
        var status = AgentStatus()
        status.thisDeviceID = "mac"
        status.thisDeviceName = "MacBook Pro"
        status.offlineSupported = supported
        status.offlineForNow = here
        status.offlineByDeviceID = by
        status.offlineByName = byName
        status.devices = devices
        return status
    }

    func testAComputerTakenOfflineShowsSoButNotWhileThisOneIs() {
        let pc = device(offline: true, by: "mini", byName: "Mac mini")
        XCTAssertTrue(OfflineText.shows(pc, status: status([pc])))
        XCTAssertFalse(OfflineText.shows(device(), status: status([device()])))
        // While this computer is offline for now, every other one is
        // plainly offline.
        XCTAssertFalse(OfflineText.shows(pc, status: status([pc], here: true, by: "mac")))
        // A service from before says nothing of it.
        XCTAssertFalse(OfflineText.shows(pc, status: status([pc], supported: false)))
        XCTAssertFalse(OfflineText.shows(nil, status: status([pc])))
        XCTAssertTrue(OfflineText.here(status([], here: true, by: "mac")))
        XCTAssertFalse(OfflineText.here(status([], supported: false, here: true, by: "mac")))
    }

    func testTheStatusAndWhoSetItInBothLanguages() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        XCTAssertEqual(OfflineText.status(bundle: en), "Offline for now")
        XCTAssertEqual(OfflineText.status(bundle: zh), "暫時離線")
        let byMini = device(offline: true, by: "mini", byName: "Mac mini")
        XCTAssertEqual(OfflineText.setBy(byMini, thisDeviceID: "mac", bundle: en), "Set by Mac mini.")
        XCTAssertEqual(OfflineText.setBy(byMini, thisDeviceID: "mac", bundle: zh), "由「Mac mini」設定。")
        let byThis = device(offline: true, by: "mac", byName: "MacBook Pro")
        XCTAssertEqual(OfflineText.setBy(byThis, thisDeviceID: "mac", bundle: en), "Set on this computer.")
        XCTAssertEqual(OfflineText.setBy(byThis, thisDeviceID: "mac", bundle: zh), "由這台設定。")
    }

    func testAnotherComputerWhenTheServiceDoesNotKnowWhichTookItOffline() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        let unknown = device(offline: true)
        XCTAssertEqual(OfflineText.setBy(unknown, thisDeviceID: "mac", bundle: en), "Set by another computer.")
        XCTAssertEqual(OfflineText.setBy(unknown, thisDeviceID: "mac", bundle: zh), "由另一台電腦設定。")
        // An empty id is never this computer, even while this one's is
        // not known.
        XCTAssertEqual(OfflineText.setBy(unknown, thisDeviceID: "", bundle: en), "Set by another computer.")
        XCTAssertFalse(OfflineText.isThis("", ""))
        XCTAssertFalse(OfflineText.isThis("", "mac"))
        XCTAssertTrue(OfflineText.isThis("mac", "mac"))
        // A name without an id is shown.
        XCTAssertEqual(
            OfflineText.setBy(device(offline: true, byName: "Mac mini"), thisDeviceID: "mac", bundle: en), "Set by Mac mini.")

        let away = status([], here: true)
        XCTAssertEqual(
            OfflineText.panelSubtitle(away, bundle: en),
            "Set by another computer. Your other computers don’t control this one or share the clipboard and files with it.")
        XCTAssertEqual(
            OfflineText.panelSubtitle(away, bundle: zh), "由另一台電腦設定。其他電腦不會控制這台，也不會和它同步剪貼簿或互傳檔案。")
        var unnamed = away
        unnamed.thisDeviceID = ""
        XCTAssertEqual(
            OfflineText.panelSubtitle(unnamed, bundle: en),
            "Set by another computer. Your other computers don’t control this one or share the clipboard and files with it.")
        XCTAssertEqual(
            OfflineText.panelSubtitle(status([], here: true, byName: "Windows Desktop"), bundle: en),
            "Set by Windows Desktop. Your other computers don’t control this one or share the clipboard and files with it.")

        // The HUD: taken offline by a computer the service does not know.
        var change = OfflineChange()
        change.deviceID = "mac"
        change.offline = true
        XCTAssertEqual(OfflineText.news(change, status: away), .takenOffline(name: ""))
        // An id without a name: still another computer.
        change.byDeviceID = "pc"
        XCTAssertEqual(OfflineText.news(change, status: away), .takenOffline(name: ""))
        change.byDeviceID = ""
        XCTAssertEqual(
            OfflineText.message(.takenOffline(name: ""), name: "", bundle: en),
            "Another computer took this computer offline for now.")
        XCTAssertEqual(OfflineText.message(.takenOffline(name: ""), name: "", bundle: zh), "另一台電腦把這台設為暫時離線。")
        // Nor is a change an unknown computer made this one's doing, even
        // while this one's id is not known.
        change.deviceID = "pc"
        XCTAssertNil(OfflineText.news(change, status: unnamed))
    }

    func testWarnsOfTheOtherComputersOnTheOlderVersionOnlyWhileItRunsItToo() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        let mini = device("mini", state: .connected, unaware: true)
        let laptop = device("laptop", state: .connected)
        // On 1.1 itself, it keeps the Mac mini away on its own.
        let pc = device(offline: true, by: "mac", byName: "MacBook Pro")
        XCTAssertEqual(OfflineText.unaware(for: pc, status: status([pc, mini, laptop])).map(\.id), [])
        // On 1.0 too: the Mac mini still controls it.
        let older = device(offline: true, by: "mac", byName: "MacBook Pro", unaware: true)
        XCTAssertEqual(OfflineText.unaware(for: older, status: status([older, mini, laptop])).map(\.id), ["mini"])
        // The only one on 1.0: the others keep off it.
        XCTAssertEqual(OfflineText.unaware(for: older, status: status([older, laptop])).map(\.id), [])
        // Not offline for now: nothing to warn of.
        let connected = device(state: .connected, unaware: true)
        XCTAssertEqual(OfflineText.unaware(for: connected, status: status([connected, mini])).map(\.id), [])
        XCTAssertEqual(
            OfflineText.unawareWarning(name: "Mac mini", bundle: en),
            "Mac mini runs an older InputWeave that doesn’t know about this and still controls it. Update Mac mini to 1.1.")
        XCTAssertEqual(
            OfflineText.unawareWarning(name: "Mac mini", bundle: zh),
            "「Mac mini」的 InputWeave 是舊版，不認得暫時離線，仍會控制它。請把「Mac mini」更新到 1.1。")
    }

    func testTheButtonForAnotherComputerOnlyWhileThisOneIsNotOffline() throws {
        let pc = device(state: .connected)
        let away = device(offline: true, by: "mini", byName: "Mac mini")
        XCTAssertEqual(OfflineText.offer(pc, status: status([pc])), .takeOffline)
        XCTAssertEqual(OfflineText.offer(away, status: status([away])), .bringBack)
        XCTAssertEqual(OfflineText.offer(device(), status: status([device()])), .takeOffline)
        // Not for this computer, which the ⋯ menu takes offline.
        XCTAssertEqual(OfflineText.offer(device("mac"), status: status([])), .hidden)
        // Not while this one is offline for now: it can only bring itself
        // back.
        XCTAssertEqual(OfflineText.offer(away, status: status([away], here: true, by: "mini")), .hidden)
        XCTAssertEqual(OfflineText.offer(pc, status: status([pc], supported: false)), .hidden)
        XCTAssertEqual(OfflineText.offer(nil, status: status([])), .hidden)

        let en = try language("en")
        let zh = try language("zh-Hant")
        XCTAssertEqual(OfflineText.label(.takeOffline, bundle: en), "Take Offline")
        XCTAssertEqual(OfflineText.label(.takeOffline, bundle: zh), "暫時離線")
        XCTAssertEqual(OfflineText.label(.bringBack, bundle: en), "Bring Back Online")
        XCTAssertEqual(OfflineText.label(.bringBack, bundle: zh), "恢復連線")
        XCTAssertNil(OfflineText.label(.hidden, bundle: en))
        XCTAssertEqual(
            OfflineText.help(bundle: en),
            "None of your computers control it or share the clipboard and files with it until someone brings it back. It can bring itself back too.")
        XCTAssertEqual(
            OfflineText.help(bundle: zh), "所有電腦都不會控制它，也不會和它同步剪貼簿或互傳檔案，直到有人恢復連線。它自己也能恢復。")
    }

    func testThisComputersMenuItemAndPanel() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        XCTAssertEqual(OfflineText.thisComputerItem(status([]), bundle: en), "Take This Computer Offline")
        XCTAssertEqual(OfflineText.thisComputerItem(status([]), bundle: zh), "讓這台暫時離線")
        let away = status([], here: true, by: "pc", byName: "Windows Desktop")
        XCTAssertEqual(OfflineText.thisComputerItem(away, bundle: en), "Bring Back Online")
        XCTAssertEqual(OfflineText.thisComputerItem(away, bundle: zh), "恢復連線")
        XCTAssertNil(OfflineText.thisComputerItem(status([], supported: false)))

        XCTAssertEqual(OfflineText.panelTitle(bundle: en), "This computer is offline for now")
        XCTAssertEqual(OfflineText.panelTitle(bundle: zh), "這台暫時離線")
        XCTAssertEqual(
            OfflineText.panelSubtitle(away, bundle: en),
            "Set by Windows Desktop. Your other computers don’t control this one or share the clipboard and files with it.")
        XCTAssertEqual(
            OfflineText.panelSubtitle(away, bundle: zh), "由「Windows Desktop」設定。其他電腦不會控制這台，也不會和它同步剪貼簿或互傳檔案。")
        let itself = status([], here: true, by: "mac", byName: "MacBook Pro")
        XCTAssertEqual(
            OfflineText.panelSubtitle(itself, bundle: en),
            "Set on this computer. Your other computers don’t control this one or share the clipboard and files with it.")
        XCTAssertEqual(
            OfflineText.panelSubtitle(itself, bundle: zh), "由這台設定。其他電腦不會控制這台，也不會和它同步剪貼簿或互傳檔案。")
    }

    func testAComputerTakenOfflineIsNoProblemAndNoneIsWhileThisOneIs() {
        let pc = device(offline: true, by: "mac", byName: "MacBook Pro")
        let mini = device("mini")
        let laptop = device("laptop", state: .connected)
        XCTAssertEqual(OfflineText.unreachable(status([pc, mini, laptop])).map(\.id), ["mini"])
        XCTAssertEqual(OfflineText.unreachable(status([pc, mini, laptop], here: true, by: "pc")).map(\.id), [])
        // A service from before: every offline computer, as before.
        XCTAssertEqual(OfflineText.unreachable(status([mini, laptop], supported: false)).map(\.id), ["mini"])
    }

    private func changed(
        _ count: UInt64, device: String, offline: Bool, by: String, supported: Bool = true, byName: String = ""
    ) -> AgentStatus {
        var status = status([], supported: supported, here: device == "mac" && offline, by: by)
        status.offlineChanges = count
        status.lastOfflineChange.deviceID = device
        status.lastOfflineChange.offline = offline
        status.lastOfflineChange.byDeviceID = by
        status.lastOfflineChange.byName = byName
        return status
    }

    func testTheHUDTellsWhatChangedButNotOfTheFirstStatus() {
        var watch = OfflineText.Watch()
        // Already counted when the app connects: only remembered.
        XCTAssertNil(watch.update(changed(3, device: "pc", offline: true, by: "mac")))
        XCTAssertNil(watch.update(changed(3, device: "pc", offline: true, by: "mac")))
        XCTAssertEqual(watch.update(changed(4, device: "pc", offline: false, by: "mac")), .back(device: "pc"))
        XCTAssertEqual(watch.update(changed(5, device: "pc", offline: true, by: "mac")), .offline(device: "pc"))
        // This computer, from here: with its own name.
        XCTAssertEqual(watch.update(changed(6, device: "mac", offline: true, by: "mac")), .offline(device: "mac"))
        // Brought back from here: nothing to tell.
        XCTAssertNil(watch.update(changed(7, device: "mac", offline: false, by: "mac")))
        // Another computer took this one offline.
        XCTAssertEqual(
            watch.update(changed(8, device: "mac", offline: true, by: "pc", byName: "Windows Desktop")),
            .takenOffline(name: "Windows Desktop"))
        // A service that started over without the app noticing: remembered.
        XCTAssertNil(watch.update(changed(1, device: "pc", offline: true, by: "mac")))
        XCTAssertEqual(watch.update(changed(2, device: "pc", offline: false, by: "mac")), .back(device: "pc"))
    }

    func testTheFirstStatusAfterConnectingAgainIsOnlyRemembered() {
        var watch = OfflineText.Watch()
        XCTAssertNil(watch.update(changed(1, device: "pc", offline: true, by: "mac")))
        XCTAssertNil(watch.update(nil))
        XCTAssertNil(watch.update(changed(2, device: "pc", offline: false, by: "mac")))
        XCTAssertEqual(watch.update(changed(3, device: "pc", offline: true, by: "mac")), .offline(device: "pc"))
    }

    func testAServiceFromBeforeTellsNothing() {
        var watch = OfflineText.Watch()
        XCTAssertNil(watch.update(changed(0, device: "", offline: false, by: "", supported: false)))
        XCTAssertNil(watch.update(changed(1, device: "pc", offline: true, by: "mac", supported: false)))
    }

    func testOnlyWhatThisComputerDidOrWhatWasDoneToIt() {
        let base = status([])
        func news(_ device: String, _ offline: Bool, by: String, byName: String = "") -> OfflineText.News? {
            var change = OfflineChange()
            change.deviceID = device
            change.offline = offline
            change.byDeviceID = by
            change.byName = byName
            return OfflineText.news(change, status: base)
        }
        XCTAssertEqual(news("pc", true, by: "mac"), .offline(device: "pc"))
        XCTAssertEqual(news("pc", false, by: "mac"), .back(device: "pc"))
        XCTAssertEqual(news("mac", true, by: "mac"), .offline(device: "mac"))
        XCTAssertNil(news("mac", false, by: "mac"))
        XCTAssertEqual(news("mac", true, by: "mini", byName: "Mac mini"), .takenOffline(name: "Mac mini"))
        // Another computer bringing this one back, or changing a third.
        XCTAssertNil(news("mac", false, by: "mini"))
        XCTAssertNil(news("pc", true, by: "mini"))
    }

    func testTheHUDsWordsInBothLanguages() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        let name = "Windows Desktop"
        XCTAssertEqual(OfflineText.message(.offline(device: "pc"), name: name, bundle: en), "Windows Desktop is offline for now.")
        XCTAssertEqual(OfflineText.message(.offline(device: "pc"), name: name, bundle: zh), "「Windows Desktop」暫時離線。")
        XCTAssertEqual(OfflineText.message(.back(device: "pc"), name: name, bundle: en), "Windows Desktop is back online.")
        XCTAssertEqual(OfflineText.message(.back(device: "pc"), name: name, bundle: zh), "「Windows Desktop」已恢復連線。")
        XCTAssertEqual(
            OfflineText.message(.takenOffline(name: "MacBook Pro"), name: "MacBook Pro", bundle: en),
            "MacBook Pro took this computer offline for now.")
        XCTAssertEqual(
            OfflineText.message(.takenOffline(name: "MacBook Pro"), name: "MacBook Pro", bundle: zh),
            "「MacBook Pro」把這台設為暫時離線。")
        XCTAssertEqual(
            OfflineText.wentOffline(name, bundle: en),
            "“Windows Desktop” went offline for now. The keyboard and mouse are back on this computer.")
        XCTAssertEqual(OfflineText.wentOffline(name, bundle: zh), "「Windows Desktop」暫時離線，鍵鼠已回到這台。")
    }

    func testTheMenuBarIconsWordsInChinese() throws {
        let zh = try language("zh-Hant")
        XCTAssertEqual(
            zh.localizedString(forKey: "InputWeave, this computer is offline for now", value: "missing", table: nil),
            "InputWeave，這台暫時離線")
    }

    func testTheSymbolComesBeforeTheOthers() {
        XCTAssertEqual(OfflineText.symbol, "network.slash")
        XCTAssertEqual(ConnectionText.nameSymbol(offlineForNow: true, screenLocked: false, wakeable: false), "network.slash")
        XCTAssertEqual(ConnectionText.nameSymbol(offlineForNow: true, screenLocked: true, wakeable: true), "network.slash")
        XCTAssertEqual(ConnectionText.nameSymbol(offlineForNow: false, screenLocked: true, wakeable: true), "lock.fill")
        XCTAssertEqual(ConnectionText.nameSymbol(offlineForNow: false, screenLocked: false, wakeable: true), "moon.zzz")
    }
}
