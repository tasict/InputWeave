using System.Collections.Immutable;
using System.Globalization;
using System.Xml.Linq;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Overlay;
using InputWeave.Panel;
using InputWeave.Settings;
using Xunit;

namespace InputWeave.Tests;

/// <summary>How long ago, as the clipboard line and an offline computer's status say it.</summary>
public class AgoTests
{
    private static readonly CultureInfo En = CultureInfo.GetCultureInfo("en-US");

    private static UiText Ago(TimeSpan elapsed) => ClipboardText.Ago(elapsed, En);

    [Fact]
    public void JustNowUnderAMinute()
    {
        Assert.Equal(new UiText(TextKeys.RelativeJustNow), Ago(TimeSpan.Zero));
        Assert.Equal(new UiText(TextKeys.RelativeJustNow), Ago(TimeSpan.FromSeconds(59.9)));
        // A clock a little behind the other computer's: just now.
        Assert.Equal(new UiText(TextKeys.RelativeJustNow), Ago(TimeSpan.FromMinutes(-5)));
    }

    [Fact]
    public void MinutesUnderAnHourRoundedDown()
    {
        Assert.Equal(new UiText(TextKeys.AgoMinute), Ago(TimeSpan.FromSeconds(60)));
        Assert.Equal(new UiText(TextKeys.AgoMinute), Ago(TimeSpan.FromSeconds(119)));
        Assert.Equal(new UiText(TextKeys.AgoMinutes, "2"), Ago(TimeSpan.FromSeconds(120)));
        Assert.Equal(new UiText(TextKeys.AgoMinutes, "59"), Ago(TimeSpan.FromMinutes(60) - TimeSpan.FromSeconds(1)));
    }

    [Fact]
    public void HoursUnderADayRoundedDown()
    {
        Assert.Equal(new UiText(TextKeys.AgoHour), Ago(TimeSpan.FromMinutes(60)));
        Assert.Equal(new UiText(TextKeys.AgoHour), Ago(TimeSpan.FromMinutes(119)));
        Assert.Equal(new UiText(TextKeys.AgoHours, "2"), Ago(TimeSpan.FromHours(2)));
        Assert.Equal(new UiText(TextKeys.AgoHours, "23"), Ago(TimeSpan.FromHours(24) - TimeSpan.FromSeconds(1)));
    }

    [Fact]
    public void DaysAfterThat()
    {
        Assert.Equal(new UiText(TextKeys.AgoDay), Ago(TimeSpan.FromHours(24)));
        Assert.Equal(new UiText(TextKeys.AgoDay), Ago(TimeSpan.FromHours(47.9)));
        Assert.Equal(new UiText(TextKeys.AgoDays, "2"), Ago(TimeSpan.FromHours(48)));
        Assert.Equal(new UiText(TextKeys.AgoDays, "1000"), Ago(TimeSpan.FromDays(1000.5)));
    }

    [Fact]
    public void TheClipboardLineKeepsItsBriefMinutes()
    {
        Assert.Equal(new UiText(TextKeys.RelativeJustNow), ClipboardText.Ago(TimeSpan.FromSeconds(30), En, brief: true));
        Assert.Equal(new UiText(TextKeys.RelativeMinutesAgo, "1"), ClipboardText.Ago(TimeSpan.FromSeconds(90), En, brief: true));
        Assert.Equal(new UiText(TextKeys.RelativeMinutesAgo, "9"), ClipboardText.Ago(TimeSpan.FromMinutes(9.9), En, brief: true));
    }

    [Fact]
    public void ReadsAsAgreedInBothLanguages()
    {
        var en = WakeTexts.Load("en-US");
        var zh = WakeTexts.Load("zh-TW");
        Assert.Equal("just now", WakeTexts.Say(en, Ago(TimeSpan.FromSeconds(5))));
        Assert.Equal("剛剛", WakeTexts.Say(zh, Ago(TimeSpan.FromSeconds(5))));
        Assert.Equal("1 minute ago", WakeTexts.Say(en, Ago(TimeSpan.FromMinutes(1))));
        Assert.Equal("1 分鐘前", WakeTexts.Say(zh, Ago(TimeSpan.FromMinutes(1))));
        Assert.Equal("5 minutes ago", WakeTexts.Say(en, Ago(TimeSpan.FromMinutes(5))));
        Assert.Equal("5 分鐘前", WakeTexts.Say(zh, Ago(TimeSpan.FromMinutes(5))));
        Assert.Equal("1 hour ago", WakeTexts.Say(en, Ago(TimeSpan.FromHours(1))));
        Assert.Equal("1 小時前", WakeTexts.Say(zh, Ago(TimeSpan.FromHours(1))));
        Assert.Equal("2 hours ago", WakeTexts.Say(en, Ago(TimeSpan.FromHours(2))));
        Assert.Equal("2 小時前", WakeTexts.Say(zh, Ago(TimeSpan.FromHours(2))));
        Assert.Equal("1 day ago", WakeTexts.Say(en, Ago(TimeSpan.FromDays(1))));
        Assert.Equal("1 天前", WakeTexts.Say(zh, Ago(TimeSpan.FromDays(1))));
        Assert.Equal("3 days ago", WakeTexts.Say(en, Ago(TimeSpan.FromDays(3))));
        Assert.Equal("3 天前", WakeTexts.Say(zh, Ago(TimeSpan.FromDays(3))));
        // The clipboard's words stay as they were.
        Assert.Equal("3 min ago", WakeTexts.Say(en, ClipboardText.Ago(TimeSpan.FromMinutes(3), En, brief: true)));
        Assert.Equal("3 分鐘前", WakeTexts.Say(zh, ClipboardText.Ago(TimeSpan.FromMinutes(3), En, brief: true)));
    }
}

/// <summary>The app's strings in one language, put together as the app does, for the tests.</summary>
internal static class WakeTexts
{
    public static Dictionary<string, string> Load(string language)
    {
        var file = Path.Combine(AppContext.BaseDirectory, "Strings", language, "Resources.resw");
        return XDocument.Load(file).Root!.Elements("data")
            .ToDictionary(d => (string)d.Attribute("name")!, d => (string)d.Element("value")!);
    }

    public static string Say(Dictionary<string, string> strings, UiText text) =>
        string.Format(
            CultureInfo.InvariantCulture,
            strings[text.Key],
            text.Args.Select(a => a is UiText inner ? Say(strings, inner) : a).ToArray());
}

/// <summary>
/// What the app offers for waking another computer, from the service's
/// can_wake and wake: the same rules as on the Mac and Linux.
/// </summary>
public class WakeTests
{
    private static readonly CultureInfo En = CultureInfo.GetCultureInfo("en-US");

    private static readonly ulong TwoHoursAgo = (ulong)Sample.Now.AddHours(-2).ToUnixTimeMilliseconds();

    /// <summary>The Mac offline, from a service that can wake computers.</summary>
    private static AgentSnapshot Asleep(bool canWake = true, WakeState wake = WakeState.None, ulong since = 0, bool supported = true) =>
        Sample.Model(macOnline: false, wakeSupported: supported, macCanWake: canWake, macWake: wake, macOfflineSince: since);

    private static UiText Status(AgentSnapshot model, string id = "mac") => DeviceText.Status(model, id, Sample.Now, En);

    [Fact]
    public void SaysSinceWhenAComputerThatCanBeWokenIsOffline()
    {
        var since = new UiText(TextKeys.ConnectionOfflineSince, new UiText(TextKeys.AgoHours, "2"));
        Assert.Equal(since, Status(Asleep(since: TwoHoursAgo)));
        // Not woken: still since when.
        Assert.Equal(since, Status(Asleep(wake: WakeState.NotWoken, since: TwoHoursAgo)));
        Assert.Equal(
            new UiText(TextKeys.ConnectionOfflineSince, new UiText(TextKeys.RelativeJustNow)),
            Status(Asleep(since: (ulong)Sample.Now.AddSeconds(-20).ToUnixTimeMilliseconds())));
        // Not known since when: plain offline.
        Assert.Equal(new UiText(TextKeys.ConnectionOffline), Status(Asleep(since: 0)));
        // One that can't be woken says nothing of when, even known.
        Assert.Equal(new UiText(TextKeys.ConnectionOffline), Status(Asleep(canWake: false, since: TwoHoursAgo)));
        // A time past anything a clock shows is just now, not a crash.
        Assert.Equal(
            new UiText(TextKeys.ConnectionOfflineSince, new UiText(TextKeys.RelativeJustNow)),
            Status(Asleep(since: ulong.MaxValue)));
    }

    [Fact]
    public void SaysWakingWhileItIsBeingWoken()
    {
        Assert.Equal(new UiText(TextKeys.ConnectionWaking), Status(Asleep(wake: WakeState.Waking, since: TwoHoursAgo)));
        Assert.True(Wake.Waking(Asleep(wake: WakeState.Waking), "mac"));
        Assert.False(Wake.Waking(Asleep(wake: WakeState.NotWoken), "mac"));
        Assert.False(Wake.Waking(Asleep(), "mac"));
        // Connected, it says what it always did.
        var awake = Sample.Model(wakeSupported: true, macWake: WakeState.Waking);
        Assert.Equal(new UiText(TextKeys.ConnectionConnected), Status(awake));
        Assert.False(Wake.Waking(awake, "mac"));
        Assert.Equal(new UiText(TextKeys.ConnectionThisComputer), Status(Asleep(), "pc"));
    }

    [Fact]
    public void TheMoonStandsBeforeTheStatusUnlessTheSpinnerDoes()
    {
        Assert.True(Wake.MoonBeforeStatus(Asleep(), "mac"));
        Assert.True(Wake.MoonBeforeStatus(Asleep(wake: WakeState.NotWoken), "mac"));
        Assert.False(Wake.MoonBeforeStatus(Asleep(wake: WakeState.Waking), "mac"));
        Assert.False(Wake.MoonBeforeStatus(Asleep(canWake: false), "mac"));
        Assert.False(Wake.MoonBeforeStatus(Asleep(supported: false), "mac"));
        Assert.False(Wake.MoonBeforeStatus(Sample.Model(wakeSupported: true), "mac"));
    }

    [Fact]
    public void AServiceFromBeforeWakingShowsNothingOfIt()
    {
        var old = Asleep(wake: WakeState.Waking, since: TwoHoursAgo, supported: false);
        Assert.Equal(new UiText(TextKeys.ConnectionOffline), Status(old));
        Assert.False(Wake.Offered(old, "mac"));
        Assert.False(Wake.CanSend(old, "mac"));
        Assert.False(Wake.Waking(old, "mac"));
        Assert.Equal(WakeNote.None, Wake.Note(old, "mac"));
        Assert.Equal(WakeNote.None, Wake.Note(Asleep(canWake: false, supported: false), "mac"));
        Assert.False(Wake.SwitchShown(old));
        Assert.True(Wake.SwitchShown(Asleep()));
    }

    [Fact]
    public void OffersWakeOnlyForOneThatCanBeWoken()
    {
        // Wake, the menu item and the moon.
        Assert.True(Wake.Offered(Asleep(), "mac"));
        Assert.True(Wake.CanSend(Asleep(), "mac"));
        // Being woken: still there, but not to press.
        Assert.True(Wake.Offered(Asleep(wake: WakeState.Waking), "mac"));
        Assert.False(Wake.CanSend(Asleep(wake: WakeState.Waking), "mac"));
        // Didn't wake up: pressed again, it tries again.
        Assert.True(Wake.CanSend(Asleep(wake: WakeState.NotWoken), "mac"));
        // Never said how to wake it.
        Assert.False(Wake.Offered(Asleep(canWake: false), "mac"));
        Assert.False(Wake.CanSend(Asleep(canWake: false), "mac"));
        // This computer, and one not trusted.
        Assert.False(Wake.Offered(Asleep(), "pc"));
        Assert.False(Wake.Offered(Asleep(), "ghost"));
    }

    [Fact]
    public void WakeWaitsForTheAnswerToThisAppsRequest()
    {
        var sent = Asleep() with { WakesSent = ImmutableHashSet.Create("mac") };
        Assert.True(Wake.Offered(sent, "mac"));
        Assert.False(Wake.CanSend(sent, "mac"));
        Assert.True(Wake.CanSend(Asleep() with { WakesSent = ImmutableHashSet.Create("mini") }, "mac"));
    }

    [Fact]
    public void ForgetsRequestsForComputersNoLongerThere()
    {
        var status = Asleep().Status;
        var sent = ImmutableHashSet.Create("mac");
        Assert.Same(sent, Wake.Kept(sent, status));
        var empty = ImmutableHashSet<string>.Empty;
        Assert.Same(empty, Wake.Kept(empty, status));
        Assert.Equal(new[] { "mac" }, Wake.Kept(ImmutableHashSet.Create("mac", "gone"), status));
    }

    [Fact]
    public void TheNoteSaysWhyItDidntWakeOrCantYet()
    {
        Assert.Equal(WakeNote.NotWoken, Wake.Note(Asleep(wake: WakeState.NotWoken), "mac"));
        Assert.Equal(new UiText(TextKeys.WakeNotWoken, "MacBook Pro"), Wake.NoteText(Asleep(wake: WakeState.NotWoken), "mac"));
        Assert.Equal(WakeNote.NotTold, Wake.Note(Asleep(canWake: false), "mac"));
        Assert.Equal(new UiText(TextKeys.WakeNotTold, "MacBook Pro"), Wake.NoteText(Asleep(canWake: false), "mac"));
        // None while it can be woken and is not known to have failed.
        Assert.Equal(WakeNote.None, Wake.Note(Asleep(), "mac"));
        Assert.Equal(WakeNote.None, Wake.Note(Asleep(wake: WakeState.Waking), "mac"));
        Assert.Null(Wake.NoteText(Asleep(), "mac"));
        // None while connected, nor for this computer.
        Assert.Equal(WakeNote.None, Wake.Note(Sample.Model(wakeSupported: true), "mac"));
        Assert.Equal(WakeNote.None, Wake.Note(Asleep(canWake: false), "pc"));
    }

    [Fact]
    public void TheWakeSwitchGoesBackWhenRefused()
    {
        var watch = new WakeOnPushSwitch();
        var off = Asleep();
        Assert.False(watch.Update(off));
        Assert.Equal(new UiText(TextKeys.GeneralWakeOnPushHint), watch.Hint);
        // Turned on, refused: the service's value stays, and the line says so.
        watch.Answered(taken: false);
        Assert.True(watch.Refused);
        Assert.Equal(new UiText(TextKeys.GeneralWakeOnPushFailed), watch.Hint);
        Assert.False(watch.Update(off));
        Assert.True(watch.Refused);
        // The service reports another value: the refusal no longer holds.
        var on = With(off, s => s.WakeOnPush = true);
        Assert.True(watch.Update(on));
        Assert.False(watch.Refused);
        // Nor once the service is out of reach.
        watch.Answered(taken: false);
        Assert.True(watch.Update(on with { Link = AgentLink.Connecting }));
        Assert.False(watch.Refused);
        watch.Answered(taken: true);
        Assert.False(watch.Refused);
    }

    internal static AgentSnapshot With(AgentSnapshot model, Action<Status> change)
    {
        var status = model.Status.Clone();
        change(status);
        return model with { Status = status };
    }

    [Fact]
    public void ReadsAsAgreedInBothLanguages()
    {
        var en = WakeTexts.Load("en-US");
        var zh = WakeTexts.Load("zh-TW");
        var since = Status(Asleep(since: TwoHoursAgo));
        Assert.Equal("Offline · 2 hours ago", WakeTexts.Say(en, since));
        Assert.Equal("離線・2 小時前", WakeTexts.Say(zh, since));
        var now = Status(Asleep(since: (ulong)Sample.Now.ToUnixTimeMilliseconds()));
        Assert.Equal("Offline · just now", WakeTexts.Say(en, now));
        Assert.Equal("離線・剛剛", WakeTexts.Say(zh, now));
        Assert.Equal("Waking…", en[TextKeys.ConnectionWaking]);
        Assert.Equal("正在喚醒…", zh[TextKeys.ConnectionWaking]);
        Assert.Equal("Wake", en[TextKeys.WakeAction]);
        Assert.Equal("喚醒", zh[TextKeys.WakeAction]);
        var notWoken = Wake.NoteText(Asleep(wake: WakeState.NotWoken), "mac")!;
        Assert.Equal(
            "MacBook Pro didn’t wake up. Wake for network access may be off on it, or it may not be on a wired network.",
            WakeTexts.Say(en, notWoken));
        Assert.Equal("「MacBook Pro」沒有醒來。它可能沒有打開網路喚醒，或沒有接在有線網路上。", WakeTexts.Say(zh, notWoken));
        Assert.Equal("How to turn on wake for network access", en[TextKeys.WakeHowTo]);
        Assert.Equal("怎麼打開網路喚醒", zh[TextKeys.WakeHowTo]);
        Assert.Equal("https://inputweave.ctsite.cc/user-guide#wake-for-network-access", en[TextKeys.WakeHowToUrl]);
        Assert.Equal("https://inputweave.ctsite.cc/zh-Hant/user-guide#wake-for-network-access", zh[TextKeys.WakeHowToUrl]);
        var notTold = Wake.NoteText(Asleep(canWake: false), "mac")!;
        Assert.Equal(
            "MacBook Pro hasn’t told this computer how to wake it yet. You can wake it after it connects again.",
            WakeTexts.Say(en, notTold));
        Assert.Equal("「MacBook Pro」還沒告訴這台怎麼喚醒它；下次連上之後就能喚醒。", WakeTexts.Say(zh, notTold));
        Assert.Equal("Waking MacBook Pro…", WakeTexts.Say(en, new UiText(TextKeys.HudWaking, "MacBook Pro")));
        Assert.Equal("正在喚醒「MacBook Pro」…", WakeTexts.Say(zh, new UiText(TextKeys.HudWaking, "MacBook Pro")));
        Assert.Equal("MacBook Pro is awake.", WakeTexts.Say(en, new UiText(TextKeys.HudAwake, "MacBook Pro")));
        Assert.Equal("「MacBook Pro」醒來了。", WakeTexts.Say(zh, new UiText(TextKeys.HudAwake, "MacBook Pro")));
        Assert.Equal("Wake a sleeping computer when you push the cursor toward it", en[TextKeys.GeneralWakeOnPush]);
        Assert.Equal("把游標推向睡眠中的電腦時喚醒它", zh[TextKeys.GeneralWakeOnPush]);
        Assert.Equal(
            "When the cursor pushes against the edge on a sleeping computer’s side, InputWeave sends it a wake-up, at most every 30 seconds. Works only on computers with wake for network access on, usually over a wired network.",
            en[TextKeys.GeneralWakeOnPushHint]);
        Assert.Equal(
            "游標推到睡眠中電腦那一側的螢幕邊緣時，送出網路喚醒，同一台 30 秒最多一次。只對打開網路喚醒的電腦有效，多半要接有線網路。",
            zh[TextKeys.GeneralWakeOnPushHint]);
    }
}

/// <summary>The HUD as another computer is woken and wakes up.</summary>
public class WakeHudTests
{
    private static AgentSnapshot Mac(WakeState wake, bool online = false, bool supported = true) =>
        Sample.Model(macOnline: online, wakeSupported: supported, macCanWake: !online, macWake: wake);

    private static UiText? Update(HudWatch watch, AgentSnapshot model)
    {
        int hints = HudWatch.HintTimes;
        return watch.Update(model, ref hints);
    }

    private static readonly UiText Waking = new(TextKeys.HudWaking, "MacBook Pro");
    private static readonly UiText Awake = new(TextKeys.HudAwake, "MacBook Pro");

    [Fact]
    public void SaysItIsWakingThenThatItIsAwake()
    {
        var watch = new HudWatch();
        Assert.Null(Update(watch, Mac(WakeState.None)));
        Assert.Equal(Waking, Update(watch, Mac(WakeState.Waking)));
        // Still waking: no news.
        Assert.Null(Update(watch, Mac(WakeState.Waking)));
        Assert.Equal(Awake, Update(watch, Mac(WakeState.None, online: true)));
        Assert.Null(Update(watch, Mac(WakeState.None, online: true)));
    }

    [Fact]
    public void SaysNothingWhenItDidntWakeUpAndAgainWhenWokenAgain()
    {
        var watch = new HudWatch();
        Update(watch, Mac(WakeState.None));
        Assert.Equal(Waking, Update(watch, Mac(WakeState.Waking)));
        // The inspector says it didn't wake up; the HUD does not.
        Assert.Null(Update(watch, Mac(WakeState.NotWoken)));
        Assert.Equal(Waking, Update(watch, Mac(WakeState.Waking)));
        // Connecting later without being woken is no waking up.
        Update(watch, Mac(WakeState.NotWoken));
        Assert.Null(Update(watch, Mac(WakeState.None, online: true)));
    }

    [Fact]
    public void TheFirstStateOnlyRecords()
    {
        // Already being woken when the app starts.
        var watch = new HudWatch();
        Assert.Null(Update(watch, Mac(WakeState.Waking)));
        Assert.Equal(Awake, Update(watch, Mac(WakeState.None, online: true)));

        // The service reached again: what it says first is no news, but it counts.
        Update(watch, Mac(WakeState.None));
        Assert.Null(Update(watch, Sample.Model(AgentLink.Connecting)));
        Assert.Null(Update(watch, Mac(WakeState.Waking)));
        Assert.Null(Update(watch, Mac(WakeState.Waking)));
        Assert.Equal(Awake, Update(watch, Mac(WakeState.None, online: true)));
    }

    [Fact]
    public void ForgetsComputersThatLeftTheStatus()
    {
        var watch = new HudWatch();
        Update(watch, Mac(WakeState.None));
        Assert.Equal(Waking, Update(watch, Mac(WakeState.Waking)));
        // Unpaired while being woken, then paired again: it was not woken.
        Assert.Null(Update(watch, Sample.Model(noDevices: true, wakeSupported: true)));
        Assert.Null(Update(watch, Mac(WakeState.None, online: true)));
    }

    [Fact]
    public void AServiceFromBeforeWakingSaysNothing()
    {
        var watch = new HudWatch();
        Update(watch, Mac(WakeState.None, supported: false));
        Assert.Null(Update(watch, Mac(WakeState.Waking, supported: false)));
        Assert.Null(Update(watch, Mac(WakeState.None, online: true, supported: false)));
    }

    private static DeviceStatus Mini(WakeState wake) =>
        new() { Id = "mini", Name = "Mac mini", State = ConnectionState.Offline, CanWake = true, Wake = wake };

    [Fact]
    public void WakingComesBeforeTheTakeBackHint()
    {
        var watch = new HudWatch();
        int hints = 0;
        watch.Update(WakeTests.With(Sample.Model(wakeSupported: true), s => s.Devices.Add(Mini(WakeState.None))), ref hints);
        // The first crossing to the MacBook Pro as the Mac mini starts being woken.
        var both = WakeTests.With(Sample.Model(active: "mac", wakeSupported: true), s => s.Devices.Add(Mini(WakeState.Waking)));
        Assert.Equal(new UiText(TextKeys.HudWaking, "Mac mini"), watch.Update(both, ref hints));
    }

    [Fact]
    public void SeveralAtOnceNameTheFirstInTheStatus()
    {
        var watch = new HudWatch();
        Update(watch, WakeTests.With(Mac(WakeState.None), s => s.Devices.Add(Mini(WakeState.None))));
        var both = WakeTests.With(Mac(WakeState.Waking), s => s.Devices.Add(Mini(WakeState.Waking)));
        Assert.Equal(Waking, Update(watch, both));
        // The other way round in the status: the other one.
        var watch2 = new HudWatch();
        Update(watch2, WakeTests.With(Mac(WakeState.None), s => s.Devices.Insert(0, Mini(WakeState.None))));
        var reversed = WakeTests.With(Mac(WakeState.Waking), s => s.Devices.Insert(0, Mini(WakeState.Waking)));
        Assert.Equal(new UiText(TextKeys.HudWaking, "Mac mini"), Update(watch2, reversed));
    }

    [Fact]
    public void AnotherNoteFirstLeavesNothingBehind()
    {
        var watch = new HudWatch();
        var start = Mac(WakeState.None);
        Update(watch, start);
        // The keyboard and mouse came back in the same state the Mac started being woken.
        var both = WakeTests.With(Mac(WakeState.Waking), s =>
        {
            s.ControlReturns = 1;
            s.LastReturn = ReturnReason.Hotkey;
        });
        Assert.Equal(new UiText(TextKeys.HudBackHere), Update(watch, both));
        // No late "Waking…".
        Assert.Null(Update(watch, both));
        Assert.Equal(Awake, Update(watch, WakeTests.With(Mac(WakeState.None, online: true), s => s.ControlReturns = 1)));
    }
}
