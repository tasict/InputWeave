using System.Diagnostics;
using System.Runtime.InteropServices.WindowsRuntime;
using InputWeave.Support;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Graphics;

namespace InputWeave.Overlay;

/// <summary>
/// Shows where the cursor is after the user shook the mouse with the
/// cursor on this computer: the cursor
/// seems to grow to several times its size and shrink back, in about a
/// second. What grows is a copy of the cursor shown now, in a window that
/// follows the cursor every frame with the copy's hotspot on it; the
/// cursor itself is never changed, so nothing stays changed should the app
/// end. It takes neither the focus nor clicks and makes no sound. One at
/// a time; a new one starts over.
/// </summary>
/// <remarks>
/// The window is the copy at its peak and only moves; the copy shrinks
/// inside it towards its hotspot through a render transform. Resizing a
/// WinUI window lays it out and redraws its swap chain, which shows as
/// flicker when done every frame; moving it is left to the compositor.
/// </remarks>
internal sealed partial class LocatorWindow : OverlayWindow
{
    private readonly Grid root = new();
    private readonly Image image = new()
    {
        Stretch = Stretch.Fill,
        HorizontalAlignment = HorizontalAlignment.Left,
        VerticalAlignment = VerticalAlignment.Top,
        UseLayoutRounding = false,
    };
    private readonly CompositeTransform zoom = new();
    private LocatorGeometry.Motion motion = LocatorGeometry.For(false);
    private CursorPicture? picture;
    /// <summary>The cursor last read, so one that cannot be is not read again every frame.</summary>
    private nint tried;
    private PixelPoint cursor;
    private long start;
    /// <summary>For screenshots: the moment to hold the copy at, and until when.</summary>
    private (double At, long Until)? held;
    /// <summary>The pixels per view unit the image was last sized for; 0 to size it again.</summary>
    private double sizedFor;
    private bool ticking;
    private int frames;

    public LocatorWindow()
    {
        image.RenderTransform = zoom;
        root.Children.Add(image);
        AutomationProperties.SetAccessibilityView(root, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
        Content = root;
    }

    /// <summary>Shows the cursor growing where it is now.</summary>
    /// <param name="holdAt">For screenshots: holds it as it is this many seconds in, for twenty seconds, following the cursor still.</param>
    public void Show(double? holdAt = null)
    {
        motion = LocatorGeometry.For(ReduceMotion);
        tried = 0;
        if (!Take(CursorPicture.Shown()) || picture is null)
        {
            AppLog.Warn("cannot read the cursor to show where it is");
            Dismiss();
            return;
        }
        start = Stopwatch.GetTimestamp();
        held = holdAt is { } at ? (Math.Clamp(at, 0, motion.Shown * 0.99), start + 20 * Stopwatch.Frequency) : null;
        frames = 0;
        var (x, y) = Native.CursorPosition();
        cursor = new PixelPoint(x, y);
        Draw(Seconds(), root.XamlRoot?.RasterizationScale ?? ScaleAt(new PointInt32(x, y)));
        ShowAt(LocatorGeometry.Window(cursor, picture.Shape, motion));
        if (!ticking)
        {
            // Once a frame, as the screen is drawn, rather than on a timer
            // that would step unevenly against it.
            CompositionTarget.Rendering += OnRendering;
            ticking = true;
        }
        AppLog.Info($"cursor shown at {motion.Peak} times its size ({picture.Width}x{picture.Height}, {picture.Source})");
    }

    /// <summary>Gone at once, as when the app quits.</summary>
    public void Dismiss()
    {
        if (ticking)
        {
            CompositionTarget.Rendering -= OnRendering;
            ticking = false;
        }
        HideNow();
    }

    private void OnRendering(object? sender, object e) => Tick();

    private void Tick()
    {
        double seconds = Seconds();
        if (seconds >= motion.Shown || picture is null)
        {
            AppLog.Info($"cursor locator done after {frames} frames");
            Dismiss();
            return;
        }
        // The cursor can change on the way, as over text.
        var shown = CursorPicture.Shown();
        if (shown != picture.Cursor && shown != tried)
        {
            Take(shown);
        }
        if (Native.GetCursorPos(out var point))
        {
            cursor = new PixelPoint(point.X, point.Y);
        }
        MoveTo(LocatorGeometry.Window(cursor, picture.Shape, motion));
        Draw(seconds, root.XamlRoot?.RasterizationScale ?? sizedFor);
        frames++;
    }

    /// <summary>The copy of <paramref name="shown"/> to grow; false, keeping the one there was, when it cannot be read.</summary>
    private bool Take(nint shown)
    {
        tried = shown;
        if (CursorPicture.Of(shown, motion.Peak) is not { } taken)
        {
            return false;
        }
        var bitmap = new WriteableBitmap(taken.Width, taken.Height);
        taken.Pixels.CopyTo(bitmap.PixelBuffer);
        bitmap.Invalidate();
        image.Source = bitmap;
        picture = taken;
        sizedFor = 0;
        return true;
    }

    private double Seconds()
    {
        long now = Stopwatch.GetTimestamp();
        if (held is { } hold)
        {
            return now < hold.Until ? hold.At : motion.Shown;
        }
        return Stopwatch.GetElapsedTime(start, now).TotalSeconds;
    }

    /// <summary>
    /// The copy at its size for now, in the window placed for the cursor:
    /// the image is the copy at its peak, scaled from its top left and
    /// moved so its hotspot stays where the window put the cursor.
    /// </summary>
    /// <param name="pixelsPerUnit">The window's scale: physical pixels per view unit.</param>
    private void Draw(double seconds, double pixelsPerUnit)
    {
        if (picture is null || pixelsPerUnit <= 0)
        {
            return;
        }
        var shape = picture.Shape;
        if (pixelsPerUnit != sizedFor)
        {
            image.Width = shape.Width * motion.Peak / pixelsPerUnit;
            image.Height = shape.Height * motion.Peak / pixelsPerUnit;
            sizedFor = pixelsPerUnit;
        }
        double scale = LocatorGeometry.Scale(motion, seconds);
        var window = LocatorGeometry.Window(cursor, shape, motion);
        var frame = LocatorGeometry.Frame(cursor, shape, scale);
        zoom.ScaleX = zoom.ScaleY = scale / motion.Peak;
        zoom.TranslateX = (frame.X - window.X) / pixelsPerUnit;
        zoom.TranslateY = (frame.Y - window.Y) / pixelsPerUnit;
    }
}
