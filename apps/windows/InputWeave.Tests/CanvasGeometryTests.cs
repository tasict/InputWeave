using Inputweave.App.V1;
using InputWeave.Canvas;
using Xunit;

namespace InputWeave.Tests;

/// <summary>The same cases as the macOS app's CanvasGeometryTests, and the mini map's.</summary>
public class CanvasGeometryTests
{
    private static readonly CanvasDevice Mac = new("mac", new(0, 0), [new CanvasRect(0, 0, 1512, 982)]);

    private static CanvasDevice Pc(double x, double y) => new("pc", new(x, y), [new CanvasRect(0, 0, 2560, 1440)]);

    private static ArrangedDevice Device(string id, double x, double y, params (double X, double Y, double W, double H, bool Primary)[] screens)
    {
        var device = new ArrangedDevice { Id = id, Name = id, Placed = true, X = x, Y = y };
        foreach (var s in screens)
        {
            device.Screens.Add(new Screen { Id = $"{id}-{s.X}", X = s.X, Y = s.Y, Width = s.W, Height = s.H, Primary = s.Primary });
        }
        return device;
    }

    [Fact]
    public void PlacesEachScreenAtItsDevicesOrigin()
    {
        var arrangement = new Arrangement();
        arrangement.Devices.Add(Device("mac", 0, 0, (0, 0, 1512, 982, true)));
        arrangement.Devices.Add(Device("pc", 1512, 0, (-1920, 0, 1920, 1080, false), (0, 0, 1920, 1080, true)));
        // Not placed yet: left out.
        arrangement.Devices.Add(new ArrangedDevice { Id = "new", Placed = false });

        var devices = CanvasGeometry.Devices(arrangement);
        Assert.Equal(["mac", "pc"], devices.Select(d => d.Id));
        Assert.Equal(new CanvasRect(-408, 0, 1920, 1080), devices[1].Frames[0]);
        Assert.Equal(1, devices[1].MainScreen);
        Assert.Equal(new CanvasRect(-408, 0, 3840, 1080), devices[1].Bounds);
    }

    [Fact]
    public void SnapsBesideAndLinesUpTops()
    {
        // Dropped a little right of the Mac and a little low.
        Assert.Equal(new CanvasPoint(1512, 0), CanvasGeometry.Snap(Pc(1520, 6), [Mac], threshold: 10));
    }

    [Fact]
    public void LeavesFarAwayDevicesAlone()
    {
        Assert.Equal(new CanvasPoint(1700, 300), CanvasGeometry.Snap(Pc(1700, 300), [Mac], threshold: 10));
    }

    [Fact]
    public void SnapsOnTop()
    {
        Assert.Equal(new CanvasPoint(0, -1440), CanvasGeometry.Snap(Pc(4, -1436), [Mac], threshold: 10));
    }

    [Fact]
    public void TouchingIsNotOverlapping()
    {
        Assert.False(CanvasGeometry.Overlaps(Mac.Frames, Pc(1512, 0).Frames.ToList()));
        Assert.True(CanvasGeometry.Overlaps(Mac.Frames, Pc(1500, 0).Frames.ToList()));
    }

    [Fact]
    public void FindsTheSharedEdge()
    {
        Assert.Equal(
            [new Seam(new CanvasPoint(1512, 400), new CanvasPoint(1512, 982))],
            CanvasGeometry.Seams([Mac, Pc(1512, 400)]));
        Assert.Empty(CanvasGeometry.Seams([Mac, Pc(1600, 0)]));
        Assert.Equal(["pc"], CanvasGeometry.Neighbours(Mac, [Mac, Pc(1512, 400)]));
        Assert.Empty(CanvasGeometry.Neighbours(Mac, [Mac, Pc(1600, 0)]));
    }

    [Fact]
    public void StitchesOnlyEdgesThatTouch()
    {
        var mac = new CanvasDevice("mac", new(0, 0), [new CanvasRect(0, 100, 1512, 982)]);
        var pc = new CanvasDevice("pc", new(0, 0), [new CanvasRect(1512, 0, 1920, 1080)]);
        var below = new CanvasDevice("tv", new(0, 0), [new CanvasRect(1600, 1080, 800, 600)]);
        var apart = new CanvasDevice("far", new(0, 0), [new CanvasRect(5000, 0, 100, 100)]);

        Assert.Equal(
            [
                new Seam(new CanvasPoint(1512, 100), new CanvasPoint(1512, 1080)),
                new Seam(new CanvasPoint(1600, 1080), new CanvasPoint(2400, 1080)),
            ],
            CanvasGeometry.Seams([mac, pc, below, apart]));
    }

    [Fact]
    public void AStitchStartsAndEndsOnTheSeam()
    {
        var points = CanvasGeometry.Stitch(new CanvasPoint(10, 0), new CanvasPoint(10, 20), pitch: 4, reach: 2);
        Assert.Equal(6, points.Count);
        Assert.Equal(new CanvasPoint(10, 0), points[0]);
        Assert.Equal(new CanvasPoint(10, 20), points[^1]);
        // Across the seam, alternately to either side.
        Assert.Equal(8, points[1].X, 6);
        Assert.Equal(12, points[2].X, 6);
        Assert.Empty(CanvasGeometry.Stitch(new CanvasPoint(1, 1), new CanvasPoint(1, 1), 4, 2));
    }

    [Fact]
    public void FitCentersContent()
    {
        var fit = CanvasGeometry.FitInto(new CanvasRect(0, 0, 200, 100), new CanvasSize(420, 420), padding: 10);
        Assert.Equal(2, fit.Scale);
        Assert.Equal(new CanvasPoint(10, 110), fit.ToView(new CanvasPoint(0, 0)));
        Assert.Equal(new CanvasPoint(410, 310), fit.ToView(new CanvasPoint(200, 100)));
        Assert.Equal(new CanvasPoint(200, 100), fit.ToCanvas(new CanvasPoint(410, 310)));

        var panel = CanvasGeometry.FitInto(new CanvasRect(0, 0, 3000, 1000), new CanvasSize(320, 120), padding: 10);
        Assert.Equal(0.1, panel.Scale, 6);
        Assert.Equal(new CanvasRect(10, 10, 300, 100), panel.ToView(new CanvasRect(0, 0, 3000, 1000)));
    }

    [Fact]
    public void LabelsTheMainScreenOrTheWidestThatHasRoom()
    {
        var room = new CanvasSize(36, 20);
        var roomy = new CanvasRect(0, 0, 90, 50);
        var narrow = new CanvasRect(90, -10, 30, 70);
        var wide = new CanvasRect(120, 0, 60, 40);
        Assert.Equal(0, CanvasGeometry.LabelTile([roomy, narrow, wide], main: 0, room));
        Assert.Equal(2, CanvasGeometry.LabelTile([roomy, narrow, wide], main: 2, room));
        Assert.Equal(0, CanvasGeometry.LabelTile([roomy, narrow, wide], main: 1, room));
        Assert.Null(CanvasGeometry.LabelTile([narrow], main: 0, room));
    }

    [Fact]
    public void CentersTheBadgeWhenTheNameLeavesTheMiddleFree()
    {
        var badge = new CanvasSize(30, 14);
        Assert.Equal(
            new CanvasRect(35, 23, 30, 14),
            CanvasGeometry.BadgeFrame(badge, new CanvasRect(10, 0, 80, 60), nameBottom: 17, inset: 2));
        // No name on this tile: the middle, whatever the tile's size.
        Assert.Equal(
            new CanvasRect(20, 10, 30, 14),
            CanvasGeometry.BadgeFrame(badge, new CanvasRect(10, 0, 50, 34), nameBottom: null, inset: 2));
    }

    [Fact]
    public void MovesTheBadgeBelowANameThatCoversTheMiddle()
    {
        var badge = new CanvasSize(30, 14);
        // The space below the name runs from 18 (a point under it) to 32.
        var tile = new CanvasRect(10, 0, 50, 34);
        var frame = CanvasGeometry.BadgeFrame(badge, tile, nameBottom: 17, inset: 2);
        Assert.Equal(new CanvasRect(20, 18, 30, 14), frame);
        Assert.True(tile.Inflate(-2, -2).Contains(frame));
        // Taller: centered in that space.
        Assert.Equal(
            new CanvasRect(20, 22, 30, 14),
            CanvasGeometry.BadgeFrame(badge, new CanvasRect(10, 0, 50, 42), nameBottom: 17, inset: 2));
    }

    [Fact]
    public void KeepsTheBadgeInsideATileWithoutRoomBelowTheName()
    {
        var badge = new CanvasSize(30, 14);
        // As low as the tile allows: over the name's last points.
        Assert.Equal(
            new CanvasRect(20, 10, 30, 14),
            CanvasGeometry.BadgeFrame(badge, new CanvasRect(10, 0, 50, 26), nameBottom: 17, inset: 2));
        // Larger than the tile: centered on it, spilling over evenly.
        Assert.Equal(
            new CanvasRect(5, -1, 30, 14),
            CanvasGeometry.BadgeFrame(badge, new CanvasRect(10, 0, 20, 12), nameBottom: 17, inset: 2));
    }

    [Fact]
    public void FindsTheComputerUnderThePointer()
    {
        var fit = new CanvasGeometry.Fit(0.1, new CanvasPoint(10, 10));
        CanvasDevice[] devices = [Mac, Pc(1512, 0)];
        Assert.Equal("mac", CanvasGeometry.HitTest(devices, fit, new CanvasPoint(20, 20), slop: 0));
        Assert.Equal("pc", CanvasGeometry.HitTest(devices, fit, new CanvasPoint(200, 20), slop: 0));
        Assert.Null(CanvasGeometry.HitTest(devices, fit, new CanvasPoint(20, 150), slop: 0));
        // Just outside the Mac's screen, within the slop.
        Assert.Equal("mac", CanvasGeometry.HitTest(devices, fit, new CanvasPoint(20, 110), slop: 4));
    }
}
