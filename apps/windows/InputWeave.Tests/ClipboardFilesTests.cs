using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Drag;
using InputWeave.Notifications;
using InputWeave.Panel;
using InputWeave.Support;
using Xunit;

namespace InputWeave.Tests;

/// <summary>Files copied on another computer, pasted here from the clipboard (docs/architecture.md §9).</summary>
public class ClipboardFilesTests
{
    private static async Task Eventually(Func<bool> condition)
    {
        var deadline = DateTime.UtcNow.AddSeconds(10);
        while (!condition())
        {
            Assert.True(DateTime.UtcNow < deadline, "timed out");
            await Task.Delay(20);
        }
    }

    [Fact]
    public void GivesEachPasteAFolderOfItsOwn()
    {
        var root = PasteFolders.Root(@"C:\Users\me\AppData\Local");
        Assert.Equal(@"C:\Users\me\AppData\Local\InputWeave\clipboard", root);
        var first = PasteFolders.For(root, 12);
        var again = PasteFolders.For(root, 12);
        Assert.NotEqual(first, again);
        Assert.Equal(root, Path.GetDirectoryName(first));
        Assert.Matches(@"^12-[0-9a-f]{8}$", Path.GetFileName(first));
    }

    /// <summary>A launch removes only what an earlier app left: a folder made since is a paste's of this run.</summary>
    [Fact]
    public void RemovesOnlyFoldersFromBeforeTheAppStarted()
    {
        var started = new DateTime(2026, 10, 28, 9, 0, 0, DateTimeKind.Utc);
        var folders = new[]
        {
            (@"C:\s\1-old", started.AddMinutes(-30)),
            (@"C:\s\2-new", started.AddSeconds(2)),
        };
        Assert.Equal([@"C:\s\1-old"], LeftoverFolders.FromBefore(folders, started));
    }

    /// <summary>Placing one promise that throws, or never finishes, does not hold up the next.</summary>
    [Fact]
    public async Task PlacesThePromisesInOrderWhateverOneDoes()
    {
        var placements = new Placements(TimeSpan.FromMilliseconds(200));
        var said = new List<(ulong, bool)>();
        Func<bool, Task> Say(ulong id) => ok =>
        {
            lock (said)
            {
                said.Add((id, ok));
            }
            return Task.CompletedTask;
        };
        Task<string?> Allowed() => Task.FromResult<string?>(null);
        var givenUp = new TaskCompletionSource<bool>();

        var throwing = placements.RunAsync(1, Allowed, _ => throw new InvalidOperationException("no clipboard"), Say(1));
        var stuck = placements.RunAsync(2, Allowed, giveUp =>
        {
            giveUp.Register(() => givenUp.TrySetResult(true));
            return new TaskCompletionSource<bool>().Task;
        }, Say(2));
        var refused = placements.RunAsync(3, () => Task.FromResult<string?>("too_late"), _ => throw new InvalidOperationException("never placed"), Say(3));
        var placed = placements.RunAsync(4, Allowed, _ => Task.FromResult(true), Say(4));
        await Task.WhenAll(throwing, stuck, refused, placed).WaitAsync(TimeSpan.FromSeconds(5));

        Assert.Equal([(1ul, false), (2ul, false), (4ul, true)], said);
        Assert.True(await givenUp.Task.WaitAsync(TimeSpan.FromSeconds(1)));
    }

    [Fact]
    public void ReadsTheFilesToSimulate()
    {
        var launch = Launch.Parse([Launch.SimulateClipboardFilesArgument, @"D:\a b\Photos", @"D:\Report.txt"]);
        Assert.Equal([@"D:\a b\Photos", @"D:\Report.txt"], launch.SimulateClipboardFiles);
        Assert.True(launch.Testing);
        Assert.Null(launch.SimulateClipboardArriving);
        Assert.Null(launch.SimulateClipboardRecopy);
        Assert.Null(Launch.Parse([]).SimulateClipboardFiles);

        // Slowly, and copied again while a paste waits.
        var slow = Launch.Parse([
            Launch.SimulateClipboardFilesArgument, @"D:\Report.txt",
            Launch.SimulateClipboardArrivingArgument, "8",
            Launch.SimulateClipboardRecopyArgument, "1.5",
        ]);
        Assert.Equal([@"D:\Report.txt"], slow.SimulateClipboardFiles);
        Assert.Equal(8, slow.SimulateClipboardArriving);
        Assert.Equal(1.5, slow.SimulateClipboardRecopy);
        Assert.Null(Launch.Parse([Launch.SimulateClipboardArrivingArgument, "-1"]).SimulateClipboardArriving);
    }

    /// <summary>Pasted here, the files went where they were pasted, through a folder that is gone by then.</summary>
    [Fact]
    public void ShowsAndAnnouncesNothingForFilesPastedHere()
    {
        var pasted = new FileTransfer
        {
            TransferId = "p1", DeviceId = "mac", Name = "a.txt", State = TransferState.Done,
            Path = @"C:\Users\me\AppData\Local\InputWeave\clipboard\1-0a1b2c3d\a.txt", Clipboard = true,
        };
        Assert.Equal(TransferActions.Remove, TransferText.Actions(pasted, dismissible: false, removable: true));
        // Copied here and pasted elsewhere: the file is where it was.
        var sent = pasted.Clone();
        sent.Outgoing = true;
        Assert.Equal(TransferActions.Show | TransferActions.Remove, TransferText.Actions(sent, dismissible: false, removable: true));

        var news = new TransferNews();
        var none = new Transfers();
        news.Update(Sample.Model() with { Transfers = none }, Sample.Now);
        var arrived = new Transfers();
        arrived.Transfers_.Add(pasted);
        var model = Sample.Model() with { Transfers = arrived };
        news.Update(model, Sample.Now.AddSeconds(1));
        Assert.Empty(news.Update(model, Sample.Now.AddSeconds(10)).Announce);
    }

    [Fact]
    public async Task SaysItTakesFilesAndHearsOfThem()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model(takesClipboardFiles: true);
        var promised = new List<ClipboardFilesPromised>();
        var arrived = new List<ClipboardFilesArrived>();
        model.ClipboardFilesPromised += p => { lock (promised) { promised.Add(p); } };
        model.ClipboardFilesArrived += a => { lock (arrived) { arrived.Add(a); } };
        model.Start();
        await Eventually(() => model.Snapshot.Link == AgentLink.Connected);
        Assert.True(agent.Received[0].Hello.TakesClipboardFiles);

        var promise = new ClipboardFilesPromised { PromiseId = 4, FromDeviceId = "mac" };
        promise.Items.Add(new DraggedItem { Name = "Photos", Folder = true });
        await agent.SendAsync(new Event { ClipboardFilesPromised = promise });
        await Eventually(() => { lock (promised) { return promised.Count == 1; } });
        Assert.Equal("Photos", promised[0].Items.Single().Name);

        var done = new ClipboardFilesArrived { PromiseId = 4, Ok = true };
        done.Paths.Add(@"C:\Users\me\AppData\Local\InputWeave\clipboard\4-0a1b2c3d\Photos");
        await agent.SendAsync(new Event { ClipboardFilesArrived = done });
        await Eventually(() => { lock (arrived) { return arrived.Count == 1; } });
        Assert.True(arrived[0].Ok);
    }

    [Fact]
    public async Task PlacesPastesAndLetsGoOfFilesOnTheClipboard()
    {
        var agent = new FakeAgent();
        agent.PromisedFiles.Add(4);
        await using var model = agent.Model(takesClipboardFiles: true);
        model.Start();
        await Eventually(() => model.Snapshot.Link == AgentLink.Connected);

        // Asked before the clipboard is touched.
        await model.ClipboardFilesPlaceAsync(4);
        Assert.Equal(4ul, agent.Received[^1].ClipboardFilesPlace.PromiseId);
        agent.GaveUpOn.Add(5);
        Assert.Equal("too_late", (await Assert.ThrowsAsync<AgentRefusal>(() => model.ClipboardFilesPlaceAsync(5))).Code);
        Assert.Equal("not_found", (await Assert.ThrowsAsync<AgentRefusal>(() => model.ClipboardFilesPlaceAsync(6))).Code);
        await model.ClipboardFilesPlacedAsync(4, ok: true);
        Assert.True(agent.Received[^1].ClipboardFilesPlaced.Ok);
        Assert.Equal("invalid_folder", (await Assert.ThrowsAsync<AgentRefusal>(() => model.PasteClipboardFilesAsync(4, "relative"))).Code);
        await model.PasteClipboardFilesAsync(4, @"C:\Users\me\AppData\Local\InputWeave\clipboard\4-0a1b2c3d");
        Assert.Equal(@"C:\Users\me\AppData\Local\InputWeave\clipboard\4-0a1b2c3d", agent.Received[^1].PasteClipboardFiles.Folder);
        await model.ClipboardFilesGoneAsync(4);
        Assert.Equal("not_found", (await Assert.ThrowsAsync<AgentRefusal>(() => model.ClipboardFilesGoneAsync(4))).Code);
        Assert.Equal("not_found", (await Assert.ThrowsAsync<AgentRefusal>(() => model.PasteClipboardFilesAsync(4, @"C:\x"))).Code);

        // The user quits: the background service hears, as the app goes.
        Assert.False(agent.UserQuit);
        await model.UserQuitAppAsync();
        Assert.True(agent.UserQuit);
    }
}
