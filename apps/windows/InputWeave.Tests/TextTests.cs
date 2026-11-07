using System.Globalization;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Canvas;
using InputWeave.Pairing;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using Xunit;

namespace InputWeave.Tests;

public class TransferTextTests
{
    private static readonly CultureInfo En = CultureInfo.GetCultureInfo("en-US");

    private static FileTransfer Transfer(
        TransferState state, string id = "t", bool outgoing = true, string reason = "", ulong done = 0, ulong rate = 0) =>
        new()
        {
            TransferId = id,
            Name = "Report.pdf",
            Size = 100_000_000,
            State = state,
            Outgoing = outgoing,
            Reason = reason,
            DoneBytes = done,
            BytesPerSecond = rate,
        };

    [Fact]
    public void DescribesProgressAndWaiting()
    {
        var going = Transfer(TransferState.Transferring, done: 64_000_000, rate: 12_000_000);
        Assert.Equal("Report.pdf → PC", TransferText.Title(going, "PC"));
        Assert.Equal("Report.pdf ← Mac", TransferText.Title(Transfer(TransferState.Done, outgoing: false), "Mac"));
        Assert.Equal(0.64, TransferText.Fraction(going));
        // 36 MB left at 12 MB/s.
        Assert.Equal(
            new UiText(TextKeys.TransferProgress, "64%", "11.4 MB", new UiText(TextKeys.TransferSecondsLeft, "3")),
            TransferText.Detail(going, "PC", En));
        // Before the speed is known, only how far.
        Assert.Equal(
            new UiText(TextKeys.Verbatim, "64%"),
            TransferText.Detail(Transfer(TransferState.Transferring, done: 64_000_000), "PC", En));

        Assert.Null(TransferText.Fraction(Transfer(TransferState.Preparing)));
        // Cut off midway, the bar stays where the file will resume.
        Assert.Equal(0.5, TransferText.Fraction(Transfer(TransferState.WaitingForConnection, done: 50_000_000)));
        Assert.Null(TransferText.Fraction(Transfer(TransferState.WaitingForConnection)));
        Assert.Equal(
            new UiText(TextKeys.TransferWaitingForConnection),
            TransferText.Detail(Transfer(TransferState.WaitingForConnection), "PC", En));
        Assert.Equal(
            new UiText(TextKeys.TransferOffer, "Mac", "95.3 MB"),
            TransferText.Detail(Transfer(TransferState.WaitingForAnswer, outgoing: false), "Mac", En));
    }

    [Fact]
    public void RoundsTheTimeLeftUpToOneUnit()
    {
        Assert.Equal(new UiText(TextKeys.TransferSecondsLeft, "1"), TransferText.Remaining(0.2, En));
        Assert.Equal(new UiText(TextKeys.TransferSecondsLeft, "59"), TransferText.Remaining(58.5, En));
        Assert.Equal(new UiText(TextKeys.TransferMinutesLeft, "2"), TransferText.Remaining(100, En));
        Assert.Equal(new UiText(TextKeys.TransferHoursLeft, "2"), TransferText.Remaining(6000, En));
    }

    [Fact]
    public void FormatsSizesAsFileExplorerDoes()
    {
        Assert.Equal("512 B", ByteSize.Format(512, En));
        Assert.Equal("1 KB", ByteSize.Format(1024, En));
        Assert.Equal("1.5 KB", ByteSize.Format(1536, En));
        Assert.Equal("11.4 MB", ByteSize.Format(12_000_000, En));
        Assert.Equal("120 MB", ByteSize.Format(120 * 1024 * 1024 + 900_000, En));
        Assert.Equal("4.2 GB", ByteSize.Format(4_510_000_000, En));
        Assert.Equal("1,5 KB", ByteSize.Format(1536, CultureInfo.GetCultureInfo("de-DE")));
    }

    [Fact]
    public void ExplainsDeclinesFromEitherSide()
    {
        Assert.Equal(
            new UiText(TextKeys.TransferPeerDeclined, "PC"),
            TransferText.Detail(Transfer(TransferState.Declined, reason: "rejected"), "PC", En));
        Assert.Equal(
            new UiText(TextKeys.TransferDeclined),
            TransferText.Detail(Transfer(TransferState.Declined, outgoing: false, reason: "rejected"), "Mac", En));
        Assert.False(TransferText.IsProblem(Transfer(TransferState.Declined, outgoing: false, reason: "rejected")));
        Assert.True(TransferText.IsProblem(Transfer(TransferState.Declined, reason: "no_space")));
        Assert.Equal(
            new UiText(TextKeys.TransferCorrupt),
            TransferText.Detail(Transfer(TransferState.Failed, reason: "corrupt"), "PC", En));
        Assert.Equal(
            new UiText(TextKeys.TransferPeerDoesNotReceive, "PC"),
            TransferText.Detail(Transfer(TransferState.Declined, reason: "unavailable"), "PC", En));
    }

    [Fact]
    public void PanelListsWhatIsGoingAndProblemsUntilDismissed()
    {
        FileTransfer[] all =
        [
            Transfer(TransferState.Transferring, id: "a"), Transfer(TransferState.Done, id: "b"),
            Transfer(TransferState.Failed, id: "c", reason: "corrupt"), Transfer(TransferState.Cancelled, id: "d"),
        ];
        Assert.Equal(["a", "c"], TransferText.PanelRows(all, new HashSet<string>()).Select(t => t.TransferId));
        Assert.Equal(["a"], TransferText.PanelRows(all, new HashSet<string> { "c" }).Select(t => t.TransferId));
    }

    [Fact]
    public void RowsUnderWayOfferOnlyCancel()
    {
        foreach (var state in new[]
        {
            TransferState.Preparing, TransferStates.Queued, TransferState.WaitingForConnection,
            TransferState.WaitingForAnswer, TransferState.Transferring, TransferState.Verifying,
        })
        {
            var going = Transfer(state);
            going.Path = @"C:\Users\me\Report.pdf";
            Assert.Equal(TransferActions.Cancel, TransferText.Actions(going, dismissible: false, removable: true));
            Assert.Equal(TransferActions.Cancel, TransferText.Actions(going, dismissible: true, removable: false));
        }
    }

    [Fact]
    public void FinishedRowsCanBeRemovedFromTheHistory()
    {
        var done = Transfer(TransferState.Done);
        done.Path = @"C:\Users\me\Downloads\InputWeave\Report.pdf";
        // Next to Show in File Explorer, as on the Mac.
        Assert.Equal(TransferActions.Show | TransferActions.Remove, TransferText.Actions(done, dismissible: false, removable: true));
        // A file that went without a path to show.
        Assert.Equal(TransferActions.Remove, TransferText.Actions(Transfer(TransferState.Done), dismissible: false, removable: true));
        foreach (var state in new[] { TransferState.Failed, TransferState.Declined, TransferState.Cancelled })
        {
            Assert.Equal(TransferActions.Remove, TransferText.Actions(Transfer(state, reason: "corrupt"), dismissible: false, removable: true));
        }
    }

    [Fact]
    public void ThePanelDismissesProblemsButRemovesNothing()
    {
        var failed = Transfer(TransferState.Failed, reason: "corrupt");
        Assert.Equal(TransferActions.Dismiss, TransferText.Actions(failed, dismissible: true, removable: false));
        var done = Transfer(TransferState.Done);
        done.Path = @"C:\Users\me\Downloads\InputWeave\Report.pdf";
        Assert.Equal(TransferActions.Show, TransferText.Actions(done, dismissible: true, removable: false));
        // Neither list: nothing but showing the file.
        Assert.Equal(TransferActions.None, TransferText.Actions(failed, dismissible: false, removable: false));
        Assert.Equal(TransferActions.Show, TransferText.Actions(done, dismissible: false, removable: false));
    }

    [Fact]
    public void HistoryClearsOnlyWithSomethingFinished()
    {
        Assert.False(TransferText.CanClearHistory([]));
        Assert.False(TransferText.CanClearHistory([Transfer(TransferState.Transferring), Transfer(TransferStates.Queued)]));
        Assert.True(TransferText.CanClearHistory([Transfer(TransferState.Transferring), Transfer(TransferState.Cancelled)]));
        Assert.True(TransferText.CanClearHistory([Transfer(TransferState.Declined, reason: "rejected")]));
    }
}

/// <summary>Which transfer problems stay off the panel, also after the app restarts.</summary>
public class DismissedTransfersTests
{
    private static FileTransfer File(string id, TransferState state, string reason = "corrupt") =>
        new() { TransferId = id, DeviceId = "mac", Name = $"{id}.txt", State = state, Reason = reason, Outgoing = true };

    private static AgentSnapshot With(params FileTransfer[] files)
    {
        var transfers = new Transfers();
        transfers.Transfers_.AddRange(files);
        return Sample.Model() with { Transfers = transfers };
    }

    private static IEnumerable<string> Panel(DismissedTransfers dismissed, AgentSnapshot snapshot) =>
        TransferText.PanelRows(snapshot.Transfers.Transfers_, dismissed.Ids).Select(t => t.TransferId);

    [Fact]
    public void ProblemsFromBeforeTheAppConnectedAreNotNews()
    {
        var dismissed = new DismissedTransfers([]);
        // Not connected yet: the list is not the service's.
        Assert.False(dismissed.Update(Sample.Model(AgentLink.Connecting)));
        var first = With(File("old", TransferState.Failed), File("going", TransferState.Transferring), File("sent", TransferState.Done));
        Assert.True(dismissed.Update(first));
        Assert.Equal(["old"], dismissed.Ids.Order());
        Assert.Equal(["going"], Panel(dismissed, first));

        // A problem after that shows until dismissed.
        var later = With(File("new", TransferState.Declined, "no_space"), File("old", TransferState.Failed), File("going", TransferState.Failed));
        Assert.False(dismissed.Update(later));
        Assert.Equal(["new", "going"], Panel(dismissed, later));
        Assert.True(dismissed.Dismiss("new"));
        Assert.False(dismissed.Dismiss("new"));
        Assert.Equal(["going"], Panel(dismissed, later));
    }

    [Fact]
    public void OnlyTheFirstConnectionCounts()
    {
        var dismissed = new DismissedTransfers([]);
        dismissed.Update(With());
        // The service restarted with a failure the app had not seen: news.
        Assert.False(dismissed.Update(Sample.Model(AgentLink.Connecting) with { Transfers = With(File("x", TransferState.Failed)).Transfers }));
        var again = With(File("x", TransferState.Failed));
        Assert.False(dismissed.Update(again));
        Assert.Equal(["x"], Panel(dismissed, again));
    }

    [Fact]
    public void KeepsOnlyIdsStillListed()
    {
        var dismissed = new DismissedTransfers(["kept", "removed", ""]);
        Assert.Equal(["kept", "removed"], dismissed.Ids.Order());
        // While the service is out of reach nothing is known to be gone.
        Assert.False(dismissed.Update(Sample.Model(AgentLink.Connecting)));
        Assert.Equal(2, dismissed.Ids.Count);

        var version = dismissed.Version;
        Assert.True(dismissed.Update(With(File("kept", TransferState.Failed), File("fine", TransferState.Done))));
        Assert.Equal(["kept"], dismissed.Ids.Order());
        Assert.NotEqual(version, dismissed.Version);
        // Taken off the history, or cleared: forgotten.
        Assert.True(dismissed.Update(With(File("fine", TransferState.Done))));
        Assert.Empty(dismissed.Ids);
        Assert.False(dismissed.Update(With(File("fine", TransferState.Done))));
    }

    [Fact]
    public void ADismissalSurvivesARestart()
    {
        var history = With(File("a", TransferState.Failed), File("b", TransferState.Done));
        var before = new DismissedTransfers([]);
        before.Update(With());
        Assert.True(before.Dismiss("a"));

        // What the app saved, read back by the next one.
        var after = new DismissedTransfers(before.Ids.ToArray());
        Assert.False(after.Update(history));
        Assert.Empty(Panel(after, history));
        Assert.Equal(["a"], after.Ids.Order());
    }
}

public class ClipboardTextTests
{
    private static readonly CultureInfo En = CultureInfo.GetCultureInfo("en-US");

    private static AgentSnapshot Model(ClipboardActivity? activity, bool sync = true)
    {
        var model = Sample.Model();
        var status = model.Status.Clone();
        status.ClipboardSync = sync;
        status.LastClipboard = activity;
        return model with { Status = status };
    }

    private static ClipboardActivity Activity(double minutesAgo, string from, string to, string skipped = "", params string[] kinds)
    {
        var activity = new ClipboardActivity
        {
            AtUnixMs = (ulong)Sample.Now.AddMinutes(-minutesAgo).ToUnixTimeMilliseconds(),
            FromDeviceId = from,
            ToDeviceId = to,
            Skipped = skipped,
        };
        activity.Kinds.AddRange(kinds);
        return activity;
    }

    [Fact]
    public void SaysWhereTheClipboardWentForTenMinutes()
    {
        Assert.Equal(
            new UiText(
                TextKeys.ClipboardSyncedFrom, "MacBook Pro", new UiText(TextKeys.RelativeMinutesAgo, "2"),
                new UiText(TextKeys.ClipboardKindText)),
            ClipboardText.Line(Model(Activity(2.5, "mac", "pc", "", "text")), Sample.Now, En));
        Assert.Equal(
            new UiText(
                TextKeys.ClipboardSentTo, "MacBook Pro", new UiText(TextKeys.RelativeJustNow),
                new UiText(TextKeys.ClipboardKindImage)),
            ClipboardText.Line(Model(Activity(0.2, "pc", "mac", "", "rich_text", "image")), Sample.Now, En));
        Assert.Null(ClipboardText.Line(Model(Activity(10, "mac", "pc", "", "text")), Sample.Now, En));
        Assert.Null(ClipboardText.Line(Model(Activity(1, "mac", "pc", "", "text"), sync: false), Sample.Now, En));
        Assert.Null(ClipboardText.Line(Model(null), Sample.Now, En));
    }

    [Fact]
    public void SaysWhyTheClipboardWasHeldBack()
    {
        Assert.Equal(
            new UiText(TextKeys.ClipboardSensitive),
            ClipboardText.Line(Model(Activity(1, "pc", "mac", "sensitive")), Sample.Now, En));
        Assert.Equal(
            new UiText(TextKeys.ClipboardTooLarge),
            ClipboardText.Line(Model(Activity(1, "pc", "mac", "too_large")), Sample.Now, En));
        Assert.Null(ClipboardText.Line(Model(Activity(1, "pc", "mac", "something_new")), Sample.Now, En));
    }
}

public class DeviceTextTests
{
    private static readonly CultureInfo En = CultureInfo.GetCultureInfo("en-US");

    [Fact]
    public void ShowsTheRoundTripOnceMeasured()
    {
        Assert.Equal(new UiText(TextKeys.ConnectionConnected), DeviceText.Connected(0));
        Assert.Equal(new UiText(TextKeys.ConnectionConnectedRtt, "12"), DeviceText.Connected(12));
        Assert.Equal(new UiText(TextKeys.ConnectionThisComputer), DeviceText.Status(Sample.Model(), "pc", Sample.Now, En));
        Assert.Equal(new UiText(TextKeys.ConnectionConnected), DeviceText.Status(Sample.Model(), "mac", Sample.Now, En));
        Assert.Equal(new UiText(TextKeys.ConnectionOffline), DeviceText.Status(Sample.Model(macOnline: false), "mac", Sample.Now, En));
        Assert.Equal(new UiText(TextKeys.ConnectionOffline), DeviceText.Status(Sample.Model(), "ghost", Sample.Now, En));
    }

    [Fact]
    public void SaysWhenAScreenIsLocked()
    {
        Assert.Equal(new UiText(TextKeys.ConnectionScreenLocked), DeviceText.Connected(0, screenLocked: true));
        Assert.Equal(new UiText(TextKeys.ConnectionScreenLockedRtt, "12"), DeviceText.Connected(12, screenLocked: true));
        Assert.Equal(new UiText(TextKeys.ConnectionConnectedRtt, "12"), DeviceText.Connected(12, screenLocked: false));
        var locked = Sample.Model(macScreenLocked: true, macLockScreenInput: true);
        Assert.Equal(new UiText(TextKeys.ConnectionScreenLocked), DeviceText.Status(locked, "mac", Sample.Now, En));
        Assert.True(DeviceText.ScreenLocked(locked, "mac"));
        Assert.False(DeviceText.ScreenLocked(Sample.Model(), "mac"));
        // Offline is offline, whatever it said last.
        var gone = Sample.Model(macOnline: false, macScreenLocked: true);
        Assert.False(DeviceText.ScreenLocked(gone, "mac"));
        Assert.Equal(new UiText(TextKeys.ConnectionOffline), DeviceText.Status(gone, "mac", Sample.Now, En));
        Assert.False(DeviceText.ScreenLocked(locked, "pc"));
    }

    [Fact]
    public void SaysWhenALockedScreenCannotTakeInput()
    {
        Assert.Equal(new UiText(TextKeys.ConnectionScreenLockedNoInput),
            DeviceText.Connected(0, screenLocked: true, lockScreenInput: false));
        Assert.Equal(new UiText(TextKeys.ConnectionScreenLockedNoInputRtt, "12"),
            DeviceText.Connected(12, screenLocked: true, lockScreenInput: false));
        Assert.Equal(new UiText(TextKeys.ConnectionConnectedRtt, "12"),
            DeviceText.Connected(12, screenLocked: false, lockScreenInput: false));
        var outOfReach = Sample.Model(macScreenLocked: true);
        Assert.Equal(new UiText(TextKeys.ConnectionScreenLockedNoInput), DeviceText.Status(outOfReach, "mac", Sample.Now, En));
        // The cursor cannot be sent there; a locked screen that takes input is fine.
        Assert.True(outOfReach.OutOfReach("mac"));
        Assert.False(outOfReach.CanJump("mac"));
        Assert.True(Sample.Model(macScreenLocked: true, macLockScreenInput: true).CanJump("mac"));
        Assert.True(Sample.Model().CanJump("mac"));
        Assert.False(Sample.Model().OutOfReach("mac"));
    }

    [Fact]
    public void SaysWhenNoInputReachesIt()
    {
        Assert.Equal(new UiText(TextKeys.ConnectionNoInput), DeviceText.Connected(0, inputUnavailable: true));
        Assert.Equal(new UiText(TextKeys.ConnectionNoInputRtt, "12"), DeviceText.Connected(12, inputUnavailable: true));
        // Locked too, it says both.
        Assert.Equal(new UiText(TextKeys.ConnectionScreenLockedNoInputRtt, "12"),
            DeviceText.Connected(12, screenLocked: true, lockScreenInput: true, inputUnavailable: true));
        var unavailable = Sample.Model(macInputUnavailable: true);
        Assert.Equal(new UiText(TextKeys.ConnectionNoInput), DeviceText.Status(unavailable, "mac", Sample.Now, En));
        Assert.True(unavailable.OutOfReach("mac"));
        Assert.False(unavailable.CanJump("mac"));
    }

    [Fact]
    public void SaysWhenItsUserPausedSharing()
    {
        Assert.Equal(new UiText(TextKeys.ConnectionPeerPaused), DeviceText.Connected(0, sharingPaused: true));
        Assert.Equal(new UiText(TextKeys.ConnectionPeerPausedRtt, "12"), DeviceText.Connected(12, sharingPaused: true));
        // Before the other reasons the cursor stays off it.
        Assert.Equal(new UiText(TextKeys.ConnectionPeerPausedRtt, "12"),
            DeviceText.Connected(12, screenLocked: true, inputUnavailable: true, sharingPaused: true));
        var paused = Sample.Model(macSharingPaused: true);
        Assert.Equal(new UiText(TextKeys.ConnectionPeerPaused), DeviceText.Status(paused, "mac", Sample.Now, En));
        Assert.True(paused.OutOfReach("mac"));
        Assert.False(paused.CanJump("mac"));
    }

    [Fact]
    public void TheGlyphFollowsTheSamePriority()
    {
        Assert.Null(DeviceText.StatusGlyph(connected: true, screenLocked: false, sharingPaused: false));
        Assert.Equal(DeviceText.LockedGlyph, DeviceText.StatusGlyph(connected: true, screenLocked: true, sharingPaused: false));
        Assert.Equal(DeviceText.PausedGlyph, DeviceText.StatusGlyph(connected: true, screenLocked: false, sharingPaused: true));
        Assert.Equal(DeviceText.PausedGlyph, DeviceText.StatusGlyph(connected: true, screenLocked: true, sharingPaused: true));
        // Offline: the dashed ring, whatever it last said.
        Assert.Null(DeviceText.StatusGlyph(connected: false, screenLocked: true, sharingPaused: true));
    }

    [Fact]
    public void NamesSystemsAndKeysWithoutLogos()
    {
        Assert.Equal(new UiText(TextKeys.Verbatim, "macOS"), DeviceText.Os("macos"));
        Assert.Equal(new UiText(TextKeys.OsUnknown), DeviceText.Os(""));
        Assert.Equal(TextKeys.SwapCommandAndControl, DeviceText.SwapKey("macos"));
        Assert.Equal(TextKeys.SwapWindowsAndControl, DeviceText.SwapKey("windows"));
        Assert.Equal(TextKeys.SwapWindowsAndControl, DeviceText.SwapKey("linux"));
        Assert.Equal(new UiText(TextKeys.Verbatim, "Linux"), DeviceText.Os("linux"));
        Assert.Equal("2560 × 1440", DeviceText.Resolution(new Screen { Width = 1280, Height = 720, Scale = 2 }));
        Assert.Equal("1920 × 1080", DeviceText.Resolution(new Screen { Width = 1920, Height = 1080 }));
    }

    [Fact]
    public void SaysWhichComputersMeet()
    {
        Assert.Equal(
            new UiText(TextKeys.CanvasDeviceNextTo, "PC", "2", "MacBook Pro"),
            DeviceText.CanvasLabel("PC", 2, ["MacBook Pro"]));
        Assert.Equal(new UiText(TextKeys.CanvasDeviceAlone, "PC", "1"), DeviceText.CanvasLabel("PC", 1, []));
    }

    [Fact]
    public void TellsTheServicesStateApartFromTheConnection()
    {
        Assert.Equal(new UiText(TextKeys.ServiceStopped), DeviceText.ServiceStatus(ServiceState.Stopped, AgentLink.Connecting));
        Assert.Equal(new UiText(TextKeys.ServiceNotInstalled), DeviceText.ServiceStatus(ServiceState.NotInstalled, AgentLink.Connecting));
        Assert.Equal(new UiText(TextKeys.ServiceRunning), DeviceText.ServiceStatus(ServiceState.Running, AgentLink.Connected));
        Assert.Equal(new UiText(TextKeys.ServiceStarting), DeviceText.ServiceStatus(ServiceState.Running, AgentLink.Connecting));
        Assert.Equal(new UiText(TextKeys.ServiceUpdating), DeviceText.ServiceStatus(ServiceState.Unknown, AgentLink.Incompatible));
    }

    [Fact]
    public void AnnouncesASyncOnlyWhenOthersGotIt()
    {
        Assert.Equal(new UiText(TextKeys.CanvasSynced, "1"), DeviceText.Synced(3, 4, Sample.Model()));
        // The first arrangement the app sees, no change, or nobody to sync with.
        Assert.Null(DeviceText.Synced(0, 4, Sample.Model()));
        Assert.Null(DeviceText.Synced(4, 4, Sample.Model()));
        Assert.Null(DeviceText.Synced(3, 4, Sample.Model(macOnline: false)));
    }
}

public class PairingTextTests
{
    [Fact]
    public void ExplainsFailuresByTheOtherComputersName()
    {
        Assert.Equal(
            new UiText(TextKeys.PairingRejected, "Mac mini"),
            PairingText.Failure(new PairingState { Phase = PairingPhase.Failed, Error = "rejected", PeerName = "Mac mini" }));
        Assert.Equal(
            new UiText(TextKeys.PairingUnreachable, new UiText(TextKeys.PairingTheOtherComputer)),
            PairingText.Failure(new PairingState { Phase = PairingPhase.Failed, Error = "unreachable" }));
        Assert.Equal(new UiText(TextKeys.PairingExpired), PairingText.Failure(new PairingState { Error = "expired" }));
        Assert.Equal(new UiText(TextKeys.PairingDidNotWork), PairingText.Failure(new PairingState { Error = "protocol" }));
        Assert.Equal(new UiText(TextKeys.PairingStartUnreachable), PairingText.StartFailure("unknown_device"));
        Assert.Equal(new UiText(TextKeys.PairingStartFailed), PairingText.StartFailure("whatever"));
    }

    [Fact]
    public void KeepsSixDigitsAndGroupsThem()
    {
        Assert.Equal("382914", PairingCode.Clean("38 29-14 7"));
        // Only ASCII digits, as the service takes.
        Assert.Equal("2", PairingCode.Clean("１2a"));
        Assert.Equal("382 914", PairingCode.Grouped("382914"));
        Assert.Equal("3829", PairingCode.Grouped("3829"));
        Assert.Equal("3 8 2 9 1 4", PairingCode.Spoken("382914"));
    }

    [Fact]
    public void CountsDownToTheCodesExpiry()
    {
        var now = DateTimeOffset.FromUnixTimeSeconds(1000);
        Assert.Equal("1:58", PairingCode.Countdown(1_000_000 + 118_000, now));
        Assert.Equal("1:58", PairingCode.Countdown(1_000_000 + 117_200, now));
        Assert.Equal("0:00", PairingCode.Countdown(999_000, now));
    }

    [Fact]
    public void ShowsTheCodeOnlyWhileTheServiceSaysSo()
    {
        Assert.True(PairingCode.ShowsCode(new PairingState { Phase = PairingPhase.ShowCode, Code = "382914" }));
        Assert.False(PairingCode.ShowsCode(new PairingState { Phase = PairingPhase.ShowCode }));
        Assert.False(PairingCode.ShowsCode(new PairingState { Phase = PairingPhase.Paired, Code = "382914" }));
        Assert.False(PairingCode.ShowsCode(new PairingState { Phase = PairingPhase.EnterCode }));
    }
}

public class LoginLaunchTests
{
    [Fact]
    public void FollowsTheRunKeyAndTaskManager()
    {
        Assert.Equal("\"C:\\Program Files\\InputWeave\\InputWeave.exe\"", LoginLaunch.Command(@"C:\Program Files\InputWeave\InputWeave.exe"));
        Assert.False(LoginLaunch.IsOn(null, null));
        Assert.False(LoginLaunch.IsOn("", LoginLaunch.Approved()));
        Assert.True(LoginLaunch.IsOn("\"C:\\InputWeave.exe\"", null));
        Assert.True(LoginLaunch.IsOn("\"C:\\InputWeave.exe\"", LoginLaunch.Approved()));
        Assert.True(LoginLaunch.IsOn("\"C:\\InputWeave.exe\"", [6, 0, 0, 0]));
        // Turned off in Task Manager.
        Assert.False(LoginLaunch.IsOn("\"C:\\InputWeave.exe\"", [3, 0, 0, 0, 1, 2, 3, 4]));
    }
}

public class DesktopPictureTests
{
    /// <summary>A JPEG's start: SOI, an APP0 segment, then a baseline frame header.</summary>
    private static byte[] Jpeg(int width, int height, int padding = 0)
    {
        var bytes = new List<byte> { 0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10 };
        bytes.AddRange("JFIF\0"u8.ToArray());
        bytes.AddRange(new byte[9]);
        bytes.AddRange([0xFF, 0xC0, 0x00, 0x11, 0x08, (byte)(height >> 8), (byte)height, (byte)(width >> 8), (byte)width, 0x03]);
        bytes.AddRange(new byte[9]);
        bytes.AddRange(new byte[padding]);
        return bytes.ToArray();
    }

    [Fact]
    public void ReadsTheSizeFromTheFrameHeader()
    {
        Assert.Equal((320, 180), DesktopPicture.Size(Jpeg(320, 180)));
        Assert.True(DesktopPicture.Acceptable(Jpeg(320, 180)));
        Assert.True(DesktopPicture.Acceptable(Jpeg(90, 320)));
    }

    [Fact]
    public void RefusesWhatIsNotAPicture()
    {
        Assert.False(DesktopPicture.Acceptable([]));
        Assert.False(DesktopPicture.Acceptable(Enumerable.Range(0, 200).Select(i => (byte)(i * 37)).ToArray()));
        // A JPEG's first bytes, then noise.
        Assert.False(DesktopPicture.Acceptable([0xFF, 0xD8, 0xFF, 0xE0, .. Enumerable.Range(0, 200).Select(i => (byte)(i * 37))]));
        // Cut off before the frame header.
        Assert.False(DesktopPicture.Acceptable(Jpeg(320, 180)[..20]));
        // A PNG.
        Assert.False(DesktopPicture.Acceptable([0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]));
        // Too many pixels, none, or too many bytes.
        Assert.False(DesktopPicture.Acceptable(Jpeg(5000, 100)));
        Assert.False(DesktopPicture.Acceptable(Jpeg(0, 100)));
        Assert.False(DesktopPicture.Acceptable(Jpeg(320, 180, padding: DesktopPicture.MaxBytes)));
    }

    [Fact]
    public void DecodesEachScreensPictureOnceUntilItChanges()
    {
        var cache = new PictureCache<string>(bytes => $"picture of {bytes.Length} bytes");
        var first = Jpeg(320, 180);
        var second = Jpeg(320, 200);

        var picture = cache.Picture("mac", "builtin", first);
        Assert.NotNull(picture);
        Assert.Same(picture, cache.Picture("mac", "builtin", first));
        Assert.Same(picture, cache.Picture("mac", "builtin", first.ToArray()));
        Assert.Equal(1, cache.Decodes);

        // Another computer's screen of the same id is its own picture.
        Assert.NotSame(picture, cache.Picture("pc", "builtin", first));
        Assert.Same(picture, cache.Picture("mac", "builtin", first));

        Assert.NotSame(picture, cache.Picture("mac", "builtin", second));
        Assert.Null(cache.Picture("mac", "builtin", []));
        Assert.Equal(3, cache.Decodes);

        // Bytes that are not a picture are not decoded, and not again.
        byte[] garbage = [0xFF, 0xD8, 0xFF, 1, 2, 3];
        Assert.Null(cache.Picture("pc", "main", garbage));
        Assert.Null(cache.Picture("pc", "main", garbage));
        Assert.Equal(3, cache.Decodes);

        // A picture the decoder could not open after all shows no picture.
        var failing = cache.Picture("pc", "side", second)!;
        cache.Fail(failing);
        Assert.Null(cache.Picture("pc", "side", second));
        Assert.Equal(4, cache.Decodes);
    }
}
