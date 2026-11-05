using System.Buffers.Binary;
using System.Globalization;
using System.Text;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Canvas;
using InputWeave.Drag;
using InputWeave.Notifications;
using InputWeave.Overlay;
using InputWeave.Panel;
using InputWeave.Support;
using Xunit;

namespace InputWeave.Tests;

/// <summary>Where the strips that catch a drag of files go: along every part of an edge the cursor leaves this computer by.</summary>
public class DragStripTests
{
    private static ArrangedDevice Device(string id, double x, double y, bool self, params (string Id, double X, double Y, double W, double H, double Scale)[] screens)
    {
        var device = new ArrangedDevice { Id = id, Name = id, X = x, Y = y, Placed = true, ThisDevice = self, Connected = !self };
        device.Screens.AddRange(screens.Select(s => new Screen { Id = s.Id, X = s.X, Y = s.Y, Width = s.W, Height = s.H, Scale = s.Scale }));
        return device;
    }

    private static Arrangement Arranged(params ArrangedDevice[] devices)
    {
        var arrangement = new Arrangement { Version = 1 };
        arrangement.Devices.AddRange(devices);
        return arrangement;
    }

    private static readonly IReadOnlySet<string> Mac = new HashSet<string> { "mac" };

    [Fact]
    public void LinesTheEdgeFacingTheOtherComputer()
    {
        // As on the build machine: the PC left of the Mac, edge to edge.
        var arrangement = Arranged(
            Device("pc", -1920, 0, true, (@"\\.\DISPLAY1", 0, 0, 1920, 1080, 1)),
            Device("mac", 0, 0, false, ("1", 0, 0, 1920, 1080, 2)));
        var strip = Assert.Single(DragStrips.For(arrangement, Mac));
        Assert.Equal(new LogicalStrip(@"\\.\DISPLAY1", StripEdge.Right, new CanvasRect(1880, 0, 40, 1080)), strip);
        // Not while the Mac is away.
        Assert.Empty(DragStrips.For(arrangement, new HashSet<string>()));
        // Nor before this computer has a place.
        arrangement.Devices[0].Placed = false;
        Assert.Empty(DragStrips.For(arrangement, Mac));
    }

    [Fact]
    public void CrossesGapsAndOnlyWhereTheOtherComputerIsNearest()
    {
        // The Mac above and to the right, apart from the PC: the cursor still
        // goes there through the right edge, and through the top.
        var apart = Arranged(
            Device("pc", 0, 0, true, (@"\\.\DISPLAY1", 0, 0, 1920, 1080, 1)),
            Device("mac", 2000, -500, false, ("1", 0, 0, 1000, 400, 2)));
        var strips = DragStrips.For(apart, Mac);
        Assert.Contains(strips, s => s.Edge == StripEdge.Right && s.Rect == new CanvasRect(1880, 0, 40, 1080));
        Assert.Contains(strips, s => s.Edge == StripEdge.Top && s.Rect == new CanvasRect(0, 0, 1920, 40));
        Assert.Equal(2, strips.Count);
    }

    [Fact]
    public void LeavesOutEdgesBetweenThisComputersOwnScreens()
    {
        // Two screens side by side, the second lower; the Mac above the
        // first's right edge, beyond it. Along that edge the second screen is
        // nearer below y = 270, so only the stretch above crosses.
        var arrangement = Arranged(
            Device("pc", 0, 0, true, (@"\\.\DISPLAY1", 0, 0, 1920, 1080, 1), (@"\\.\DISPLAY2", 1920, 540, 1920, 1080, 1)),
            Device("mac", 1920, -500, false, ("1", 0, 0, 1000, 500, 2)));
        var strips = DragStrips.For(arrangement, Mac);
        Assert.Contains(strips, s => s.ScreenId == @"\\.\DISPLAY1" && s.Edge == StripEdge.Right && s.Rect == new CanvasRect(1880, 0, 40, 270));
        Assert.DoesNotContain(strips, s => s.ScreenId == @"\\.\DISPLAY2" && s.Edge == StripEdge.Left);
        // The second screen's top, which faces the Mac across a gap.
        Assert.Contains(strips, s => s.ScreenId == @"\\.\DISPLAY2" && s.Edge == StripEdge.Top);
        Assert.Contains(strips, s => s.ScreenId == @"\\.\DISPLAY1" && s.Edge == StripEdge.Top);
    }

    [Fact]
    public void ReadsThisComputersScreensFromTheArrangement()
    {
        var arrangement = new Arrangement();
        var mac = new ArrangedDevice { Id = "mac" };
        mac.Screens.Add(new Screen { Id = "1", Width = 1512, Height = 982, Scale = 2 });
        var pc = new ArrangedDevice { Id = "pc", ThisDevice = true };
        pc.Screens.Add(new Screen { Id = @"\\.\DISPLAY1", X = 0, Y = 0, Width = 1920, Height = 1080, Scale = 1 });
        arrangement.Devices.Add(mac);
        arrangement.Devices.Add(pc);
        Assert.Equal(new[] { new LogicalScreen(@"\\.\DISPLAY1", 0, 0, 1920, 1080, 1) }, DragStrips.ThisComputersScreens(arrangement));
        Assert.Empty(DragStrips.ThisComputersScreens(new Arrangement()));
    }

    [Fact]
    public void PlacesStripsInPixelsMonitorByMonitor()
    {
        // The second screen at 150 %: logical 1920 wide is 2880 pixels.
        LogicalScreen[] screens =
        [
            new(@"\\.\DISPLAY1", 0, 0, 1920, 1080, 1),
            new(@"\\.\DISPLAY2", 1920, 0, 1920, 1080, 1.5),
        ];
        MonitorPixels[] monitors =
        [
            new(@"\\.\DISPLAY1", 0, 0, 1920, 1080, 1, Primary: true),
            new(@"\\.\DISPLAY2", 1920, 0, 4800, 1620, 1.5, Primary: false),
        ];
        LogicalStrip[] strips =
        [
            new(@"\\.\DISPLAY2", StripEdge.Right, new CanvasRect(3800, 0, 40, 1080)),
            new(@"\\.\DISPLAY1", StripEdge.Left, new CanvasRect(0, 100.5, 40, 200)),
        ];
        Assert.Equal(
            [new PixelRect(4740, 0, 60, 1620), new PixelRect(0, 100, 40, 201)],
            DragStrips.ToPixels(strips, screens, monitors));
        // A screen no monitor is named after: the main monitor, from the origin.
        var renamed = new LogicalStrip(@"\\.\DISPLAY9", StripEdge.Right, new CanvasRect(1880, 0, 40, 1080));
        Assert.Equal([new PixelRect(1880, 0, 40, 1080)], DragStrips.ToPixels([renamed], screens, monitors));
        Assert.Empty(DragStrips.ToPixels(strips, screens, []));
    }

    [Fact]
    public void TellsWhetherThePointerIsInAStrip()
    {
        PixelRect[] strips = [new(1880, 0, 40, 1080)];
        Assert.True(DragStrips.Contains(strips, new PixelPoint(1919, 500)));
        Assert.False(DragStrips.Contains(strips, new PixelPoint(1920, 500)));
        Assert.False(DragStrips.Contains(strips, new PixelPoint(1879, 500)));
    }

    [Fact]
    public void StandsOnlyWhileFilesCanLeaveFromHere()
    {
        Assert.True(DragStrips.Shown(Sample.Model()));
        Assert.False(DragStrips.Shown(Sample.Model(sharing: false)));
        Assert.False(DragStrips.Shown(Sample.Model(locked: true)));
        // Controlled from the Mac: only if the Mac can carry a drag from here.
        Assert.True(DragStrips.Shown(Sample.Model(active: "pc", controlledBy: "mac", dragsCarried: true)));
        Assert.False(DragStrips.Shown(Sample.Model(active: "pc", controlledBy: "mac")));
        Assert.False(DragStrips.Shown(Sample.Model(AgentLink.Connecting)));
        Assert.Equal(["mac"], DragStrips.Enterable(Sample.Model()));
        Assert.Empty(DragStrips.Enterable(Sample.Model(macOnline: false)));
        // Files turned off with the Mac: no strips towards it.
        Assert.Empty(DragStrips.Enterable(Sample.Model(macFilesOff: true)));
    }

    [Fact]
    public void ArePassedThroughForAPressThatBeganOnThem()
    {
        var strips = new[] { new PixelRect(1880, 0, 40, 1080) };
        var press = new StripPress();
        Assert.False(press.Solid(false, new PixelPoint(500, 500), strips));
        // A drag coming towards the edge: pressed away from it.
        Assert.True(press.Solid(true, new PixelPoint(500, 500), strips));
        Assert.True(press.Solid(true, new PixelPoint(1900, 500), strips));
        Assert.False(press.Solid(false, new PixelPoint(1900, 500), strips));
        // Pressed on the strip, on what lies below it: it gets the button up too.
        Assert.False(press.Solid(true, new PixelPoint(1900, 1070), strips));
        Assert.False(press.Solid(true, new PixelPoint(500, 500), strips));
        Assert.False(press.Solid(false, new PixelPoint(500, 500), strips));
        // The next press decides anew.
        Assert.True(press.Solid(true, new PixelPoint(600, 500), strips));
        // Without strips any press is outside them.
        Assert.True(new StripPress().Solid(true, new PixelPoint(1900, 500), []));
    }

    [Fact]
    public void ArePlacedAgainOnlyWhenWhatTheyDependOnChanges()
    {
        var model = Sample.Model();
        var inputs = StripInputs.Of(model);
        // Transfers and round-trip times come and go: the same strips.
        var transfers = new Transfers();
        transfers.Transfers_.Add(new FileTransfer { TransferId = "t", DoneBytes = 1 });
        Assert.Equal(inputs, StripInputs.Of(model with { Transfers = transfers }));
        var status = model.Status.Clone();
        status.Devices[0].RttMs = 12;
        Assert.Equal(inputs, StripInputs.Of(model with { Status = status }));

        Assert.NotEqual(inputs, StripInputs.Of(Sample.Model(macFilesOff: true)));
        Assert.NotEqual(inputs, StripInputs.Of(Sample.Model(macOnline: false)));
        Assert.NotEqual(inputs, StripInputs.Of(Sample.Model(locked: true)));
        Assert.NotEqual(inputs, StripInputs.Of(Sample.Model(AgentLink.Connecting)));
        var arrangement = new Arrangement { Version = 2 };
        Assert.NotEqual(inputs, StripInputs.Of(model with { Arrangement = arrangement }));
        Assert.Equal(StripInputs.Of(model with { Arrangement = arrangement }), StripInputs.Of(model with { Arrangement = arrangement.Clone() }));
    }

    [Fact]
    public void EndsTheDragTheServiceBeganUnlessItCrossed()
    {
        Assert.False(StripLeave.Ends(null, null));
        // The status naming the drag may come after the answer to BeginDrag.
        Assert.True(StripLeave.Ends("d1", null));
        Assert.True(StripLeave.Ends("d1", new Dragging { DragId = "d1" }));
        Assert.True(StripLeave.Ends("d1", new Dragging { DragId = "d0", DeviceId = "mac" }));
        // Across on the Mac: the service ends it there.
        Assert.False(StripLeave.Ends("d1", new Dragging { DragId = "d1", DeviceId = "mac" }));
    }
}

/// <summary>The files another computer drags here, as File Explorer is told about them.</summary>
public class PromisedFileTests
{
    private static DragArrived Offer(params (string Name, bool Folder, ulong Size, uint Files)[] items)
    {
        var offer = new DragArrived { DragId = "d1", FromDeviceId = "mac" };
        offer.Items.AddRange(items.Select(i => new DraggedItem { Name = i.Name, Folder = i.Folder, Size = i.Size, Files = i.Files }));
        return offer;
    }

    [Fact]
    public void NamesTheItemsOfferedBeforeAnyHasArrived()
    {
        var offer = Offer(("Photos", true, 1_000_000, 312), ("Report.pdf", false, 2_000, 1), ("..\\evil:name?.", false, 5, 1));
        Assert.Equal(
            [
                new PromisedEntry("Photos", true, 0),
                new PromisedEntry("Report.pdf", false, 2_000),
                new PromisedEntry(".._evil_name_", false, 5),
            ],
            FileDescriptors.Offered(offer));
        Assert.Equal("_", FileDescriptors.SafeName(".."));
        Assert.Equal("a_b", FileDescriptors.SafeName("a/b"));
    }

    [Fact]
    public void ListsWhatArrivedFoldersFirstWithoutPartialFiles()
    {
        var root = Path.Combine(Path.GetTempPath(), $"iw-staged-{Guid.NewGuid():N}");
        try
        {
            Directory.CreateDirectory(Path.Combine(root, "Photos", "raw"));
            Directory.CreateDirectory(Path.Combine(root, "Photos", "empty"));
            File.WriteAllText(Path.Combine(root, "Photos", "b.jpg"), "bb");
            File.WriteAllText(Path.Combine(root, "Photos", "A.jpg"), "a");
            File.WriteAllText(Path.Combine(root, "Photos", "raw", "IMG_0001.raw"), "raw!");
            File.WriteAllText(Path.Combine(root, "Photos", ".inputweave-t-9.part"), "unfinished");
            File.WriteAllText(Path.Combine(root, "Report.pdf"), "report");
            var deep = Path.Combine(root, "Deep");
            // Deep\<130>\<130> does not fit a descriptor's 259 characters.
            var name = new string('x', 130);
            Directory.CreateDirectory(Path.Combine(deep, name, name));
            File.WriteAllText(Path.Combine(deep, name, name, "far.txt"), "far");

            var entries = FileDescriptors.Staged(
                [Path.Combine(root, "Photos"), Path.Combine(root, "Report.pdf"), deep, Path.Combine(root, "gone.txt")],
                out int tooLong);
            Assert.Equal(
                [
                    "Photos", @"Photos\A.jpg", @"Photos\b.jpg", @"Photos\empty", @"Photos\raw", @"Photos\raw\IMG_0001.raw",
                    "Report.pdf", "Deep", $@"Deep\{name}",
                ],
                entries.Select(e => e.RelativePath));
            Assert.Equal(1, tooLong);
            var raw = entries.Single(e => e.RelativePath == @"Photos\raw\IMG_0001.raw");
            Assert.Equal((false, 4ul, Path.Combine(root, "Photos", "raw", "IMG_0001.raw")), (raw.IsFolder, raw.Size, raw.Source));
            Assert.True(entries.Single(e => e.RelativePath == @"Photos\empty").IsFolder);
        }
        finally
        {
            Directory.Delete(root, recursive: true);
        }
    }

    [Fact]
    public void EncodesAFileGroupDescriptor()
    {
        var bytes = FileDescriptors.Encode(
        [
            new PromisedEntry("Photos", true, 0),
            new PromisedEntry(@"Photos\big.mov", false, 0x1_2345_6789),
        ]);
        Assert.Equal(4 + 2 * FileDescriptors.DescriptorSize, bytes.Length);
        Assert.Equal(2u, BinaryPrimitives.ReadUInt32LittleEndian(bytes));
        var folder = bytes.AsSpan(4, FileDescriptors.DescriptorSize);
        var file = bytes.AsSpan(4 + FileDescriptors.DescriptorSize, FileDescriptors.DescriptorSize);
        Assert.Equal(
            FileDescriptors.FdAttributes | FileDescriptors.FdProgressUi | FileDescriptors.FdUnicode,
            BinaryPrimitives.ReadUInt32LittleEndian(folder));
        Assert.Equal(FileDescriptors.FileAttributeDirectory, BinaryPrimitives.ReadUInt32LittleEndian(folder[36..]));
        Assert.Equal(
            FileDescriptors.FdAttributes | FileDescriptors.FdProgressUi | FileDescriptors.FdUnicode | FileDescriptors.FdFileSize,
            BinaryPrimitives.ReadUInt32LittleEndian(file));
        Assert.Equal(FileDescriptors.FileAttributeNormal, BinaryPrimitives.ReadUInt32LittleEndian(file[36..]));
        Assert.Equal(1u, BinaryPrimitives.ReadUInt32LittleEndian(file[64..]));
        Assert.Equal(0x2345_6789u, BinaryPrimitives.ReadUInt32LittleEndian(file[68..]));
        Assert.Equal(@"Photos\big.mov", Encoding.Unicode.GetString(file[72..]).TrimEnd('\0'));
        Assert.Equal("Photos", Encoding.Unicode.GetString(folder[72..]).TrimEnd('\0'));
    }
}

/// <summary>What a drag offered here does as it goes, and where its files wait.</summary>
public class DragStagingTests
{
    private static FileTransfer Row(string id, TransferState state, string drag = "d1", uint index = 0, uint items = 2, bool outgoing = false) =>
        new()
        {
            TransferId = id, DragId = drag, State = state, BatchIndex = index, BatchItems = items, Outgoing = outgoing,
            Path = state == TransferState.Done ? $@"C:\stage\{id}" : "",
        };

    private static Transfers With(params FileTransfer[] rows)
    {
        var transfers = new Transfers();
        transfers.Transfers_.AddRange(rows);
        return transfers;
    }

    [Fact]
    public void KeepsEachDragInAFolderOfItsOwn()
    {
        var root = DragStaging.Root(@"C:\Users\me\AppData\Local");
        Assert.Equal(@"C:\Users\me\AppData\Local\InputWeave\drag".Replace('\\', Path.DirectorySeparatorChar), root.Replace('\\', Path.DirectorySeparatorChar));
        Assert.Equal("4f9c-0a_B", DragStaging.SafeId("4f9c-0a_B"));
        // An id from the other computer that is no plain name is hashed.
        var odd = DragStaging.SafeId(@"..\..\Windows");
        Assert.Matches("^[0-9a-f]{32}$", odd);
        Assert.Equal(odd, DragStaging.SafeId(@"..\..\Windows"));
        Assert.Matches("^[0-9a-f]{32}$", DragStaging.SafeId(""));
        Assert.Matches("^[0-9a-f]{32}$", DragStaging.SafeId(new string('a', 65)));
        Assert.EndsWith(Path.DirectorySeparatorChar + "4f9c", DragStaging.FolderFor(root, "4f9c"));
    }

    [Fact]
    public void WaitsForEveryItemOfTheDrag()
    {
        // Not yet listed: not finished.
        Assert.False(DragStaging.Batch(With(), "d1", 2).Finished);
        // One of two arrived, the other still coming.
        var half = DragStaging.Batch(With(Row("a", TransferState.Done), Row("b", TransferState.Transferring, index: 1)), "d1", 2);
        Assert.False(half.Finished);
        Assert.Equal([@"C:\stage\a"], half.Arrived);
        // Both ended, one failed: finished, with what arrived.
        var done = DragStaging.Batch(
            With(Row("b", TransferState.Failed, index: 1), Row("a", TransferState.Done), Row("x", TransferState.Done, drag: "other"), Row("o", TransferState.Done, outgoing: true)),
            "d1", 2);
        Assert.True(done.Finished);
        Assert.Equal(["a", "b"], done.Rows.Select(r => r.TransferId));
        Assert.Equal([@"C:\stage\a"], done.Arrived);
        // Only one row listed of the two offered: not finished.
        Assert.False(DragStaging.Batch(With(Row("a", TransferState.Done, items: 0)), "d1", 2).Finished);
    }

    [Fact]
    public void DropsOnlyWhenTheUserLetsGo()
    {
        Assert.Equal(DragRelease.Continue, DragStaging.OnQuery(false, true, false, true, false));
        Assert.Equal(DragRelease.Drop, DragStaging.OnQuery(false, false, false, true, false));
        Assert.Equal(DragRelease.Cancel, DragStaging.OnQuery(true, true, false, true, false));
        // The offer was taken back.
        Assert.Equal(DragRelease.Cancel, DragStaging.OnQuery(false, true, true, true, false));
        // Let go at the edge the cursor goes back by, or once no longer controlled.
        Assert.Equal(DragRelease.Cancel, DragStaging.OnQuery(false, false, false, true, true));
        Assert.Equal(DragRelease.Cancel, DragStaging.OnQuery(false, false, false, false, false));
        // Held at the edge is still a drag.
        Assert.Equal(DragRelease.Continue, DragStaging.OnQuery(false, true, false, true, true));

        Assert.True(DragStaging.TakesDrop(offerEnded: false, controlledBySource: true));
        Assert.False(DragStaging.TakesDrop(offerEnded: true, controlledBySource: true));
        Assert.False(DragStaging.TakesDrop(offerEnded: false, controlledBySource: false));
    }

    [Fact]
    public void KnowsWhetherTheCursorIsStillWhereTheDragCameWithIt()
    {
        var controlled = Sample.Model(active: "pc", controlledBy: "mac");
        // Offered by the Mac, which controls this computer.
        var offered = new DragArrived { DragId = "d1", FromDeviceId = "mac" };
        Assert.True(DragStaging.StillHere(offered, controlled));
        Assert.False(DragStaging.StillHere(offered, Sample.Model(active: "pc")));
        Assert.False(DragStaging.StillHere(offered, Sample.Model(active: "pc", controlledBy: "mini")));
        // Offered by the Mac for files on another computer it controls.
        var carried = new DragArrived { DragId = "d2", FromDeviceId = "mac", SourceDeviceId = "mini" };
        Assert.True(DragStaging.StillHere(carried, controlled));
        // Carried back here from the Mac, which this computer controls:
        // here while nothing controls it and the cursor is on it.
        var back = new DragArrived { DragId = "d3", FromDeviceId = "mac", AtController = true };
        Assert.True(DragStaging.StillHere(back, Sample.Model(active: "pc")));
        Assert.False(DragStaging.StillHere(back, controlled));
        // Gone on to the Mac again.
        Assert.False(DragStaging.StillHere(back, Sample.Model(active: "mac")));
    }

    [Fact]
    public void WaitsForATargetThatCopiesOnItsOwnTime()
    {
        Assert.Equal(AfterDrop.Wait, DragStaging.Next(accepted: false, copyingStarted: true, copyingEnded: false));
        Assert.Equal(AfterDrop.Wait, DragStaging.Next(accepted: true, copyingStarted: true, copyingEnded: false));
        Assert.Equal(AfterDrop.Finish, DragStaging.Next(accepted: true, copyingStarted: true, copyingEnded: true));
        // Read while it took the drop.
        Assert.Equal(AfterDrop.Finish, DragStaging.Next(accepted: true, copyingStarted: false, copyingEnded: false));
        // Not dropped, or taken by a target that asked for nothing.
        Assert.Equal(AfterDrop.Decline, DragStaging.Next(accepted: false, copyingStarted: false, copyingEnded: false));
        Assert.Equal(AfterDrop.Decline, DragStaging.Next(accepted: false, copyingStarted: true, copyingEnded: true));
    }

    [Fact]
    public void SaysWhyADropWasRefused()
    {
        Assert.Equal(new UiText(TextKeys.HudDragInvalidFolder), DragStaging.AcceptRefused("invalid_folder"));
        Assert.Equal(new UiText(TextKeys.HudDragGone), DragStaging.AcceptRefused("not_found"));
        Assert.Equal(new UiText(TextKeys.HudDragNotReceived), DragStaging.AcceptRefused("no_broker"));
    }
}

/// <summary>Rows for folders and batches, and what failed in them.</summary>
public class FolderRowTests
{
    private static readonly CultureInfo En = CultureInfo.GetCultureInfo("en-US");

    private static FileTransfer Folder(TransferState state, bool outgoing = true, uint files = 312, uint done = 0, uint failed = 0) =>
        new()
        {
            TransferId = "f", Name = "Photos", Folder = true, Outgoing = outgoing, State = state,
            Size = 1_288_490_189, DoneBytes = 644_245_094, Files = files, FilesDone = done, FilesFailed = failed,
            Path = @"C:\Users\me\Photos",
        };

    [Fact]
    public void CountsAFoldersFiles()
    {
        Assert.Equal(new UiText(TextKeys.TransferFolderFiles, "312", "1.2 GB"), TransferText.FolderLine(Folder(TransferStates.Queued), En));
        Assert.Equal(new UiText(TextKeys.TransferFolderFilesOne, "1.2 GB"), TransferText.FolderLine(Folder(TransferState.Preparing, files: 1), En));
        Assert.Equal(new UiText(TextKeys.TransferFolderProgress, "8", "312"), TransferText.FolderLine(Folder(TransferState.Transferring, done: 8), En));
        Assert.Equal(0.5, TransferText.Fraction(Folder(TransferState.Transferring, done: 8))!.Value, 3);
        // Ended with every file: the count, and "Sent" below.
        var sent = Folder(TransferState.Done, done: 312);
        Assert.Equal(new UiText(TextKeys.TransferFolderFiles, "312", "1.2 GB"), TransferText.FolderLine(sent, En));
        Assert.Equal(new UiText(TextKeys.TransferSent), TransferText.Detail(sent, "Mac", En));
        Assert.False(TransferText.IsProblem(sent));
        Assert.Null(TransferText.FolderLine(new FileTransfer { Name = "a.txt", State = TransferState.Done }, En));
    }

    [Fact]
    public void SaysHowManyFailedAndWhy()
    {
        var some = Folder(TransferState.Done, done: 310, failed: 2);
        some.Failures.Add(new FailedFile { RelativePath = "Photos/raw/IMG_0001.CR3", Reason = "unreadable" });
        some.Failures.Add(new FailedFile { RelativePath = "Photos/link", Reason = "symlink" });
        Assert.Equal(new UiText(TextKeys.TransferSentSome, "310", "2"), TransferText.FolderLine(some, En));
        Assert.Null(TransferText.Detail(some, "Mac", En));
        Assert.True(TransferText.IsProblem(some));
        Assert.True(TransferText.FolderLineIsProblem(some));
        Assert.False(TransferText.DetailIsProblem(some));
        Assert.Equal(
            [
                new UiText(TextKeys.TransferFailureLine, "Photos/raw/IMG_0001.CR3", new UiText(TextKeys.TransferUnreadable)),
                new UiText(TextKeys.TransferFailureLine, "Photos/link", new UiText(TextKeys.TransferSymlink)),
            ],
            TransferText.FailureLines(some));
        Assert.Equal(0u, TransferText.UnlistedFailures(some));
        // In the panel it stays until dismissed, and can still be shown.
        Assert.Equal(TransferActions.Show | TransferActions.Dismiss, TransferText.Actions(some, dismissible: true, removable: false));
        Assert.Equal(TransferActions.Show | TransferActions.Remove, TransferText.Actions(some, dismissible: false, removable: true));
        var many = Folder(TransferState.Done, outgoing: false, done: 100, failed: 150);
        Assert.Equal(new UiText(TextKeys.TransferReceivedSome, "100", "150"), TransferText.FolderLine(many, En));
        Assert.Equal(150u, TransferText.UnlistedFailures(many));
        // None arrived: the folder failed, with its first failure's reason.
        var none = Folder(TransferState.Failed, done: 0, failed: 312);
        none.Reason = "not_a_file";
        Assert.Equal(new UiText(TextKeys.TransferSentSome, "0", "312"), TransferText.FolderLine(none, En));
        Assert.Equal(new UiText(TextKeys.TransferNotAFile), TransferText.Detail(none, "Mac", En));
    }

    [Fact]
    public void ExplainsTheNewReasons()
    {
        Assert.Equal(new UiText(TextKeys.TransferSymlink), TransferText.Failed("symlink"));
        Assert.Equal(new UiText(TextKeys.TransferNotAFile), TransferText.Failed("not_a_file"));
        var tooLarge = new FileTransfer { State = TransferState.Declined, Reason = "too_large", Outgoing = true };
        Assert.Equal(new UiText(TextKeys.TransferPeerTooLarge), TransferText.Detail(tooLarge, "Mac", En));
        var badName = new FileTransfer { State = TransferState.Declined, Reason = "invalid_path", Outgoing = true };
        Assert.Equal(new UiText(TextKeys.TransferPeerInvalidName), TransferText.Detail(badName, "Mac", En));
        badName.Outgoing = false;
        Assert.Equal(new UiText(TextKeys.TransferInvalidName), TransferText.Detail(badName, "Mac", En));
        Assert.Equal(new UiText(TextKeys.TransferTooMany), TransferText.FailureReason("too_large", outgoing: false));
        Assert.Equal(new UiText(TextKeys.TransferNoSpaceShort), TransferText.FailureReason("no_space", outgoing: true));
        Assert.Equal(new UiText(TextKeys.TransferCorrupt), TransferText.FailureReason("corrupt", outgoing: false));
    }

    [Fact]
    public void KeepsABatchTogetherInTheOrderSent()
    {
        FileTransfer Item(string id, string batch, uint index) => new() { TransferId = id, BatchId = batch, BatchIndex = index };
        // Newest first from the service; a batch's items may come in any order.
        var rows = TransferText.Ordered([Item("b2", "B", 1), Item("lone", "", 0), Item("b0", "B", 0), Item("a0", "A", 0), Item("b1", "B", 2)]);
        Assert.Equal(["b0", "b2", "b1", "lone", "a0"], rows.Select(r => r.TransferId));
    }

    [Fact]
    public void ShowsNothingForWhatADragDroppedHere()
    {
        var dropped = new FileTransfer { State = TransferState.Done, Path = @"C:\Users\me\AppData\Local\InputWeave\drag\d1\a.txt", DragId = "d1" };
        Assert.Equal(TransferActions.Remove, TransferText.Actions(dropped, dismissible: false, removable: true));
        // Sent by a drag from here: the file is where it was.
        dropped.Outgoing = true;
        Assert.Equal(TransferActions.Show | TransferActions.Remove, TransferText.Actions(dropped, dismissible: false, removable: true));
    }

    [Fact]
    public void AnnouncesFoldersAndWhatFailed()
    {
        var culture = CultureInfo.InvariantCulture;
        var photos = Folder(TransferState.Done, outgoing: false, done: 312);
        Assert.Equal(
            new UiText(TextKeys.NotifyReceivedItems, new UiText(TextKeys.ItemsFolderOne), new UiText(TextKeys.ItemsFileMany, "312"), "1.2 GB", "Mac mini"),
            TransferText.Received([photos], "Mac mini", culture));
        var some = Folder(TransferState.Done, outgoing: false, done: 310, failed: 2);
        var file = new FileTransfer { Name = "a.txt", Size = 1024, State = TransferState.Done };
        var lost = new FileTransfer { Name = "b.txt", Size = 1024, State = TransferState.Failed };
        Assert.Equal(
            new UiText(
                TextKeys.NotifyFailedMany,
                new UiText(
                    TextKeys.NotifyReceivedItems,
                    new UiText(TextKeys.ItemsAnd, new UiText(TextKeys.ItemsFolderOne), new UiText(TextKeys.ItemsFileOne)),
                    new UiText(TextKeys.ItemsFileMany, "311"), "1.2 GB", "Mac mini"),
                "3"),
            TransferText.Received([some, file, lost], "Mac mini", culture));
        Assert.Equal(
            new UiText(TextKeys.NotifyFailedOne, new UiText(TextKeys.NotifyReceivedOne, "a.txt", "1 KB", "Mac mini")),
            TransferText.Received([file, lost], "Mac mini", culture));
        Assert.Null(TransferText.Received([lost], "Mac mini", culture));
        Assert.Equal(
            new UiText(TextKeys.ItemsAnd, new UiText(TextKeys.ItemsFolderMany, "2"), new UiText(TextKeys.ItemsFileMany, "3")),
            TransferText.Items(2, 3, culture));
    }
}

/// <summary>Announcing files a drag brought, and failures alongside what arrived.</summary>
public class BatchNewsTests
{
    private static readonly DateTimeOffset T = Sample.Now;

    private static AgentSnapshot With(params FileTransfer[] files)
    {
        var transfers = new Transfers();
        transfers.Transfers_.AddRange(files);
        return Sample.Model() with { Transfers = transfers };
    }

    private static FileTransfer File(string id, TransferState state, string drag = "") =>
        new() { TransferId = id, DeviceId = "mac", Name = id, Size = 1024, State = state, DragId = drag };

    [Fact]
    public void LeavesOutWhatADragBroughtHere()
    {
        var news = new TransferNews();
        news.Update(With(), T);
        news.Update(With(File("d", TransferState.Done, drag: "d1")), T);
        Assert.Empty(news.Update(With(File("d", TransferState.Done, drag: "d1")), T.AddSeconds(2)).Announce);
    }

    [Fact]
    public void CountsFailuresAlongsideWhatArrived()
    {
        var news = new TransferNews();
        news.Update(With(), T);
        var update = With(File("ok", TransferState.Done), File("bad", TransferState.Failed));
        news.Update(update, T);
        var group = Assert.Single(news.Update(update, T.AddSeconds(2)).Announce);
        Assert.Equal(["ok", "bad"], group.Files.Select(f => f.TransferId));
        Assert.Equal(["ok"], group.Arrived.Select(f => f.TransferId));
        // Failures alone are no news: the panel shows them.
        var failed = new TransferNews();
        failed.Update(With(), T);
        failed.Update(With(File("bad", TransferState.Failed)), T);
        Assert.Empty(failed.Update(With(File("bad", TransferState.Failed)), T.AddSeconds(2)).Announce);
    }
}

/// <summary>What the HUD says as files are dragged between computers.</summary>
public class DragHudTests
{
    private static AgentSnapshot Dragging(string device, uint items, string id = "d1")
    {
        var model = Sample.Model(active: device.Length == 0 ? "pc" : device);
        var status = model.Status.Clone();
        status.Dragging = new Inputweave.App.V1.Dragging { DragId = id, Items = items, DeviceId = device };
        return model with { Status = status };
    }

    [Fact]
    public void SaysWhereADragGoes()
    {
        var hud = new DragHud();
        Assert.Null(hud.Update(Sample.Model()));
        // Entering the strip says nothing yet; crossing says where.
        Assert.Null(hud.Update(Dragging("", 3)));
        Assert.Equal(HudCue.Show(new UiText(TextKeys.HudDraggingMany, "3", "MacBook Pro")), hud.Update(Dragging("mac", 3)));
        Assert.Null(hud.Update(Dragging("mac", 3)));
        var one = new DragHud();
        one.Update(Sample.Model());
        Assert.Equal(HudCue.Show(new UiText(TextKeys.HudDraggingOne, "MacBook Pro")), one.Update(Dragging("mac", 1)));

        var accepted = Dragging("mac", 3) with
        {
            DragFinish = new DragFinish(1, new DragEnded { DragId = "d1", Reason = DragEnd.Accepted, DeviceId = "mac" }),
        };
        Assert.Equal(HudCue.Show(new UiText(TextKeys.HudDragSending, "MacBook Pro")), hud.Update(accepted));
        var declined = accepted with
        {
            DragFinish = new DragFinish(2, new DragEnded { DragId = "d2", Reason = DragEnd.Declined, DeviceId = "mac" }),
        };
        Assert.Equal(HudCue.Show(new UiText(TextKeys.HudDragDeclined, "MacBook Pro")), hud.Update(declined));
        // Released here: nothing to say.
        Assert.Null(hud.Update(declined with
        {
            DragFinish = new DragFinish(3, new DragEnded { DragId = "d3", Reason = DragEnd.Released }),
        }));
    }

    [Fact]
    public void AsksForTheDropUntilTheOfferEnds()
    {
        var hud = new DragHud();
        var model = Sample.Model(active: "pc", controlledBy: "mac");
        hud.Update(model);
        var offered = model with { DragOffer = new DragOffer(1, new DragArrived { DragId = "o1", FromDeviceId = "mac" }) };
        Assert.Equal(HudCue.Show(new UiText(TextKeys.HudDropHere), hold: true), hud.Update(offered));
        Assert.Null(hud.Update(offered));
        var withdrawn = offered with { DragFinish = new DragFinish(1, new DragEnded { DragId = "o1", Reason = DragEnd.Withdrawn, DeviceId = "mac" }) };
        Assert.Equal(HudCue.TakeDown, hud.Update(withdrawn));
    }

    [Fact]
    public void TheTakeBackHintGivesWayToADrag()
    {
        var watch = new HudWatch();
        int hints = 0;
        watch.Update(Sample.Model(), ref hints);
        Assert.Null(watch.Update(Dragging("mac", 2), ref hints));
        Assert.Equal(0, hints);
    }
}

/// <summary>The drag commands and events, against a stand-in for the background service.</summary>
public class DragModelTests
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
    public async Task FollowsDragsOfferedAndEnded()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model();
        model.Start();
        await Eventually(() => model.Snapshot.Link == AgentLink.Connected);
        Assert.Null(model.Snapshot.DragOffer);

        var arrived = new DragArrived { DragId = "o1", FromDeviceId = "mac", Files = 3, Bytes = 3072 };
        arrived.Items.Add(new DraggedItem { Name = "Photos", Folder = true, Size = 3072, Files = 3 });
        await agent.SendAsync(new Event { DragArrived = arrived });
        await Eventually(() => model.Snapshot.DragOffer is not null);
        Assert.Equal(1ul, model.Snapshot.DragOffer!.Serial);
        Assert.Equal("Photos", model.Snapshot.DragOffer.Arrived.Items.Single().Name);

        await agent.SendAsync(new Event { DragEnded = new DragEnded { DragId = "o1", Reason = DragEnd.Withdrawn, DeviceId = "mac" } });
        await Eventually(() => model.Snapshot.DragFinish is not null);
        Assert.Equal(DragEnd.Withdrawn, model.Snapshot.DragFinish!.Ended.Reason);
        // The offer stays as the last one seen.
        Assert.Equal(1ul, model.Snapshot.DragOffer.Serial);
    }

    [Fact]
    public async Task BeginsAndEndsADragFromHere()
    {
        var agent = new FakeAgent();
        await using var model = agent.Model();
        model.Start();
        await Eventually(() => model.Snapshot.Link == AgentLink.Connected);

        var begun = await model.BeginDragAsync([(@"C:\Users\me\Photos", true), (@"C:\Users\me\a.txt", false)]);
        Assert.Equal("d1", begun.DragId);
        Assert.Equal(2u, begun.Items);
        var sent = agent.Received[^1].BeginDrag.Items;
        Assert.Equal([(@"C:\Users\me\Photos", true), (@"C:\Users\me\a.txt", false)], sent.Select(i => (i.Path, i.IsDir)));
        await model.EndDragAsync("d1");
        Assert.Null(model.Snapshot.Status.Dragging);
        Assert.Equal("not_found", (await Assert.ThrowsAsync<AgentRefusal>(() => model.EndDragAsync("d1"))).Code);
        Assert.Equal("not_dragging", (await Assert.ThrowsAsync<AgentRefusal>(() => model.BeginDragAsync([]))).Code);
    }

    [Fact]
    public async Task AcceptsOrDeclinesADragOfferedHere()
    {
        var agent = new FakeAgent { Offered = "o1" };
        await using var model = agent.Model();
        model.Start();
        await Eventually(() => model.Snapshot.Link == AgentLink.Connected);

        Assert.Equal("invalid_folder", (await Assert.ThrowsAsync<AgentRefusal>(() => model.AcceptDragAsync("o1", "relative"))).Code);
        await model.AcceptDragAsync("o1", @"C:\Users\me\AppData\Local\InputWeave\drag\o1");
        Assert.Equal(@"C:\Users\me\AppData\Local\InputWeave\drag\o1", agent.Received[^1].AcceptDrag.Destination);
        Assert.Equal("o1", model.Snapshot.Transfers.Transfers_.Single().DragId);
        Assert.Equal("not_found", (await Assert.ThrowsAsync<AgentRefusal>(() => model.DeclineDragAsync("o1"))).Code);
        agent.Offered = "o2";
        await model.DeclineDragAsync("o2");
        Assert.Equal("o2", agent.Received[^1].DeclineDrag.DragId);
    }
}

public class DragLaunchTests
{
    [Fact]
    public void ReadsTheDragsToSimulate()
    {
        var launch = Launch.Parse([Launch.SimulateDragArrivedArgument, @"D:\a b\Photos", @"D:\Report.txt", "--language", "zh-TW"]);
        Assert.Equal([@"D:\a b\Photos", @"D:\Report.txt"], launch.SimulateDragArrived);
        Assert.True(launch.Testing);
        Assert.Null(launch.SetupAt(finishedSetup: false));
        Assert.Equal([@"D:\x.txt"], Launch.Parse([Launch.SimulateDragEnterArgument, @"D:\x.txt"]).SimulateDragEnter);
        Assert.Null(Launch.Parse([]).SimulateDragEnter);
    }
}
