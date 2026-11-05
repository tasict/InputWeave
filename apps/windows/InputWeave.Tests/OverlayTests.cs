using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Overlay;
using InputWeave.Panel;
using InputWeave.Support;
using Xunit;

namespace InputWeave.Tests;

/// <summary>How the cursor grows and where its copy goes, as the macOS app's LocatorGeometryTests.</summary>
public class LocatorGeometryTests
{
    [Fact]
    public void GrowsHoldsAndShrinksBackUnlessMotionIsReduced()
    {
        var moving = LocatorGeometry.For(reduceMotion: false);
        Assert.Equal(new LocatorGeometry.Motion(4, 0.15, 0.6, 0.35), moving);
        Assert.Equal(1.1, moving.Shown, 6);
        Assert.Equal(1, LocatorGeometry.Scale(moving, 0));
        // Easing out: most of the way there halfway through growing.
        Assert.True(LocatorGeometry.Scale(moving, moving.Grow / 2) > 3);
        Assert.Equal(4, LocatorGeometry.Scale(moving, moving.Grow));
        Assert.Equal(4, LocatorGeometry.Scale(moving, moving.Grow + moving.Hold - 0.01));
        // Halfway back, halfway between; its own size as it goes.
        double halfway = moving.Grow + moving.Hold + moving.Shrink / 2;
        Assert.Equal(2.5, LocatorGeometry.Scale(moving, halfway), 6);
        // Easing in and out: little of the way a quarter through shrinking, most of it three quarters through.
        Assert.True(LocatorGeometry.Scale(moving, moving.Grow + moving.Hold + moving.Shrink / 4) > 3.5);
        Assert.True(LocatorGeometry.Scale(moving, moving.Grow + moving.Hold + moving.Shrink * 3 / 4) < 1.5);
        Assert.Equal(1, LocatorGeometry.Scale(moving, moving.Shown - 0.001), 2);
        Assert.Equal(1, LocatorGeometry.Scale(moving, moving.Shown));
        Assert.Equal(1, LocatorGeometry.Scale(moving, 5));

        var still = LocatorGeometry.For(reduceMotion: true);
        Assert.Equal(new LocatorGeometry.Motion(3, 0, 1, 0), still);
        Assert.Equal(1, still.Shown);
        Assert.All(new[] { 0, 0.5, 0.99 }, seconds => Assert.Equal(3, LocatorGeometry.Scale(still, seconds)));
    }

    /// <summary>The arrow at 100 %: 32 × 32 pixels, its tip 5 in from the top left.</summary>
    private static readonly CursorShape Arrow = new(32, 32, 5, 5);

    [Fact]
    public void KeepsTheHotspotOnTheCursor()
    {
        var cursor = new PixelPoint(700, 482);
        Assert.Equal((695.0, 477.0, 32.0, 32.0), LocatorGeometry.Frame(cursor, Arrow, 1));
        Assert.Equal((680.0, 462.0, 128.0, 128.0), LocatorGeometry.Frame(cursor, Arrow, 4));
        var shrinking = LocatorGeometry.Frame(cursor, Arrow, 2.5);
        Assert.Equal(80, shrinking.Width);
        Assert.Equal((700.0, 482.0), (shrinking.X + 5 * 2.5, shrinking.Y + 5 * 2.5));
        // On a monitor left of and above the main one.
        Assert.Equal((-1020.0, -220.0, 128.0, 128.0), LocatorGeometry.Frame(new PixelPoint(-1000, -200), Arrow, 4));
    }

    [Fact]
    public void FitsTheWindowToTheCopyAtItsPeak()
    {
        var cursor = new PixelPoint(700, 482);
        var moving = LocatorGeometry.For(false);
        var window = LocatorGeometry.Window(cursor, Arrow, moving);
        Assert.Equal(new PixelRect(680, 462, 128, 128), window);
        Assert.Equal(new PixelRect(685, 467, 96, 96), LocatorGeometry.Window(cursor, Arrow, LocatorGeometry.For(true)));
        // The copy stays inside it as it shrinks towards its hotspot.
        foreach (double seconds in new[] { 0, 0.1, 0.5, 0.9, 1.09 })
        {
            var frame = LocatorGeometry.Frame(cursor, Arrow, LocatorGeometry.Scale(moving, seconds));
            Assert.True(frame.X >= window.X && frame.Y >= window.Y, $"{seconds}");
            Assert.True(frame.X + frame.Width <= window.X + window.Width && frame.Y + frame.Height <= window.Y + window.Height, $"{seconds}");
        }
        // A hotspot between pixels, from a larger copy: whole pixels, rounded outwards.
        var between = new CursorShape(32, 32, 15.75, 15.75);
        Assert.Equal(new PixelRect(452, 452, 97, 97), LocatorGeometry.Window(new PixelPoint(500, 500), between, LocatorGeometry.For(true)));
        Assert.Equal(new PixelRect(437, 437, 128, 128), LocatorGeometry.Window(new PixelPoint(500, 500), between, moving));
    }

    [Fact]
    public void TakesTheCopysHotspotOnlyNearTheCursors()
    {
        // Loaded at four times the size, the copy's own hotspot (63 of 128) is more exact.
        Assert.Equal(15.75, LocatorGeometry.Hotspot(63 / 4.0, 15));
        Assert.Equal(0, LocatorGeometry.Hotspot(0, 0));
        // Stretched without the hotspot along: the cursor's.
        Assert.Equal(15, LocatorGeometry.Hotspot(15 / 4.0, 15));
        Assert.Equal(10, LocatorGeometry.Hotspot(12, 10));
    }
}

/// <summary>A cursor's pixels as GetDIBits gives them, made into premultiplied BGRA.</summary>
public class CursorPixelsTests
{
    /// <summary>A one-bit mask of <paramref name="width"/>, top-down, from rows of 0 and 1.</summary>
    private static byte[] Mask(int width, params string[] rows)
    {
        int stride = CursorPixels.MaskStride(width);
        var mask = new byte[stride * rows.Length];
        for (int y = 0; y < rows.Length; y++)
        {
            for (int x = 0; x < width; x++)
            {
                if (rows[y][x] == '1')
                {
                    mask[y * stride + x / 8] |= (byte)(0x80 >> (x % 8));
                }
            }
        }
        return mask;
    }

    [Fact]
    public void PadsMaskRowsTo32Bits()
    {
        Assert.Equal(4, CursorPixels.MaskStride(1));
        Assert.Equal(4, CursorPixels.MaskStride(32));
        Assert.Equal(8, CursorPixels.MaskStride(33));
        Assert.Equal(16, CursorPixels.MaskStride(128));
    }

    [Fact]
    public void PremultipliesColorWithAlphaWhateverTheMask()
    {
        // BGRA: opaque red, half-transparent blue, half-transparent white, clear.
        byte[] color = [0, 0, 255, 255, 255, 0, 0, 128, 255, 255, 255, 128, 90, 90, 90, 0];
        var pixels = CursorPixels.Premultiplied(4, 1, color, Mask(4, "1111"));
        Assert.Equal(new byte[] { 0, 0, 255, 255, 128, 0, 0, 128, 128, 128, 128, 128, 0, 0, 0, 0 }, pixels);
    }

    [Fact]
    public void TakesAlphaFromTheMaskForColorWithout()
    {
        // No alpha byte set anywhere: opaque where AND is clear, clear where it is set.
        byte[] color = [10, 20, 30, 0, 40, 50, 60, 0, 70, 80, 90, 0, 255, 255, 255, 0];
        var pixels = CursorPixels.Premultiplied(2, 2, color, Mask(2, "01", "10"));
        Assert.Equal(new byte[] { 10, 20, 30, 255, 0, 0, 0, 0, 0, 0, 0, 0, 255, 255, 255, 255 }, pixels);
    }

    [Fact]
    public void DrawsMonochromeCursorsInBlackAndWhite()
    {
        // AND above XOR: black, white, the screen (clear), and the screen
        // inverted, which a window cannot do and draws black.
        var mask = Mask(4, "0011", "0101");
        var pixels = CursorPixels.Premultiplied(4, 1, [], mask);
        Assert.Equal(new byte[] { 0, 0, 0, 255, 255, 255, 255, 255, 0, 0, 0, 0, 0, 0, 0, 255 }, pixels);
    }

    [Fact]
    public void ReadsBitsPastTheFirstByteOfARow()
    {
        // A 10-pixel row spans two bytes; the ninth pixel is the next byte's highest bit.
        var mask = Mask(10, "1111111101", "0000000010");
        var pixels = CursorPixels.Premultiplied(10, 1, [], mask);
        Assert.Equal(new byte[] { 0, 0, 0, 0 }, pixels[(7 * 4)..(8 * 4)]);
        Assert.Equal(new byte[] { 255, 255, 255, 255 }, pixels[(8 * 4)..(9 * 4)]);
        Assert.Equal(new byte[] { 0, 0, 0, 0 }, pixels[(9 * 4)..(10 * 4)]);
    }

    [Fact]
    public void RefusesTooFewBytes()
    {
        Assert.Throws<ArgumentException>(() => CursorPixels.Premultiplied(2, 2, new byte[15], Mask(2, "00", "00")));
        Assert.Throws<ArgumentException>(() => CursorPixels.Premultiplied(2, 2, [], Mask(2, "00", "00")));
    }
}

/// <summary>What the HUD says, as the macOS app's HUDController decides it.</summary>
public class HudWatchTests
{
    private static AgentSnapshot With(AgentSnapshot model, Action<Status> change)
    {
        var status = model.Status.Clone();
        change(status);
        return model with { Status = status };
    }

    [Fact]
    public void RemembersTheFirstStateAndTellsOnlyChanges()
    {
        var watch = new HudWatch();
        int hints = 0;
        // Already locked and on the Mac when the app starts: nothing to say.
        var start = With(Sample.Model(active: "mac", locked: true), s => s.ControlReturns = 4);
        Assert.Null(watch.Update(start, ref hints));
        Assert.Null(watch.Update(start, ref hints));
        Assert.Equal(0, hints);
    }

    [Fact]
    public void SaysWhyTheKeyboardAndMouseCameBack()
    {
        var watch = new HudWatch();
        int hints = 3;
        var model = Sample.Model(active: "mac");
        watch.Update(model, ref hints);
        Assert.Equal(new UiText(TextKeys.HudPeerLost, "MacBook Pro"), watch.Update(With(model, s =>
        {
            s.ActiveDeviceId = "pc";
            s.ControlReturns = 1;
            s.LastReturn = ReturnReason.PeerLost;
            s.LastReturnDeviceId = "mac";
        }), ref hints));
        var back = With(model, s =>
        {
            s.ActiveDeviceId = "pc";
            s.ControlReturns = 2;
            s.LastReturn = ReturnReason.RemoteActivity;
            s.LastReturnDeviceId = "mac";
        });
        Assert.Equal(new UiText(TextKeys.HudRemoteActivity, "MacBook Pro"), watch.Update(back, ref hints));
        Assert.Null(watch.Update(back, ref hints));
        Assert.Equal(new UiText(TextKeys.HudBackHere), watch.Update(With(back, s =>
        {
            s.ControlReturns = 3;
            s.LastReturn = ReturnReason.Hotkey;
        }), ref hints));
        Assert.Equal(new UiText(TextKeys.HudLayoutChanged), watch.Update(With(back, s =>
        {
            s.ControlReturns = 4;
            s.LastReturn = ReturnReason.LayoutChanged;
        }), ref hints));
        // Pausing says so in the panel; no note.
        Assert.Null(watch.Update(With(back, s =>
        {
            s.ControlReturns = 5;
            s.LastReturn = ReturnReason.SharingPaused;
        }), ref hints));
        Assert.Equal(new UiText(TextKeys.HudScreenLocked, "MacBook Pro"), watch.Update(With(back, s =>
        {
            s.ControlReturns = 6;
            s.LastReturn = ReturnReason.ScreenLocked;
            s.LastReturnDeviceId = "mac";
        }), ref hints));
        Assert.Equal(new UiText(TextKeys.HudInputUnavailable, "MacBook Pro"), watch.Update(With(back, s =>
        {
            s.ControlReturns = 7;
            s.LastReturn = ReturnReason.InputUnavailable;
            s.LastReturnDeviceId = "mac";
        }), ref hints));
        Assert.Equal(new UiText(TextKeys.HudPeerPaused, "MacBook Pro"), watch.Update(With(back, s =>
        {
            s.ControlReturns = 8;
            s.LastReturn = ReturnReason.PeerPaused;
            s.LastReturnDeviceId = "mac";
        }), ref hints));
        Assert.Equal(new UiText(TextKeys.HudCaptureEnded), watch.Update(With(back, s =>
        {
            s.ControlReturns = 9;
            s.LastReturn = ReturnReason.CaptureEnded;
        }), ref hints));
    }

    [Fact]
    public void SaysWhereTheCursorIsLocked()
    {
        var watch = new HudWatch();
        int hints = 3;
        watch.Update(Sample.Model(), ref hints);
        Assert.Equal(new UiText(TextKeys.HudLockedHere, "Ctrl + Alt + Shift + L"), watch.Update(Sample.Model(locked: true), ref hints));
        Assert.Equal(new UiText(TextKeys.HudUnlocked), watch.Update(Sample.Model(), ref hints));
        watch.Update(Sample.Model(active: "mac"), ref hints);
        Assert.Equal(
            new UiText(TextKeys.HudLockedTo, "MacBook Pro", "Ctrl + Alt + Shift + L"),
            watch.Update(Sample.Model(active: "mac", locked: true), ref hints));
    }

    [Fact]
    public void WritesTheHotkeysTheServiceReports()
    {
        var watch = new HudWatch();
        int hints = 0;
        var hotkeys = new Hotkeys { TakeBack = "control+alt+F12", Lock = "gui+shift+KeyK", JumpModifiers = "" };
        watch.Update(Sample.Model(hotkeys: hotkeys), ref hints);
        Assert.Equal(
            new UiText(TextKeys.HudTakeBackHint, "Ctrl + Alt + F12"),
            watch.Update(Sample.Model(active: "mac", hotkeys: hotkeys), ref hints));
        Assert.Equal(
            new UiText(TextKeys.HudLockedTo, "MacBook Pro", "Shift + Win + K"),
            watch.Update(Sample.Model(active: "mac", locked: true, hotkeys: hotkeys), ref hints));
    }

    [Fact]
    public void HintsHowToTakeBackControlThreeTimes()
    {
        var watch = new HudWatch();
        int hints = 0;
        var here = Sample.Model();
        var there = Sample.Model(active: "mac");
        watch.Update(here, ref hints);
        for (int i = 1; i <= HudWatch.HintTimes; i++)
        {
            Assert.Equal(new UiText(TextKeys.HudTakeBackHint, "Ctrl + Alt + Shift + Esc"), watch.Update(there, ref hints));
            Assert.Equal(i, hints);
            Assert.Null(watch.Update(there, ref hints));
            watch.Update(here, ref hints);
        }
        Assert.Null(watch.Update(there, ref hints));
        Assert.Equal(HudWatch.HintTimes, hints);
    }

    private static readonly Hotkeys WithRelative = new()
    {
        TakeBack = Shortcut.DefaultTakeBack, Lock = Shortcut.DefaultLock,
        JumpModifiers = Shortcut.DefaultJumpModifiers, Relative = Shortcut.DefaultRelative,
    };

    [Fact]
    public void SaysWhenRelativeModeStartsAndEnds()
    {
        // The service says why it ended.
        var watch = new HudWatch();
        int hints = 3;
        var there = Sample.Model(active: "mac", hotkeys: WithRelative);
        watch.Update(there, ref hints);
        var on = Sample.Model(active: "mac", hotkeys: WithRelative, relative: "mac");
        Assert.Equal(new UiText(TextKeys.HudRelativeEntered, "MacBook Pro", "Ctrl + Alt + Shift + G"), watch.Update(on, ref hints));
        Assert.Null(watch.Update(on, ref hints));
        Assert.Equal(
            new UiText(TextKeys.HudRelativeEnded),
            watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relativeEnds: 1, endReason: RelativeEndReason.TurnedOff), ref hints));

        // Its hotkey off: it ends as control is taken back.
        var off = WithRelative.Clone();
        off.Relative = "";
        Assert.Equal(
            new UiText(TextKeys.HudRelativeEnteredTakeBack, "MacBook Pro", "Ctrl + Alt + Shift + Esc"),
            watch.Update(Sample.Model(active: "mac", hotkeys: off, relative: "mac", relativeAutomatic: true, relativeEnds: 1, endReason: RelativeEndReason.TurnedOff), ref hints));
        // The game let go.
        Assert.Equal(
            new UiText(TextKeys.HudRelativeReleased, "MacBook Pro"),
            watch.Update(Sample.Model(active: "mac", hotkeys: off, relativeEnds: 2, endReason: RelativeEndReason.Released), ref hints));
        // Ended and began again between two states: it began.
        var entered = new UiText(TextKeys.HudRelativeEntered, "MacBook Pro", "Ctrl + Alt + Shift + G");
        Assert.Equal(entered, watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relative: "mac", relativeEnds: 2), ref hints));
        Assert.Equal(
            entered,
            watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relative: "mac", relativeEnds: 3, endReason: RelativeEndReason.TurnedOff), ref hints));
        // Turning automatic relative mode off, or jumping, ends it.
        foreach (var (ends, reason) in new[] { (4UL, RelativeEndReason.AutoOff), (6UL, RelativeEndReason.Jumped) })
        {
            Assert.Equal(
                new UiText(TextKeys.HudRelativeEnded),
                watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relativeEnds: ends, endReason: reason), ref hints));
            watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relative: "mac", relativeEnds: ends), ref hints);
        }
        // Sharing paused: the panel says so.
        Assert.Null(watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relativeEnds: 7, endReason: RelativeEndReason.SharingPaused), ref hints));
    }

    [Fact]
    public void WhyTheKeyboardAndMouseCameBackComesBeforeRelativeMode()
    {
        var watch = new HudWatch();
        int hints = 3;
        watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relative: "mac"), ref hints);
        var back = With(Sample.Model(hotkeys: WithRelative, relativeEnds: 1, endReason: RelativeEndReason.DeviceLost), s =>
        {
            s.ControlReturns = 1;
            s.LastReturn = ReturnReason.PeerLost;
            s.LastReturnDeviceId = "mac";
        });
        Assert.Equal(new UiText(TextKeys.HudPeerLost, "MacBook Pro"), watch.Update(back, ref hints));
        // Taken back with the hotkey: that ends relative mode, nothing more to say.
        watch.Update(With(Sample.Model(active: "mac", hotkeys: WithRelative, relative: "mac", relativeEnds: 1), s => s.ControlReturns = 1), ref hints);
        Assert.Equal(new UiText(TextKeys.HudBackHere), watch.Update(With(Sample.Model(hotkeys: WithRelative, relativeEnds: 2, endReason: RelativeEndReason.ControlReturned), s =>
        {
            s.ControlReturns = 2;
            s.LastReturn = ReturnReason.Hotkey;
        }), ref hints));
        // Nor afterwards.
        Assert.Null(watch.Update(With(Sample.Model(hotkeys: WithRelative, relativeEnds: 2, endReason: RelativeEndReason.ControlReturned), s =>
        {
            s.ControlReturns = 2;
            s.LastReturn = ReturnReason.Hotkey;
        }), ref hints));
    }

    [Fact]
    public void TellsWhatAGameDoesNowAndThen()
    {
        // For each computer the first of each kind,
        // then at most one a minute; what the user does, always.
        var watch = new HudWatch();
        int hints = 3;
        var start = Sample.Now;
        UiText? At(double seconds, string relative = "", bool automatic = true, ulong ends = 0, RelativeEndReason reason = RelativeEndReason.Released) =>
            watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relative: relative, relativeAutomatic: automatic, relativeEnds: ends, endReason: reason), ref hints, start.AddSeconds(seconds));
        var entered = new UiText(TextKeys.HudRelativeEntered, "MacBook Pro", "Ctrl + Alt + Shift + G");
        var released = new UiText(TextKeys.HudRelativeReleased, "MacBook Pro");
        At(0);
        Assert.Equal(entered, At(1, "mac"));
        Assert.Equal(released, At(5, ends: 1));
        Assert.Null(At(10, "mac", ends: 1));
        Assert.Null(At(20, ends: 2));
        // By hand, always.
        Assert.Equal(entered, At(21, "mac", automatic: false, ends: 2));
        Assert.Equal(new UiText(TextKeys.HudRelativeEnded), At(22, ends: 3, reason: RelativeEndReason.TurnedOff));
        Assert.Null(At(64, "mac", ends: 3));
        Assert.Null(At(64.5, ends: 4));
        // A minute after the last one told.
        Assert.Equal(entered, At(65, "mac", ends: 4));
        Assert.Null(At(100, ends: 5));
        Assert.Null(At(110, "mac", ends: 5));
        Assert.Equal(released, At(126, ends: 6));
    }

    [Fact]
    public void TheFirstCrossingIntoRelativeModeSaysHowToTakeBackControlToo()
    {
        var watch = new HudWatch();
        int hints = 0;
        watch.Update(Sample.Model(hotkeys: WithRelative), ref hints);
        Assert.Equal(
            new UiText(TextKeys.HudTwoLines,
                new UiText(TextKeys.HudTakeBackHint, "Ctrl + Alt + Shift + Esc"),
                new UiText(TextKeys.HudRelativeEntered, "MacBook Pro", "Ctrl + Alt + Shift + G")),
            watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relative: "mac", relativeAutomatic: true), ref hints));
        Assert.Equal(1, hints);
        // Once the hints are used up, relative mode alone.
        hints = HudWatch.HintTimes;
        watch.Update(Sample.Model(hotkeys: WithRelative, relativeEnds: 1, endReason: RelativeEndReason.ControlReturned), ref hints);
        Assert.Equal(
            new UiText(TextKeys.HudRelativeEntered, "MacBook Pro", "Ctrl + Alt + Shift + G"),
            watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relative: "mac", relativeEnds: 1), ref hints));
    }

    [Fact]
    public void FewerEndsThanBeforeAreARestartedService()
    {
        var watch = new HudWatch();
        int hints = 3;
        watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relative: "mac", relativeEnds: 5), ref hints);
        // Counted from 0 again, with relative mode on elsewhere or off: no note.
        Assert.Null(watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relative: "mac", relativeEnds: 1, endReason: RelativeEndReason.TurnedOff), ref hints));
        Assert.Null(watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relativeEnds: 0), ref hints));
        // From there, an increase is news again.
        Assert.Equal(
            new UiText(TextKeys.HudRelativeEnded),
            watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relativeEnds: 1, endReason: RelativeEndReason.TurnedOff), ref hints));
    }

    [Fact]
    public void RelativeModeAfterReachingTheServiceAgainIsNoNews()
    {
        var watch = new HudWatch();
        int hints = 3;
        watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relative: "mac", relativeEnds: 4), ref hints);
        Assert.Null(watch.Update(Sample.Model(AgentLink.Connecting), ref hints));
        // A restarted service counts from 0, with relative mode on again.
        Assert.Null(watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relative: "mac", relativeAutomatic: true), ref hints));
        Assert.Equal(
            new UiText(TextKeys.HudRelativeEnded),
            watch.Update(Sample.Model(active: "mac", hotkeys: WithRelative, relativeEnds: 1, endReason: RelativeEndReason.TurnedOff), ref hints));
    }

    [Fact]
    public void SaysWhenAComputerHasNoRelativeMode()
    {
        var watch = new HudWatch();
        int hints = 3;
        var there = Sample.Model(active: "mac", hotkeys: WithRelative);
        watch.Update(there, ref hints);
        var refused = there with { RelativeRefusal = RelativeRefusal.After(null, "mac") };
        Assert.Equal(new UiText(TextKeys.HudRelativeUnsupported, "MacBook Pro"), watch.Update(refused, ref hints));
        Assert.Null(watch.Update(refused, ref hints));
        // Each time counts anew.
        Assert.Equal(
            new UiText(TextKeys.HudRelativeUnsupported, "MacBook Pro"),
            watch.Update(refused with { RelativeRefusal = RelativeRefusal.After(refused.RelativeRefusal, "mac") }, ref hints));

        // A refusal from before the app started is no news.
        var later = new HudWatch();
        Assert.Null(later.Update(refused, ref hints));
        Assert.Null(later.Update(refused, ref hints));
    }

    [Fact]
    public void SaysNothingWhileTheServiceIsOutOfReach()
    {
        var watch = new HudWatch();
        int hints = 0;
        watch.Update(Sample.Model(), ref hints);
        Assert.Null(watch.Update(Sample.Model(AgentLink.Connecting, locked: true), ref hints));
        // A restarted service unlocks.
        watch.Update(Sample.Model(locked: true), ref hints);
        Assert.Equal(new UiText(TextKeys.HudUnlocked), watch.Update(Sample.Model(), ref hints));
    }
}

/// <summary>Where the edge lights up and for how long, as the macOS app's EdgeGlowTests.</summary>
public class EdgeGlowTests
{
    private static CursorCrossed Crossed(
        ScreenEdge edge, double along, double x = 0, double y = 0, double w = 1920, double h = 1080, string id = @"\\.\DISPLAY1") => new()
    {
        DisplayId = id,
        Edge = edge,
        Along = along,
        DisplayX = x,
        DisplayY = y,
        DisplayWidth = w,
        DisplayHeight = h,
    };

    private static readonly MonitorPixels Main = new(@"\\.\DISPLAY1", 0, 0, 1920, 1080, 1, true);

    [Fact]
    public void PlacesTheCrossingAlongTheDisplay()
    {
        Assert.Equal(0.5, EdgeGlow.Fraction(Crossed(ScreenEdge.Right, 540)));
        Assert.Equal(0.25, EdgeGlow.Fraction(Crossed(ScreenEdge.Top, 480)));
        // On a display that does not start at the origin.
        Assert.Equal(0.25, EdgeGlow.Fraction(Crossed(ScreenEdge.Bottom, 2190, x: 1920, w: 1080)));
        // Outside it, at its nearest end.
        Assert.Equal(0, EdgeGlow.Fraction(Crossed(ScreenEdge.Left, -50)));
        Assert.Equal(1, EdgeGlow.Fraction(Crossed(ScreenEdge.Left, 5000)));
    }

    [Fact]
    public void RunsTheWholeEdge()
    {
        Assert.Equal(new PixelRect(0, 0, 24, 1080), EdgeGlow.Strip(Main, ScreenEdge.Left, EdgeGlow.Depth));
        Assert.Equal(new PixelRect(1896, 0, 24, 1080), EdgeGlow.Strip(Main, ScreenEdge.Right, EdgeGlow.Depth));
        Assert.Equal(new PixelRect(0, 0, 1920, 24), EdgeGlow.Strip(Main, ScreenEdge.Top, EdgeGlow.Depth));
        Assert.Equal(new PixelRect(0, 1076, 1920, 4), EdgeGlow.Strip(Main, ScreenEdge.Bottom, EdgeGlow.SolidDepth));
        // At 150%, in pixels, on a monitor not at the origin.
        var scaled = new MonitorPixels("x", 1920, -200, 1920 + 2160, -200 + 3840, 1.5, false);
        Assert.Equal(new PixelRect(1920, -200, 36, 3840), EdgeGlow.Strip(scaled, ScreenEdge.Left, EdgeGlow.Depth));
        Assert.Equal(new PixelRect(1920, 3604, 2160, 36), EdgeGlow.Strip(scaled, ScreenEdge.Bottom, EdgeGlow.Depth));
        // Never deeper than the monitor.
        var tiny = new MonitorPixels("t", 0, 0, 120, 20, 1, false);
        Assert.Equal(new PixelRect(0, 0, 120, 20), EdgeGlow.Strip(tiny, ScreenEdge.Top, EdgeGlow.Depth));
    }

    [Fact]
    public void IsBrightestWhereTheCursorCrossedAndShowsAllAlong()
    {
        Assert.Equal(1, EdgeGlow.Brightness(0.3, 0.3), 9);
        // Crossed in the middle: about two fifths at both ends.
        Assert.Equal(EdgeGlow.Brightness(0, 0.5), EdgeGlow.Brightness(1, 0.5), 9);
        Assert.InRange(EdgeGlow.Brightness(0, 0.5), 0.35, 0.45);
        // Crossed at one end: the other end still shows.
        Assert.InRange(EdgeGlow.Brightness(1, 0), 0.35, 0.36);
        // Dimmer the further from it.
        Assert.True(EdgeGlow.Brightness(0.6, 0.2) > EdgeGlow.Brightness(0.8, 0.2));
    }

    [Fact]
    public void DrawsTheGlowStrongestAtTheEdgeAndWhereItCrossed()
    {
        var thread = ((byte)0x8a, (byte)0x46, (byte)0xd3);
        // A right edge: 4 wide, 8 high; the screen's edge on the right.
        var right = EdgeGlow.Pixels(ScreenEdge.Right, 8, 4, 0.5, thread);
        Assert.Equal(8 * 4 * 4, right.Length);
        byte Alpha(byte[] pixels, int width, int x, int y) => pixels[(y * width + x) * 4 + 3];
        Assert.True(Alpha(right, 4, 3, 4) > Alpha(right, 4, 2, 4));
        Assert.True(Alpha(right, 4, 2, 4) > Alpha(right, 4, 0, 4));
        // Brighter where it crossed than at the ends, which still show.
        Assert.True(Alpha(right, 4, 3, 4) > Alpha(right, 4, 3, 0));
        Assert.True(Alpha(right, 4, 3, 0) > 0);
        // Premultiplied.
        int i = (4 * 4 + 3) * 4;
        var red = 0x8a * right[i + 3] / 255.0;
        Assert.InRange(right[i + 2], red - 1, red + 1);
        Assert.True(right[i + 2] <= right[i + 3]);
        // A top edge: 8 wide, 4 high; the screen's edge on top.
        var top = EdgeGlow.Pixels(ScreenEdge.Top, 8, 4, 0, thread);
        Assert.True(Alpha(top, 8, 0, 0) > Alpha(top, 8, 0, 3));
        Assert.True(Alpha(top, 8, 0, 0) > Alpha(top, 8, 7, 0));
        // Painted again into the same pixels, as the window does while the
        // brightest part slides: all of them, as new.
        var again = new byte[right.Length];
        EdgeGlow.Paint(again, ScreenEdge.Right, 8, 4, 0.1, thread);
        EdgeGlow.Paint(again, ScreenEdge.Right, 8, 4, 0.5, thread);
        Assert.Equal(right, again);
        // Left and bottom are mirrored.
        var left = EdgeGlow.Pixels(ScreenEdge.Left, 8, 4, 0.5, thread);
        Assert.Equal(Alpha(right, 4, 3, 4), Alpha(left, 4, 0, 4));
        var bottom = EdgeGlow.Pixels(ScreenEdge.Bottom, 8, 4, 0, thread);
        Assert.Equal(Alpha(top, 8, 0, 0), Alpha(bottom, 8, 0, 3));
    }

    [Fact]
    public void TheBrightestPartSlidesToANewCrossing()
    {
        Assert.Equal(0.2, EdgeGlow.SpotAt(0.2, 0.8, TimeSpan.Zero), 9);
        // Easing out: more than halfway at half the time.
        Assert.True(EdgeGlow.SpotAt(0.2, 0.8, EdgeGlow.Slide / 2) > 0.5);
        Assert.Equal(0.8, EdgeGlow.SpotAt(0.2, 0.8, EdgeGlow.Slide));
        Assert.Equal(0.8, EdgeGlow.SpotAt(0.2, 0.8, TimeSpan.FromSeconds(1)));
    }

    [Fact]
    public void FindsTheMonitorByNameElseByItsOnlySize()
    {
        var side = new MonitorPixels(@"\\.\DISPLAY2", 1920, 0, 3840, 1080, 1, false);
        var hidpi = new MonitorPixels(@"\\.\DISPLAY3", -2560, 0, 0, 1440, 2, false);
        IReadOnlyList<MonitorPixels> monitors = [Main, side, hidpi];
        Assert.Equal(side, EdgeGlow.MonitorFor(Crossed(ScreenEdge.Left, 10, id: @"\\.\display2"), monitors));
        // Not a name of these: the only one of its size in view units.
        Assert.Equal(hidpi, EdgeGlow.MonitorFor(Crossed(ScreenEdge.Left, 10, w: 1280, h: 720, id: "gone"), monitors));
    }

    [Fact]
    public void TellsTwoIdenticalMonitorsApartByWhereTheyAre()
    {
        // Two of the same size, neither by the name given: the one at the
        // display's place, in view units.
        var side = new MonitorPixels(@"\\.\DISPLAY2", 1920, 0, 3840, 1080, 1, false);
        IReadOnlyList<MonitorPixels> monitors = [Main, side];
        Assert.Equal(side, EdgeGlow.MonitorFor(Crossed(ScreenEdge.Left, 540, x: 1920, id: "gone"), monitors));
        Assert.Equal(Main, EdgeGlow.MonitorFor(Crossed(ScreenEdge.Right, 540, id: "gone"), monitors));
        // At 150 %, the same.
        var left = new MonitorPixels(@"\\.\DISPLAY5", 0, 0, 2880, 1620, 1.5, true);
        var right = new MonitorPixels(@"\\.\DISPLAY6", 2880, 0, 5760, 1620, 1.5, false);
        Assert.Equal(right, EdgeGlow.MonitorFor(Crossed(ScreenEdge.Top, 2400, x: 1920, id: "gone"), [left, right]));
        // The name comes first, wherever the display was said to be.
        Assert.Equal(Main, EdgeGlow.MonitorFor(Crossed(ScreenEdge.Left, 540, x: 1920), monitors));
        // Neither by name nor by place, and two of its size: none.
        Assert.Null(EdgeGlow.MonitorFor(Crossed(ScreenEdge.Left, 540, x: 700, id: "gone"), monitors));
    }

    [Fact]
    public void LightsOnlyTheFourEdges()
    {
        Assert.True(EdgeGlow.Known(ScreenEdge.Left));
        Assert.True(EdgeGlow.Known(ScreenEdge.Right));
        Assert.True(EdgeGlow.Known(ScreenEdge.Top));
        Assert.True(EdgeGlow.Known(ScreenEdge.Bottom));
        Assert.False(EdgeGlow.Known(ScreenEdge.Unspecified));
        // One a newer background service might send.
        Assert.False(EdgeGlow.Known((ScreenEdge)7));
    }

    [Fact]
    public void CrossingTheSameEdgeWhileItShowsCarriesOnWithoutGoingDark()
    {
        // Each edge of each monitor has its own window, which keeps when
        // its glow lit; another edge's crossing has nothing of it.
        var timing = EdgeGlow.Animated;
        var start = DateTime.UtcNow;
        DateTime Relit(double ms) => EdgeGlow.LitAt(start, start.AddMilliseconds(ms), timing);
        // Held: full at once, and held anew from now.
        var held = Relit(300);
        Assert.Equal(1, EdgeGlow.Opacity(timing, start.AddMilliseconds(300) - held));
        Assert.Equal(1, EdgeGlow.Opacity(timing, start.AddMilliseconds(600) - held));
        // Fading, halfway out: from half as bright, up again.
        var fading = Relit(600);
        Assert.Equal(0.5, EdgeGlow.Opacity(timing, start.AddMilliseconds(600) - fading)!.Value, 6);
        Assert.Equal(1, EdgeGlow.Opacity(timing, start.AddMilliseconds(650) - fading)!.Value, 6);
        // Coming up: on from there, not from dark.
        var rising = Relit(50);
        Assert.Equal(0.5, EdgeGlow.Opacity(timing, start.AddMilliseconds(50) - rising)!.Value, 6);
        // Gone, or never lit: anew.
        Assert.Equal(start.AddMilliseconds(900), Relit(900));
        Assert.Equal(start, EdgeGlow.LitAt(null, start, timing));
        // Animations off: a crossing while it shows keeps it there, for half a second again.
        Assert.Equal(start.AddMilliseconds(400), EdgeGlow.LitAt(start, start.AddMilliseconds(400), EdgeGlow.Still));
    }

    [Fact]
    public void FadesInHoldsAndFadesOutUnlessAnimationsAreOff()
    {
        var timing = EdgeGlow.For(reduceMotion: false);
        Assert.Equal(EdgeGlow.Animated, timing);
        Assert.Equal(0, EdgeGlow.Opacity(timing, TimeSpan.Zero));
        Assert.Equal(0.5, EdgeGlow.Opacity(timing, TimeSpan.FromMilliseconds(50))!.Value, 6);
        Assert.Equal(1, EdgeGlow.Opacity(timing, TimeSpan.FromMilliseconds(350)));
        Assert.Equal(0.5, EdgeGlow.Opacity(timing, TimeSpan.FromMilliseconds(600))!.Value, 6);
        Assert.Null(EdgeGlow.Opacity(timing, TimeSpan.FromMilliseconds(800)));
        // Animations off: there at once for half a second.
        var still = EdgeGlow.For(reduceMotion: true);
        Assert.Equal(1, EdgeGlow.Opacity(still, TimeSpan.Zero));
        Assert.Equal(1, EdgeGlow.Opacity(still, TimeSpan.FromMilliseconds(499)));
        Assert.Null(EdgeGlow.Opacity(still, TimeSpan.FromMilliseconds(500)));
    }

    [Fact]
    public void ShowsNothingWhileAGameIsFullScreenOrTheUserPresents()
    {
        // QUNS_RUNNING_D3D_FULL_SCREEN, QUNS_PRESENTATION_MODE.
        Assert.True(EdgeGlow.HoldsBack(3));
        Assert.True(EdgeGlow.HoldsBack(4));
        // Any other full-screen window (QUNS_BUSY), not present, accepting
        // notifications, quiet time, an app: shows.
        Assert.False(EdgeGlow.HoldsBack(2));
        Assert.False(EdgeGlow.HoldsBack(1));
        Assert.False(EdgeGlow.HoldsBack(5));
        Assert.False(EdgeGlow.HoldsBack(6));
        Assert.False(EdgeGlow.HoldsBack(7));
        Assert.False(EdgeGlow.HoldsBack(null));
    }
}
