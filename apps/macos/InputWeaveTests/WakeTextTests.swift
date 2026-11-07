import XCTest

/// The translations in one language, as the compiled string catalog has
/// them: a bundle of that language's folder alone.
private func language(_ code: String) throws -> Bundle {
    let folder = try XCTUnwrap(
        Bundle(for: AgoTextTests.self).path(forResource: code, ofType: "lproj"), "no \(code) translations")
    return try XCTUnwrap(Bundle(path: folder))
}

/// How long ago, at each unit's edges.
final class AgoTextTests: XCTestCase {
    func testCountsWholeUnits() {
        XCTAssertEqual(AgoText.span(0), .justNow)
        XCTAssertEqual(AgoText.span(59.9), .justNow)
        XCTAssertEqual(AgoText.span(60), .minutes(1))
        XCTAssertEqual(AgoText.span(119.9), .minutes(1))
        XCTAssertEqual(AgoText.span(120), .minutes(2))
        XCTAssertEqual(AgoText.span(59 * 60 + 59), .minutes(59))
        XCTAssertEqual(AgoText.span(60 * 60), .hours(1))
        XCTAssertEqual(AgoText.span(2 * 60 * 60 - 1), .hours(1))
        XCTAssertEqual(AgoText.span(2 * 60 * 60), .hours(2))
        XCTAssertEqual(AgoText.span(24 * 60 * 60 - 1), .hours(23))
        XCTAssertEqual(AgoText.span(24 * 60 * 60), .days(1))
        XCTAssertEqual(AgoText.span(2 * 24 * 60 * 60 - 1), .days(1))
        XCTAssertEqual(AgoText.span(45 * 24 * 60 * 60), .days(45))
        // A clock set back.
        XCTAssertEqual(AgoText.span(-300), .justNow)
    }

    func testSaysItInEnglishWithOneAndMany() throws {
        let en = try language("en")
        let cases: [(TimeInterval, String)] = [
            (0, "just now"), (59, "just now"),
            (60, "1 minute ago"), (2 * 60, "2 minutes ago"), (59 * 60, "59 minutes ago"),
            (60 * 60, "1 hour ago"), (2 * 60 * 60, "2 hours ago"), (23 * 60 * 60, "23 hours ago"),
            (24 * 60 * 60, "1 day ago"), (3 * 24 * 60 * 60, "3 days ago"),
        ]
        for (elapsed, text) in cases {
            XCTAssertEqual(AgoText.text(elapsed, bundle: en), text, "\(elapsed)")
        }
    }

    func testSaysItInChinese() throws {
        let zh = try language("zh-Hant")
        let cases: [(TimeInterval, String)] = [
            (30, "剛剛"), (60, "1 分鐘前"), (5 * 60, "5 分鐘前"), (60 * 60, "1 小時前"), (2 * 60 * 60, "2 小時前"),
            (24 * 60 * 60, "1 天前"), (3 * 24 * 60 * 60, "3 天前"),
        ]
        for (elapsed, text) in cases {
            XCTAssertEqual(AgoText.text(elapsed, bundle: zh), text, "\(elapsed)")
        }
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        XCTAssertEqual(AgoText.text(since: now.addingTimeInterval(-90 * 60), now: now, bundle: zh), "1 小時前")
    }
}

/// What the app shows about waking another computer.
final class WakeTextTests: XCTestCase {
    private let now = Date(timeIntervalSince1970: 1_800_000_000)

    private func device(
        _ state: Inputweave_App_V1_ConnectionState = .offline, canWake: Bool = false,
        wake: WakeState = WakeState.none, offlineFor: TimeInterval? = nil, id: String = "mini"
    ) -> DeviceStatus {
        var device = DeviceStatus()
        device.id = id
        device.name = id == "mini" ? "Mac mini" : "Windows Desktop"
        device.paired = true
        device.state = state
        device.canWake = canWake
        device.wake = wake
        if let offlineFor {
            device.offlineSinceUnixMs = UInt64((now.timeIntervalSince1970 - offlineFor) * 1000)
        }
        return device
    }

    func testTheStatusSaysHowLongItHasBeenOfflineWhenItCanBeWoken() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        func status(_ device: DeviceStatus, supported: Bool = true, in bundle: Bundle) -> String {
            WakeText.offlineStatus(device, supported: supported, now: now, bundle: bundle)
        }
        let asleep = device(canWake: true, offlineFor: 2 * 60 * 60 + 5)
        XCTAssertEqual(status(asleep, in: en), "Offline · 2 hours ago")
        XCTAssertEqual(status(asleep, in: zh), "離線・2 小時前")
        let justNow = device(canWake: true, offlineFor: 10)
        XCTAssertEqual(status(justNow, in: en), "Offline · just now")
        XCTAssertEqual(status(justNow, in: zh), "離線・剛剛")
        XCTAssertEqual(status(device(canWake: true, offlineFor: 60), in: en), "Offline · 1 minute ago")
        // It didn't wake up: the same.
        XCTAssertEqual(
            status(device(canWake: true, wake: .notWoken, offlineFor: 3 * 24 * 60 * 60), in: en), "Offline · 3 days ago")
        // When it went offline is not known.
        XCTAssertEqual(status(device(canWake: true), in: en), "Offline")
        XCTAssertEqual(status(device(canWake: true), in: zh), "離線")
        // It can't be woken: plain, even knowing since when.
        XCTAssertEqual(status(device(offlineFor: 60 * 60), in: en), "Offline")
        XCTAssertEqual(status(device(.incompatible), in: en), "Offline")
        // A service from before waking says nothing of it.
        XCTAssertEqual(status(asleep, supported: false, in: en), "Offline")
    }

    func testBeingWokenShowsASpinnerAndSaysSo() throws {
        let waking = device(canWake: true, wake: .waking, offlineFor: 60 * 60)
        XCTAssertTrue(WakeText.waking(waking, supported: true))
        XCTAssertEqual(WakeText.offlineStatus(waking, supported: true, now: now, bundle: try language("en")), "Waking…")
        XCTAssertEqual(
            WakeText.offlineStatus(waking, supported: true, now: now, bundle: try language("zh-Hant")), "正在喚醒…")
        XCTAssertFalse(WakeText.waking(waking, supported: false))
        XCTAssertEqual(WakeText.offlineStatus(waking, supported: false, now: now), "Offline")
        XCTAssertFalse(WakeText.waking(device(canWake: true), supported: true))
        XCTAssertFalse(WakeText.waking(device(canWake: true, wake: .notWoken), supported: true))
        // Connected, it is no longer being woken.
        XCTAssertFalse(WakeText.waking(device(.connected, wake: .waking), supported: true))
    }

    func testWakeIsOfferedForAComputerThatCanBeWokenAndNotWhileItIsBeingWoken() {
        XCTAssertEqual(WakeText.offer(device(canWake: true), supported: true, requested: false), .enabled)
        // Didn't wake up: press again to try again.
        XCTAssertEqual(WakeText.offer(device(canWake: true, wake: .notWoken), supported: true, requested: false), .enabled)
        XCTAssertEqual(WakeText.offer(device(canWake: true, wake: .waking), supported: true, requested: false), .disabled)
        // This app's request is on its way.
        XCTAssertEqual(WakeText.offer(device(canWake: true), supported: true, requested: true), .disabled)
        XCTAssertEqual(WakeText.offer(device(canWake: true, wake: .notWoken), supported: true, requested: true), .disabled)
        XCTAssertEqual(WakeText.offer(device(), supported: true, requested: false), .hidden)
        XCTAssertEqual(WakeText.offer(device(.connected), supported: true, requested: false), .hidden)
        XCTAssertEqual(WakeText.offer(device(canWake: true), supported: false, requested: false), .hidden)
        XCTAssertEqual(WakeText.offer(nil, supported: true, requested: false), .hidden)
    }

    func testTheNoteUnderTheStatus() {
        XCTAssertEqual(WakeText.note(device(canWake: true, wake: .notWoken), supported: true), .notWoken)
        XCTAssertNil(WakeText.note(device(canWake: true), supported: true))
        XCTAssertNil(WakeText.note(device(canWake: true, wake: .waking), supported: true))
        XCTAssertEqual(WakeText.note(device(), supported: true), .cannotWake)
        XCTAssertEqual(WakeText.note(device(.incompatible), supported: true), .cannotWake)
        XCTAssertNil(WakeText.note(device(.connected), supported: true))
        XCTAssertNil(WakeText.note(device(), supported: false))
        XCTAssertNil(WakeText.note(device(canWake: true, wake: .notWoken), supported: false))
    }

    func testTheNotesWordsInBothLanguages() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        XCTAssertEqual(
            WakeText.text(.notWoken, name: "Mac mini", bundle: en),
            "Mac mini didn’t wake up. Wake for network access may be off on it, or it may not be on a wired network.")
        XCTAssertEqual(
            WakeText.text(.notWoken, name: "Mac mini", bundle: zh),
            "「Mac mini」沒有醒來。它可能沒有打開網路喚醒，或沒有接在有線網路上。")
        XCTAssertEqual(
            WakeText.text(.cannotWake, name: "Linux Laptop", bundle: en),
            "Linux Laptop hasn’t told this computer how to wake it yet. You can wake it after it connects again.")
        XCTAssertEqual(
            WakeText.text(.cannotWake, name: "Linux 筆電", bundle: zh),
            "「Linux 筆電」還沒告訴這台怎麼喚醒它；下次連上之後就能喚醒。")
        XCTAssertEqual(WakeText.helpLink(bundle: en), "How to turn on wake for network access")
        XCTAssertEqual(WakeText.helpLink(bundle: zh), "怎麼打開網路喚醒")
    }

    func testTheLinkGoesToTheUserGuideInTheAppsLanguage() {
        XCTAssertEqual(
            WakeText.helpURL(localization: "zh-Hant").absoluteString,
            "https://inputweave.ctsite.cc/zh-Hant/user-guide#wake-for-network-access")
        XCTAssertEqual(
            WakeText.helpURL(localization: "en").absoluteString,
            "https://inputweave.ctsite.cc/user-guide#wake-for-network-access")
        XCTAssertEqual(
            WakeText.helpURL(localization: nil).absoluteString,
            "https://inputweave.ctsite.cc/user-guide#wake-for-network-access")
    }

    func testTheMoonMarksAComputerThatCanBeWokenWhateverTheWaking() {
        XCTAssertTrue(WakeText.showsMoon(device(canWake: true), supported: true))
        XCTAssertTrue(WakeText.showsMoon(device(canWake: true, wake: .waking), supported: true))
        XCTAssertTrue(WakeText.showsMoon(device(canWake: true, wake: .notWoken), supported: true))
        XCTAssertFalse(WakeText.showsMoon(device(), supported: true))
        XCTAssertFalse(WakeText.showsMoon(device(.connected), supported: true))
        XCTAssertFalse(WakeText.showsMoon(device(canWake: true), supported: false))
        XCTAssertFalse(WakeText.showsMoon(nil, supported: true))
        // Beside the name, in the lock's place.
        XCTAssertEqual(ConnectionText.nameSymbol(screenLocked: false, wakeable: true), "moon.zzz")
        XCTAssertEqual(ConnectionText.nameSymbol(screenLocked: true, wakeable: false), "lock.fill")
        XCTAssertEqual(ConnectionText.nameSymbol(screenLocked: true, wakeable: true), "lock.fill")
        XCTAssertNil(ConnectionText.nameSymbol(screenLocked: false, wakeable: false))
    }

    private func status(_ devices: [DeviceStatus], supported: Bool = true) -> AgentStatus {
        var status = AgentStatus()
        status.thisDeviceID = "mac"
        status.wakeSupported = supported
        status.devices = devices
        return status
    }

    func testTheHUDTellsOfWakingAndWakingUpButNotOfTheFirstStatus() {
        var watch = WakeText.Watch()
        // Already being woken when the app connects: only remembered.
        XCTAssertNil(watch.update(status([device(canWake: true, wake: .waking)])))
        XCTAssertNil(watch.update(status([device(canWake: true, wake: .waking)])))
        XCTAssertEqual(watch.update(status([device(.connected)])), .awake(device: "mini"))
        XCTAssertNil(watch.update(status([device(.connected)])))
        // Asleep again, then woken.
        XCTAssertNil(watch.update(status([device(canWake: true)])))
        XCTAssertEqual(watch.update(status([device(canWake: true, wake: .waking)])), .waking(device: "mini"))
        // Woken again while being woken: nothing new.
        XCTAssertNil(watch.update(status([device(canWake: true, wake: .waking)])))
        // It didn't wake up, and is woken again.
        XCTAssertNil(watch.update(status([device(canWake: true, wake: .notWoken)])))
        XCTAssertEqual(watch.update(status([device(canWake: true, wake: .waking)])), .waking(device: "mini"))
        // Connecting after it didn't wake up is no news.
        XCTAssertNil(watch.update(status([device(canWake: true, wake: .notWoken)])))
        XCTAssertNil(watch.update(status([device(.connected)])))
    }

    func testTheFirstStatusAfterConnectingAgainIsOnlyRemembered() {
        var watch = WakeText.Watch()
        XCTAssertNil(watch.update(status([device(canWake: true)])))
        XCTAssertNil(watch.update(nil))
        XCTAssertNil(watch.update(status([device(canWake: true, wake: .waking)])))
        XCTAssertEqual(watch.update(status([device(.connected)])), .awake(device: "mini"))
    }

    func testAComputerNoLongerListedIsForgotten() {
        var watch = WakeText.Watch()
        XCTAssertNil(watch.update(status([device(canWake: true, wake: .waking)])))
        // Unpaired while being woken, then paired again and connected.
        XCTAssertNil(watch.update(status([])))
        XCTAssertNil(watch.update(status([device(.connected)])))
        // Listed anew while being woken: that is news.
        XCTAssertNil(watch.update(status([])))
        XCTAssertEqual(watch.update(status([device(canWake: true, wake: .waking)])), .waking(device: "mini"))
    }

    func testOneComputerAtATimeAndNeverThisOne() {
        var watch = WakeText.Watch()
        let pc = device(canWake: true, id: "pc")
        XCTAssertNil(watch.update(status([pc, device(canWake: true)])))
        let both = status([device(canWake: true, wake: .waking, id: "pc"), device(canWake: true, wake: .waking)])
        XCTAssertEqual(watch.update(both), .waking(device: "pc"))
        // This computer is not one of the others.
        var mine = WakeText.Watch()
        let mac = device(canWake: true, wake: .waking, id: "mac")
        XCTAssertNil(mine.update(status([])))
        XCTAssertNil(mine.update(status([mac])))
    }

    func testAServiceFromBeforeWakingTellsNothing() {
        var watch = WakeText.Watch()
        XCTAssertNil(watch.update(status([device(canWake: true)], supported: false)))
        XCTAssertNil(watch.update(status([device(canWake: true, wake: .waking)], supported: false)))
        XCTAssertNil(watch.update(status([device(.connected)], supported: false)))
    }

    func testTheHUDsWordsInBothLanguages() throws {
        let en = try language("en")
        let zh = try language("zh-Hant")
        XCTAssertEqual(WakeText.message(.waking(device: "mini"), name: "Mac mini", bundle: en), "Waking Mac mini…")
        XCTAssertEqual(WakeText.message(.waking(device: "mini"), name: "Mac mini", bundle: zh), "正在喚醒「Mac mini」…")
        XCTAssertEqual(WakeText.message(.awake(device: "mini"), name: "Mac mini", bundle: en), "Mac mini is awake.")
        XCTAssertEqual(WakeText.message(.awake(device: "mini"), name: "Mac mini", bundle: zh), "「Mac mini」醒來了。")
    }

    func testTheSwitchAndButtonInChinese() throws {
        let zh = try language("zh-Hant")
        func zhText(_ key: String) -> String { zh.localizedString(forKey: key, value: "missing", table: nil) }
        XCTAssertEqual(zhText("Wake"), "喚醒")
        XCTAssertEqual(zhText("Wake a sleeping computer when you push the cursor toward it"), "把游標推向睡眠中的電腦時喚醒它")
        XCTAssertEqual(
            zhText("When the cursor pushes against the edge on a sleeping computer’s side, InputWeave sends it a wake-up, at most every 30 seconds. Works only on computers with wake for network access on, usually over a wired network."),
            "游標推到睡眠中電腦那一側的螢幕邊緣時，送出網路喚醒，同一台 30 秒最多一次。只對打開網路喚醒的電腦有效，多半要接有線網路。")
    }
}

/// The computers this app asked to wake.
final class WakeRequestsTests: XCTestCase {
    func testKeptFromAskingUntilAnsweredOrNoLongerListed() {
        var requests = WakeRequests()
        XCTAssertFalse(requests.contains("mini"))
        requests.begin("mini")
        requests.begin("pc")
        XCTAssertTrue(requests.contains("mini"))
        requests.end("mini")
        XCTAssertFalse(requests.contains("mini"))
        XCTAssertTrue(requests.contains("pc"))
        requests.begin("mini")
        requests.keep(["mini"])
        XCTAssertFalse(requests.contains("pc"))
        XCTAssertTrue(requests.contains("mini"))
        requests.keep([String]())
        XCTAssertEqual(requests, WakeRequests())
    }
}
