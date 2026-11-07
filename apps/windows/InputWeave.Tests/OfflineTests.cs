using System.Globalization;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Overlay;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using InputWeave.Tray;
using Xunit;

namespace InputWeave.Tests;

/// <summary>
/// What the app shows and offers for computers taken offline for now, from
/// the service's offline fields: the same rules as on the Mac and Linux.
/// </summary>
public class OfflineForNowTests
{
    private static readonly CultureInfo En = CultureInfo.GetCultureInfo("en-US");

    /// <summary>The MacBook Pro taken offline for now by <paramref name="by"/>, which the service shows offline.</summary>
    private static AgentSnapshot MacOffline(string by = "pc", bool supported = true) =>
        Sample.Model(macOnline: false, offlineSupported: supported, macOfflineBy: by);

    /// <summary>This computer taken offline for now by <paramref name="by"/>: every other one shows plainly offline.</summary>
    private static AgentSnapshot ThisOffline(string by = "pc", bool sharing = true) =>
        Sample.Model(macOnline: false, offlineSupported: true, thisOfflineBy: by, sharing: sharing);

    private static UiText Status(AgentSnapshot model, string id = "mac") => DeviceText.Status(model, id, Sample.Now, En);

    /// <summary>A connected Mac mini still on InputWeave 1.0, beside the MacBook Pro.</summary>
    private static AgentSnapshot WithUnaware(AgentSnapshot model, bool macToo = false) => WakeTests.With(model, s =>
    {
        s.Devices.Add(new DeviceStatus { Id = "mini", Name = "Mac mini", State = ConnectionState.Connected, OfflineUnaware = true });
        if (macToo)
        {
            s.Devices[0].OfflineUnaware = true;
        }
    });

    [Fact]
    public void TheStatusSaysOfflineForNowBeforeAnythingElse()
    {
        Assert.Equal(new UiText(TextKeys.ConnectionOfflineForNow), Status(MacOffline()));
        // Before what would say it can be woken.
        var asleep = WakeTests.With(MacOffline(), s =>
        {
            s.WakeSupported = true;
            s.Devices[0].Wake = WakeState.Waking;
            s.Devices[0].OfflineSinceUnixMs = 1;
        });
        Assert.Equal(new UiText(TextKeys.ConnectionOfflineForNow), Status(asleep));
        // This computer is still this computer.
        Assert.Equal(new UiText(TextKeys.ConnectionThisComputer), Status(ThisOffline(), "pc"));
        // While this one is offline for now, the others are just offline.
        Assert.Equal(new UiText(TextKeys.ConnectionOffline), Status(ThisOffline()));
        // A service from before says nothing of it.
        Assert.Equal(new UiText(TextKeys.ConnectionOffline), Status(MacOffline(supported: false)));
        Assert.False(OfflineForNow.IsOffline(MacOffline(supported: false), "mac"));
    }

    [Fact]
    public void SaysWhoTookItOffline()
    {
        Assert.Equal(new UiText(TextKeys.OfflineSetHere), OfflineForNow.SetBy(MacOffline(), "mac"));
        Assert.Equal(new UiText(TextKeys.OfflineSetBy, "Mac mini"), OfflineForNow.SetBy(MacOffline(by: "mini"), "mac"));
        Assert.Null(OfflineForNow.SetBy(Sample.Model(offlineSupported: true), "mac"));
        Assert.Null(OfflineForNow.SetBy(MacOffline(), "pc"));
    }

    [Fact]
    public void AnUnknownSetterIsAnotherComputer()
    {
        // The service doesn't know which computer did it: no id, no name.
        var unknown = WakeTests.With(MacOffline(by: "mini"), s => (s.Devices[0].OfflineByDeviceId, s.Devices[0].OfflineByName) = ("", ""));
        Assert.Equal(new UiText(TextKeys.OfflineSetByAnother), OfflineForNow.SetBy(unknown, "mac"));
        // An empty id is never this computer, even before the service names this one.
        var unnamedHere = WakeTests.With(unknown, s => s.ThisDeviceId = "");
        Assert.Equal(new UiText(TextKeys.OfflineSetByAnother), OfflineForNow.SetBy(unnamedHere, "mac"));
        // A name without an id: the name.
        var nameOnly = WakeTests.With(unknown, s => s.Devices[0].OfflineByName = "Old Laptop");
        Assert.Equal(new UiText(TextKeys.OfflineSetBy, "Old Laptop"), OfflineForNow.SetBy(nameOnly, "mac"));
        // An id without a name: still another computer.
        var idOnly = WakeTests.With(unknown, s => s.Devices[0].OfflineByDeviceId = "mini");
        Assert.Equal(new UiText(TextKeys.OfflineSetByAnother), OfflineForNow.SetBy(idOnly, "mac"));

        // This computer, in the panel.
        var here = WakeTests.With(ThisOffline(by: "mac"), s => (s.OfflineByDeviceId, s.OfflineByName) = ("", ""));
        Assert.Equal(new UiText(TextKeys.SubtitleThisOfflineByAnother), PanelText.Subtitle(here));
        // An id without a name, even one this app knows by a name: never the id, nor a name looked up.
        Assert.Equal(
            new UiText(TextKeys.SubtitleThisOfflineByAnother),
            PanelText.Subtitle(WakeTests.With(here, s => s.OfflineByDeviceId = "mac")));
        Assert.Equal(
            new UiText(TextKeys.SubtitleThisOfflineBy, "Old Laptop"),
            PanelText.Subtitle(WakeTests.With(here, s => s.OfflineByName = "Old Laptop")));
        Assert.Equal(new UiText(TextKeys.TitleThisOffline), PanelText.Title(here, ServiceState.Running, Sample.Now));
    }

    [Fact]
    public void WarnsOfTheOtherOldComputersOnlyWhenTheOneShownIsOldToo()
    {
        // The one shown on 1.0 itself, offline for now: the other 1.0
        // computer still controls it. The service says it is on 1.0 while it
        // is connected, even when it shows offline for being offline for now.
        Assert.Equal(
            [new UiText(TextKeys.OfflineUnaware, "Mac mini")],
            OfflineForNow.Unaware(WithUnaware(MacOffline(), macToo: true), "mac"));
        // The one shown on 1.1: it keeps the 1.0 computer away by itself.
        Assert.Empty(OfflineForNow.Unaware(WithUnaware(MacOffline()), "mac"));
        // The one shown the only one on 1.0: the 1.1 computers keep away from it.
        var alone = WakeTests.With(MacOffline(), s => s.Devices[0].OfflineUnaware = true);
        Assert.Empty(OfflineForNow.Unaware(alone, "mac"));
        // Only while the one shown is offline for now.
        Assert.Empty(OfflineForNow.Unaware(WithUnaware(Sample.Model(offlineSupported: true), macToo: true), "mac"));
        Assert.Empty(OfflineForNow.Unaware(MacOffline(), "mac"));
    }

    [Fact]
    public void OffersTakingAnotherOfflineOrBringingItBack()
    {
        var online = Sample.Model(offlineSupported: true);
        Assert.True(OfflineForNow.Offered(online, "mac"));
        Assert.Equal(TextKeys.OfflineTake, OfflineForNow.ActionKey(online, "mac"));
        Assert.True(OfflineForNow.Offered(MacOffline(), "mac"));
        Assert.Equal(TextKeys.OfflineBringBack, OfflineForNow.ActionKey(MacOffline(), "mac"));
        // An offline computer can be taken offline for now too.
        Assert.True(OfflineForNow.Offered(Sample.Model(macOnline: false, offlineSupported: true), "mac"));
        // Not this computer, nor one not trusted here, nor from a service from before.
        Assert.False(OfflineForNow.Offered(online, "pc"));
        Assert.False(OfflineForNow.Offered(online, "ghost"));
        Assert.False(OfflineForNow.Offered(Sample.Model(), "mac"));
        // While this one is offline for now, it can only bring itself back.
        Assert.False(OfflineForNow.Offered(ThisOffline(), "mac"));
    }

    [Fact]
    public void WhileThisComputerIsOfflineTheOthersOnlyHaveRename()
    {
        Assert.True(OfflineForNow.OnlyRename(ThisOffline(), "mac"));
        // Its own menu stays.
        Assert.False(OfflineForNow.OnlyRename(ThisOffline(), "pc"));
        Assert.False(OfflineForNow.OnlyRename(MacOffline(), "mac"));
        Assert.False(OfflineForNow.OnlyRename(Sample.Model(offlineSupported: true), "mac"));
    }

    [Fact]
    public void NothingOfWakingForAComputerCutOff()
    {
        // Taken offline for now: the service says it can't be woken, which
        // is no reason to say how to wake it, and it is not being woken.
        var offline = WakeTests.With(MacOffline(), s =>
        {
            s.WakeSupported = true;
            s.Devices[0].Wake = WakeState.Waking;
        });
        Assert.Equal(WakeNote.None, Wake.Note(offline, "mac"));
        Assert.False(Wake.Waking(offline, "mac"));
        Assert.False(Wake.Offered(offline, "mac"));
        Assert.Null(Wake.OfflineStatus(offline, "mac", Sample.Now, En));
        // Every other one, while this computer is offline for now.
        var cutOff = WakeTests.With(ThisOffline(), s => s.WakeSupported = true);
        Assert.Equal(WakeNote.None, Wake.Note(cutOff, "mac"));
        // Plainly offline, it still says so.
        Assert.Equal(WakeNote.NotTold, Wake.Note(Sample.Model(macOnline: false, wakeSupported: true, offlineSupported: true), "mac"));
    }

    [Fact]
    public void FilesDraggedOverOneOfflineForNowSaySo()
    {
        Assert.Equal(new UiText(TextKeys.ConnectionOfflineForNow), DeviceText.CannotReceive(MacOffline(), "mac", compact: true));
        Assert.Equal(new UiText(TextKeys.ConnectionOffline), DeviceText.CannotReceive(ThisOffline(), "mac", compact: true));
    }

    [Fact]
    public void ProblemsLeaveOutComputersOfflineOnPurpose()
    {
        var unreachable = new ExceptionRow(new UiText(TextKeys.ExceptionUnreachable, "MacBook Pro"), ExceptionAction.Troubleshoot);
        Assert.Equal([unreachable], ExceptionRows.For(Sample.Model(macOnline: false, offlineSupported: true), ServiceState.Running));
        Assert.Empty(ExceptionRows.For(MacOffline(), ServiceState.Running));
        // While this one is offline for now, none of the others is a problem.
        Assert.Empty(ExceptionRows.For(ThisOffline(), ServiceState.Running));
        Assert.Empty(ExceptionRows.For(ThisOffline(), ServiceState.Running, compact: false));
        var capturing = WakeTests.With(MacOffline(), s => s.Capturing = true);
        Assert.True(ExceptionRows.EverythingWorks(capturing, ServiceState.Running));
        Assert.True(ExceptionRows.EverythingWorks(WakeTests.With(ThisOffline(), s => s.Capturing = true), ServiceState.Running));
        // An offline one taken offline for now beside one that can't be reached.
        var both = WakeTests.With(capturing, s => s.Devices.Add(new DeviceStatus { Id = "mini", Name = "Mac mini", State = ConnectionState.Offline }));
        Assert.Equal(
            [new ExceptionRow(new UiText(TextKeys.ExceptionUnreachable, "Mac mini"), ExceptionAction.Troubleshoot)],
            ExceptionRows.For(both, ServiceState.Running));
        Assert.False(ExceptionRows.EverythingWorks(both, ServiceState.Running));
        // From a service from before, all of them count.
        Assert.Equal([unreachable], ExceptionRows.For(MacOffline(supported: false), ServiceState.Running));
    }

    [Fact]
    public void TheMenuTakesThisComputerOfflineOrBringsItBack()
    {
        Assert.Null(MenuState.For(Sample.Model()).OfflineKey);
        var online = MenuState.For(Sample.Model(offlineSupported: true));
        Assert.Equal(TextKeys.MenuTakeThisOffline, online.OfflineKey);
        Assert.True(online.OfflineEnabled);
        Assert.True(online.LockEnabled);
        var offline = MenuState.For(ThisOffline());
        Assert.Equal(TextKeys.OfflineBringBack, offline.OfflineKey);
        Assert.True(offline.OfflineEnabled);
        // Offline for now, the cursor goes nowhere: no locking it.
        Assert.False(offline.LockEnabled);
        Assert.False(MenuState.For(ThisOffline(by: "mac")).LockEnabled);
        Assert.False(MenuState.For(Sample.Model(AgentLink.Connecting, offlineSupported: true)).OfflineEnabled);
        // Another computer offline for now changes nothing here.
        Assert.Equal(
            new MenuState(TextKeys.MenuPauseSharing, true, TextKeys.MenuLock, true, true, TextKeys.MenuTakeThisOffline, true),
            MenuState.For(MacOffline()));
    }

    [Fact]
    public void ThePanelSaysThisComputerIsOfflineAndWhoSetIt()
    {
        var title = new UiText(TextKeys.TitleThisOffline);
        Assert.Equal(title, PanelText.Title(ThisOffline(), ServiceState.Running, Sample.Now));
        Assert.Equal(new UiText(TextKeys.SubtitleThisOfflineHere), PanelText.Subtitle(ThisOffline()));
        Assert.Equal(new UiText(TextKeys.SubtitleThisOfflineBy, "MacBook Pro"), PanelText.Subtitle(ThisOffline(by: "mac")));
        // Before paused sharing, which the switch still shows.
        Assert.Equal(title, PanelText.Title(ThisOffline(sharing: false), ServiceState.Running, Sample.Now));
        Assert.Equal(new UiText(TextKeys.SubtitleThisOfflineHere), PanelText.Subtitle(ThisOffline(sharing: false)));
        // Another computer offline for now: as usual.
        Assert.Equal(new UiText(TextKeys.TitleSharing), PanelText.Title(MacOffline(), ServiceState.Running, Sample.Now));
        Assert.Null(OfflineForNow.PanelSubtitle(MacOffline()));
        // Until the service answers, what it last said is not news.
        Assert.Equal(new UiText(TextKeys.TitleStarting), PanelText.Title(ThisOffline() with { Link = AgentLink.Connecting }, ServiceState.Running, Sample.Now));
    }

    [Fact]
    public void TheTrayIconIsPausedAndSaysWhy()
    {
        Assert.Equal(TrayIconState.Paused, TrayIcons.StateFor(ThisOffline(), ServiceState.Running, Sample.Now));
        Assert.Equal(new UiText(TextKeys.TrayOfflineForNow), TrayIcons.Description(TrayIconState.Paused, ThisOffline()));
        Assert.Equal(new UiText(TextKeys.TrayOfflineForNow), TrayIcons.Description(TrayIconState.Paused, ThisOffline(sharing: false)));
        Assert.Equal(new UiText(TextKeys.TrayPaused), TrayIcons.Description(TrayIconState.Paused, Sample.Model(sharing: false, offlineSupported: true)));
        Assert.Equal(new UiText(TextKeys.TrayPaused), TrayIcons.Description(TrayIconState.Paused));
        // Another computer offline for now changes nothing here.
        Assert.Equal(TrayIconState.Normal, TrayIcons.StateFor(MacOffline(), ServiceState.Running, Sample.Now));
        Assert.Equal(new UiText(TextKeys.TrayNormal), TrayIcons.Description(TrayIconState.Normal, MacOffline()));
    }

    [Fact]
    public void ReadsAsAgreedInBothLanguages()
    {
        var en = WakeTexts.Load("en-US");
        var zh = WakeTexts.Load("zh-TW");
        void Same(string english, string chinese, UiText text)
        {
            Assert.Equal(english, WakeTexts.Say(en, text));
            Assert.Equal(chinese, WakeTexts.Say(zh, text));
        }
        Same("Offline for now", "暫時離線", Status(MacOffline()));
        Same("Set by Mac mini.", "由「Mac mini」設定。", OfflineForNow.SetBy(MacOffline(by: "mini"), "mac")!);
        Same("Set on this computer.", "由這台設定。", OfflineForNow.SetBy(MacOffline(), "mac")!);
        Same(
            "Mac mini runs an older InputWeave that doesn’t know about this and still controls it. Update Mac mini to 1.1.",
            "「Mac mini」的 InputWeave 是舊版，不認得暫時離線，仍會控制它。請把「Mac mini」更新到 1.1。",
            OfflineForNow.Unaware(WithUnaware(MacOffline(), macToo: true), "mac")[0]);
        Same("Take Offline", "暫時離線", new UiText(TextKeys.OfflineTake));
        Same("Bring Back Online", "恢復連線", new UiText(TextKeys.OfflineBringBack));
        Same(
            "None of your computers control it or share the clipboard and files with it until someone brings it back. It can bring itself back too.",
            "所有電腦都不會控制它，也不會和它同步剪貼簿或互傳檔案，直到有人恢復連線。它自己也能恢復。",
            new UiText(TextKeys.OfflineHint));
        Same("Take This Computer Offline", "讓這台暫時離線", new UiText(TextKeys.MenuTakeThisOffline));
        Same("This computer is offline for now", "這台暫時離線", new UiText(TextKeys.TitleThisOffline));
        Same(
            "Set by MacBook Pro. Your other computers don’t control this one or share the clipboard and files with it.",
            "由「MacBook Pro」設定。其他電腦不會控制這台，也不會和它同步剪貼簿或互傳檔案。",
            PanelText.Subtitle(ThisOffline(by: "mac"))!);
        Same(
            "Set on this computer. Your other computers don’t control this one or share the clipboard and files with it.",
            "由這台設定。其他電腦不會控制這台，也不會和它同步剪貼簿或互傳檔案。",
            PanelText.Subtitle(ThisOffline())!);
        Same("InputWeave, this computer is offline for now", "InputWeave，這台暫時離線", new UiText(TextKeys.TrayOfflineForNow));
        Same("Windows Desktop is offline for now.", "「Windows Desktop」暫時離線。", new UiText(TextKeys.HudOfflineForNow, "Windows Desktop"));
        Same("Windows Desktop is back online.", "「Windows Desktop」已恢復連線。", new UiText(TextKeys.HudBackOnline, "Windows Desktop"));
        Same(
            "Windows Desktop went offline for now. The keyboard and mouse are back on this computer.",
            "「Windows Desktop」暫時離線，鍵鼠已回到這台。",
            new UiText(TextKeys.HudOfflineReturned, "Windows Desktop"));
        Same(
            "MacBook Pro took this computer offline for now.",
            "「MacBook Pro」把這台設為暫時離線。",
            new UiText(TextKeys.HudTookThisOffline, "MacBook Pro"));
        Same("Set by another computer.", "由另一台電腦設定。", new UiText(TextKeys.OfflineSetByAnother));
        Same(
            "Set by another computer. Your other computers don’t control this one or share the clipboard and files with it.",
            "由另一台電腦設定。其他電腦不會控制這台，也不會和它同步剪貼簿或互傳檔案。",
            new UiText(TextKeys.SubtitleThisOfflineByAnother));
        Same(
            "Another computer took this computer offline for now.",
            "另一台電腦把這台設為暫時離線。",
            new UiText(TextKeys.HudTookThisOfflineAnother));
    }
}

/// <summary>The HUD as computers are taken offline for now or brought back.</summary>
public class OfflineHudTests
{
    private static UiText? Update(HudWatch watch, AgentSnapshot model)
    {
        int hints = HudWatch.HintTimes;
        return watch.Update(model, ref hints);
    }

    /// <summary>
    /// A state after <paramref name="changes"/> changes, the last one
    /// <paramref name="device"/> taken offline (or brought back) by <paramref name="by"/>.
    /// </summary>
    private static AgentSnapshot Changed(
        ulong changes, string device = "mac", bool offline = true, string by = "pc", string byName = "", Action<Status>? also = null) =>
        WakeTests.With(Sample.Model(offlineSupported: true), s =>
        {
            s.OfflineChanges = changes;
            s.LastOfflineChange = changes == 0
                ? null
                : new OfflineChange { DeviceId = device, Offline = offline, ByDeviceId = by, ByName = byName };
            also?.Invoke(s);
        });

    [Fact]
    public void TellsTheComputerThatDidIt()
    {
        var watch = new HudWatch();
        Assert.Null(Update(watch, Changed(0)));
        Assert.Equal(new UiText(TextKeys.HudOfflineForNow, "MacBook Pro"), Update(watch, Changed(1)));
        // The same count again is no news.
        Assert.Null(Update(watch, Changed(1)));
        Assert.Equal(new UiText(TextKeys.HudBackOnline, "MacBook Pro"), Update(watch, Changed(2, offline: false)));
        // This computer, by its own name; brought back by itself, nothing.
        Assert.Equal(new UiText(TextKeys.HudOfflineForNow, "Windows Desktop"), Update(watch, Changed(3, device: "pc")));
        Assert.Null(Update(watch, Changed(4, device: "pc", offline: false)));
    }

    [Fact]
    public void TellsThisComputerWhoTookItOffline()
    {
        var watch = new HudWatch();
        Update(watch, Changed(0));
        var taken = Changed(1, device: "pc", by: "mac", byName: "Work Mac", also: s =>
        {
            s.OfflineForNow = true;
            s.OfflineByDeviceId = "mac";
            s.OfflineByName = "Work Mac";
        });
        // By the name the service gave it as the change came.
        Assert.Equal(new UiText(TextKeys.HudTookThisOffline, "Work Mac"), Update(watch, taken));
        Assert.Null(Update(watch, taken));
        // Whatever the status says by the time it is told: renamed since,
        // or no name any more.
        var renamed = Changed(2, device: "pc", by: "mac", byName: "Work Mac", also: s =>
        {
            s.OfflineForNow = true;
            s.OfflineByDeviceId = "mac";
            s.OfflineByName = "Home Mac";
        });
        Assert.Equal(new UiText(TextKeys.HudTookThisOffline, "Work Mac"), Update(watch, renamed));
        var nameless = Changed(3, device: "pc", by: "mac", byName: "Work Mac", also: s =>
        {
            s.OfflineForNow = true;
            s.OfflineByDeviceId = "mac";
            s.OfflineByName = "";
        });
        Assert.Equal(new UiText(TextKeys.HudTookThisOffline, "Work Mac"), Update(watch, nameless));
        // The change without a name: another computer, whatever the status says.
        var unnamedChange = Changed(4, device: "pc", by: "mac", also: s =>
        {
            s.OfflineForNow = true;
            s.OfflineByDeviceId = "mac";
            s.OfflineByName = "Home Mac";
        });
        Assert.Equal(new UiText(TextKeys.HudTookThisOfflineAnother), Update(watch, unnamedChange));
    }

    [Fact]
    public void SaysAnotherComputerWhenTheServiceDoesNotKnowWhich()
    {
        var watch = new HudWatch();
        Update(watch, Changed(0));
        var taken = Changed(1, device: "pc", by: "", also: s => s.OfflineForNow = true);
        Assert.Equal(new UiText(TextKeys.HudTookThisOfflineAnother), Update(watch, taken));
        // A name without an id: the name.
        var named = Changed(2, device: "pc", by: "", byName: "Old Laptop", also: s =>
        {
            s.OfflineForNow = true;
            s.OfflineByName = "Old Laptop";
        });
        Assert.Equal(new UiText(TextKeys.HudTookThisOffline, "Old Laptop"), Update(watch, named));

        // An empty id is never this computer, even before the service names this one.
        var early = new HudWatch();
        var start = WakeTests.With(Changed(0), s => s.ThisDeviceId = "");
        Update(early, start);
        var unknown = WakeTests.With(Changed(1, device: "", by: ""), s => s.ThisDeviceId = "");
        Assert.Equal(new UiText(TextKeys.HudTookThisOfflineAnother), Update(early, unknown));
    }

    [Fact]
    public void TheKeyboardAndMouseComingBackSaysSoInstead()
    {
        var watch = new HudWatch();
        Update(watch, Changed(0, also: s => s.ActiveDeviceId = "mac"));
        // Taken offline from here while controlling it.
        var back = Changed(1, also: s =>
        {
            s.ControlReturns = 1;
            s.LastReturn = ReturnReason.OfflineForNow;
            s.LastReturnDeviceId = "mac";
        });
        Assert.Equal(new UiText(TextKeys.HudOfflineReturned, "MacBook Pro"), Update(watch, back));
        // Nothing left behind for the same change.
        Assert.Null(Update(watch, back));
        // Taken offline by another computer: only the return, from the one controlling it.
        var byOther = WakeTests.With(back, s =>
        {
            s.ControlReturns = 2;
            s.LastReturnDeviceId = "mac";
        });
        Assert.Equal(new UiText(TextKeys.HudOfflineReturned, "MacBook Pro"), Update(watch, byOther));
    }

    [Fact]
    public void ThisComputerTakenOfflineWhileControllingAnotherSaysWhatHappenedToIt()
    {
        var watch = new HudWatch();
        Update(watch, Changed(0, also: s => s.ActiveDeviceId = "mac"));
        var taken = Changed(1, device: "pc", by: "mac", byName: "MacBook Pro", also: s =>
        {
            s.OfflineForNow = true;
            s.OfflineByDeviceId = "mac";
            s.OfflineByName = "MacBook Pro";
            s.ControlReturns = 1;
            s.LastReturn = ReturnReason.OfflineForNow;
            s.LastReturnDeviceId = "pc";
        });
        Assert.Equal(new UiText(TextKeys.HudTookThisOffline, "MacBook Pro"), Update(watch, taken));

        // Taking itself offline, the same: its own name.
        var self = new HudWatch();
        Update(self, Changed(0, also: s => s.ActiveDeviceId = "mac"));
        var itself = Changed(1, device: "pc", also: s =>
        {
            s.ControlReturns = 1;
            s.LastReturn = ReturnReason.OfflineForNow;
            s.LastReturnDeviceId = "pc";
        });
        Assert.Equal(new UiText(TextKeys.HudOfflineForNow, "Windows Desktop"), Update(self, itself));
    }

    [Fact]
    public void TheFirstStateOnlyRecords()
    {
        var watch = new HudWatch();
        Assert.Null(Update(watch, Changed(3)));
        Assert.Equal(new UiText(TextKeys.HudBackOnline, "MacBook Pro"), Update(watch, Changed(4, offline: false)));

        // The service reached again: what it says first is no news.
        Assert.Null(Update(watch, Sample.Model(AgentLink.Connecting)));
        Assert.Null(Update(watch, Changed(5)));
        Assert.Equal(new UiText(TextKeys.HudBackOnline, "MacBook Pro"), Update(watch, Changed(6, offline: false)));
        // Started again without the app noticing: a lower count only records.
        Assert.Null(Update(watch, Changed(1)));
        Assert.Equal(new UiText(TextKeys.HudBackOnline, "MacBook Pro"), Update(watch, Changed(2, offline: false)));
    }

    [Fact]
    public void AServiceFromBeforeSaysNothing()
    {
        var watch = new HudWatch();
        Update(watch, WakeTests.With(Changed(0), s => s.OfflineSupported = false));
        Assert.Null(Update(watch, WakeTests.With(Changed(1), s => s.OfflineSupported = false)));
    }

    [Fact]
    public void TheLockComesFirstAndNothingIsLeftBehind()
    {
        var watch = new HudWatch();
        Update(watch, Changed(0));
        // Locked in the same state another computer is taken offline from
        // here: the lock is told, as in the other apps, and the computer
        // taken offline is not told late.
        var both = Changed(1, also: s => s.Locked = true);
        Assert.Equal(new UiText(TextKeys.HudLockedHere, "Ctrl + Alt + Shift + L"), Update(watch, both));
        Assert.Null(Update(watch, both));
    }

    [Fact]
    public void ComesBeforeWaking()
    {
        var watch = new HudWatch();
        var mini = new DeviceStatus { Id = "mini", Name = "Mac mini", State = ConnectionState.Offline, CanWake = true };
        Update(watch, Changed(0, also: s =>
        {
            s.WakeSupported = true;
            s.Devices.Add(mini.Clone());
        }));
        // The MacBook Pro taken offline from here as the Mac mini starts being woken.
        var both = Changed(1, also: s =>
        {
            s.WakeSupported = true;
            var waking = mini.Clone();
            waking.Wake = WakeState.Waking;
            s.Devices.Add(waking);
        });
        Assert.Equal(new UiText(TextKeys.HudOfflineForNow, "MacBook Pro"), Update(watch, both));
        // Waking is not told late.
        Assert.Null(Update(watch, both));
    }
}
