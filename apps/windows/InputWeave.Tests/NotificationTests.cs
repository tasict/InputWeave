using System.Globalization;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Notifications;
using InputWeave.Panel;
using InputWeave.Support;
using Xunit;

namespace InputWeave.Tests;

/// <summary>When the "service stopped" notification goes out and comes back.</summary>
public class ServiceWatchTests
{
    private static readonly DateTimeOffset T = Sample.Now;

    [Fact]
    public void TellsOnlyAStopThatLasts()
    {
        var watch = new ServiceWatch();
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Running, T, quiet: false));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopping, T.AddSeconds(1), quiet: false));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopped, T.AddSeconds(2), quiet: false));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopped, T.AddSeconds(11), quiet: false));
        Assert.Equal(ServiceNews.Raise, watch.Update(ServiceState.Stopped, T.AddSeconds(12), quiet: false));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopped, T.AddSeconds(40), quiet: false));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Starting, T.AddSeconds(41), quiet: false));
        Assert.Equal(ServiceNews.Withdraw, watch.Update(ServiceState.Running, T.AddSeconds(42), quiet: false));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Running, T.AddSeconds(43), quiet: false));
    }

    [Fact]
    public void LetsWindowsRestartACrashedService()
    {
        var watch = new ServiceWatch();
        watch.Update(ServiceState.Running, T, quiet: false);
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopped, T.AddSeconds(1), quiet: false));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopped, T.AddSeconds(5), quiet: false));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Running, T.AddSeconds(7), quiet: false));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Running, T.AddSeconds(30), quiet: false));
    }

    [Fact]
    public void SaysNothingOfAServiceStoppedBeforeTheAppStarted()
    {
        var watch = new ServiceWatch();
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopped, T, quiet: false));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopped, T.AddMinutes(5), quiet: false));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.NotInstalled, T.AddMinutes(6), quiet: false));
    }

    [Fact]
    public void StaysQuietBeforeSetupAndWhileTheAppRestartsTheService()
    {
        var watch = new ServiceWatch();
        watch.Update(ServiceState.Running, T, quiet: true);
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopped, T.AddSeconds(1), quiet: true));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopped, T.AddSeconds(20), quiet: true));
        // Setup finishing later does not make that stop news.
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopped, T.AddSeconds(30), quiet: false));
        watch.Update(ServiceState.Running, T.AddSeconds(31), quiet: false);

        watch.Restarting(T.AddSeconds(40));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopped, T.AddSeconds(42), quiet: false));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopped, T.AddSeconds(60), quiet: false));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Running, T.AddSeconds(62), quiet: false));
        // Restarted: a stop is news again.
        watch.Update(ServiceState.Stopped, T.AddSeconds(70), quiet: false);
        Assert.Equal(ServiceNews.Raise, watch.Update(ServiceState.Stopped, T.AddSeconds(80), quiet: false));
    }

    [Fact]
    public void TellsOfARestartThatNeverStartedAgain()
    {
        var watch = new ServiceWatch();
        watch.Update(ServiceState.Running, T, quiet: false);
        watch.Restarting(T.AddSeconds(1));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopped, T.AddSeconds(3), quiet: false));
        Assert.Equal(ServiceNews.None, watch.Update(ServiceState.Stopped, T.AddSeconds(100), quiet: false));
        Assert.Equal(ServiceNews.Raise, watch.Update(ServiceState.Stopped, T.AddMinutes(3), quiet: false));
    }
}

/// <summary>The file notifications, as the macOS app's TransferNotifier decides them.</summary>
public class TransferNewsTests
{
    private static readonly DateTimeOffset T = Sample.Now;

    private static FileTransfer File(string id, TransferState state, string device = "mac", bool outgoing = false) =>
        new() { TransferId = id, DeviceId = device, Name = $"{id}.txt", Size = 1024, State = state, Outgoing = outgoing, Path = $@"C:\Users\me\Downloads\InputWeave\{id}.txt" };

    private static AgentSnapshot With(params FileTransfer[] files)
    {
        var transfers = new Transfers();
        transfers.Transfers_.AddRange(files);
        return Sample.Model() with { Transfers = transfers };
    }

    [Fact]
    public void AsksAboutOffersWaitingWhenTheAppConnectsButNotAboutOldFiles()
    {
        var news = new TransferNews();
        Assert.True(news.Update(Sample.Model(AgentLink.Connecting), T).IsEmpty);
        var first = news.Update(With(File("a", TransferState.Done), File("b", TransferState.WaitingForAnswer)), T);
        Assert.Equal(["b"], first.Ask.Select(t => t.TransferId));
        Assert.Empty(first.Announce);
        Assert.True(news.Update(With(File("a", TransferState.Done), File("b", TransferState.WaitingForAnswer)), T.AddSeconds(5)).IsEmpty);
    }

    [Fact]
    public void TakesAnOfferBackOnceItIsAnswered()
    {
        var news = new TransferNews();
        news.Update(With(), T);
        Assert.Equal(["x"], news.Update(With(File("x", TransferState.WaitingForAnswer)), T).Ask.Select(t => t.TransferId));
        var answered = news.Update(With(File("x", TransferState.Transferring)), T.AddSeconds(1));
        Assert.Equal(["x"], answered.Withdraw);
        Assert.Empty(answered.Ask);
        // Gone from the list while still asking: taken back too.
        news.Update(With(File("y", TransferState.WaitingForAnswer)), T.AddSeconds(2));
        Assert.Equal(["y"], news.Update(With(), T.AddSeconds(3)).Withdraw);
    }

    [Fact]
    public void AnnouncesTheFilesFromOneComputerTogetherOnceTheyHaveSettled()
    {
        var news = new TransferNews();
        news.Update(With(File("1", TransferState.Transferring), File("2", TransferState.Transferring), File("out", TransferState.Done, outgoing: true)), T);
        // One done, one still coming: wait.
        Assert.Empty(news.Update(With(File("1", TransferState.Done), File("2", TransferState.Transferring)), T.AddSeconds(1)).Announce);
        Assert.Empty(news.Update(With(File("1", TransferState.Done), File("2", TransferState.Transferring)), T.AddSeconds(5)).Announce);
        var both = With(File("1", TransferState.Done), File("2", TransferState.Done));
        Assert.Empty(news.Update(both, T.AddSeconds(6)).Announce);
        Assert.Empty(news.Update(both, T.AddSeconds(7.4)).Announce);
        var announced = news.Update(both, T.AddSeconds(7.5)).Announce;
        var group = Assert.Single(announced);
        Assert.Equal("mac", group.DeviceId);
        Assert.Equal(["1", "2"], group.Files.Select(f => f.TransferId));
        Assert.Empty(news.Update(both, T.AddSeconds(9)).Announce);
    }

    [Fact]
    public void KeepsSendersApartAndLeavesOutFailures()
    {
        var news = new TransferNews();
        news.Update(With(), T);
        var update = With(File("m", TransferState.Done, "mac"), File("l", TransferState.Done, "laptop"), File("f", TransferState.Failed, "mac"));
        news.Update(update, T);
        var announced = news.Update(update, T.AddSeconds(2)).Announce;
        Assert.Equal(2, announced.Count);
        // The failure goes along, to be counted, but is not among what arrived.
        Assert.Equal(["m"], announced.Single(g => g.DeviceId == "mac").Arrived.Select(f => f.TransferId));
        Assert.Equal(["m", "f"], announced.Single(g => g.DeviceId == "mac").Files.Select(f => f.TransferId));
        Assert.Equal(["l"], announced.Single(g => g.DeviceId == "laptop").Files.Select(f => f.TransferId));
    }

    [Fact]
    public void NamesTheFilesReceived()
    {
        var culture = CultureInfo.InvariantCulture;
        var one = File("Report", TransferState.Done);
        one.Size = 2 * 1024 * 1024;
        Assert.Equal(new UiText(TextKeys.NotifyReceivedOne, "Report.txt", "2 MB", "Mac mini"), TransferText.Received([one], "Mac mini", culture));
        Assert.Equal(
            new UiText(TextKeys.NotifyReceivedMany, "3", "3 KB", "Mac mini"),
            TransferText.Received([File("a", TransferState.Done), File("b", TransferState.Done), File("c", TransferState.Done)], "Mac mini", culture));
    }

    [Fact]
    public void ShowsQueuedFilesAndSurvivesStatesItDoesNotKnow()
    {
        var culture = CultureInfo.InvariantCulture;
        var queued = File("q", TransferStates.Queued, outgoing: true);
        Assert.Equal(new UiText(TextKeys.TransferQueued), TransferText.Detail(queued, "Mac mini", culture));
        Assert.False(queued.State.IsFinished());
        Assert.Null(TransferText.Fraction(queued));
        Assert.Equal(["q"], TransferText.PanelRows([queued], new HashSet<string>()).Select(t => t.TransferId));

        var unknown = File("u", (TransferState)99);
        Assert.Null(TransferText.Detail(unknown, "Mac mini", culture));
        Assert.Null(TransferText.Fraction(unknown));
        Assert.False(TransferText.IsProblem(unknown));
    }
}

/// <summary>Which keys the uninstaller removes to take back the notification registration.</summary>
public class NotificationRegistrationTests
{
    private const string Exe = @"C:\Program Files\InputWeave\App\InputWeave.exe";

    [Fact]
    public void NamesTheAppByItsPath()
    {
        Assert.Equal(
            @"Software\Classes\AppUserModelId\c:.program files.inputweave.app.inputweave.exe",
            NotificationRegistration.PathKey(Exe));
    }

    [Fact]
    public void FollowsOnlyBracedGuids()
    {
        Assert.Equal(
            @"Software\Classes\AppUserModelId\{E20C9416-E3EB-4874-AD72-7B7F850AA6EC}",
            NotificationRegistration.RegistrationKey("{E20C9416-E3EB-4874-AD72-7B7F850AA6EC}"));
        Assert.Equal(
            @"Software\Classes\CLSID\{F14512AD-E53C-425D-945E-F25179C2792C}",
            NotificationRegistration.ActivatorKey("{F14512AD-E53C-425D-945E-F25179C2792C}"));
        // Never the root, or another value taken for a key.
        Assert.Null(NotificationRegistration.RegistrationKey(null));
        Assert.Null(NotificationRegistration.RegistrationKey(""));
        Assert.Null(NotificationRegistration.RegistrationKey("E20C9416-E3EB-4874-AD72-7B7F850AA6EC"));
        Assert.Null(NotificationRegistration.RegistrationKey(@"{E20C9416-E3EB-4874-AD72-7B7F850AA6EC}\.."));
        Assert.Null(NotificationRegistration.ActivatorKey(@"..\..\Something"));
        Assert.Null(NotificationRegistration.ActivatorKey("{not-a-guid-at-all-not-a-guid-at-all-x}"));
    }

    [Fact]
    public void RemovesOnlyTheComClassThatStartsThisApp()
    {
        Assert.True(NotificationRegistration.StartsApp(
            "\"c:\\program files\\inputweave\\app\\inputweave.exe\" ----AppNotificationActivated:", Exe));
        Assert.True(NotificationRegistration.StartsApp(Exe, Exe));
        Assert.True(NotificationRegistration.StartsApp(Exe + " ----AppNotificationActivated:", Exe));
        Assert.False(NotificationRegistration.StartsApp(
            "\"C:\\Program Files\\Other\\Other.exe\" ----AppNotificationActivated:", Exe));
        Assert.False(NotificationRegistration.StartsApp("\"" + Exe + ".old\" -x", Exe));
        Assert.False(NotificationRegistration.StartsApp(Exe + "x", Exe));
        Assert.False(NotificationRegistration.StartsApp(null, Exe));
        Assert.False(NotificationRegistration.StartsApp("\"\"", ""));
    }
}
