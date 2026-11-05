namespace InputWeave.Overlay;

/// <summary>
/// One of this computer's screens as the background service reports it in
/// the arrangement: its rectangle in the service's logical desktop
/// coordinates, its name (<c>\\.\DISPLAY1</c>) and its scale.
/// </summary>
public sealed record LogicalScreen(string Id, double X, double Y, double Width, double Height, double Scale);

/// <summary>A monitor as Windows reports it to a per-monitor DPI aware app: physical pixels, right and bottom exclusive.</summary>
public sealed record MonitorPixels(string Id, int Left, int Top, int Right, int Bottom, double Scale, bool Primary);

public readonly record struct PixelPoint(int X, int Y);

public readonly record struct PixelRect(int X, int Y, int Width, int Height);

/// <summary>
/// A cursor as Windows shows it: its size in pixels and its hotspot,
/// measured from its top left in those pixels. The hotspot may fall
/// between pixels when it is taken from a larger copy.
/// </summary>
public readonly record struct CursorShape(double Width, double Height, double HotX, double HotY);

/// <summary>
/// The parts of the cursor locator that
/// need no screen: how the cursor grows and shrinks, and where its
/// enlarged copy goes so that its hotspot stays on the cursor. As the
/// macOS app's LocatorGeometry.swift, except that Windows places windows
/// in physical pixels, y down.
/// </summary>
public static class LocatorGeometry
{
    /// <summary>How the cursor is shown; times in seconds.</summary>
    /// <param name="Peak">How many times its size the cursor grows to.</param>
    /// <param name="Grow">How long it takes to grow; 0 is at its peak at once.</param>
    /// <param name="Hold">How long it then stays at its peak.</param>
    /// <param name="Shrink">How long it then takes to shrink back to its own size; 0 is gone at once.</param>
    public sealed record Motion(double Peak, double Grow, double Hold, double Shrink)
    {
        /// <summary>How long it is on screen in all.</summary>
        public double Shown => Grow + Hold + Shrink;
    }

    /// <summary>
    /// The cursor grows to four times its size, stays a moment and shrinks
    /// back, in 1.1 seconds. With animations turned off it is three times
    /// its size for a second and is gone.
    /// </summary>
    public static Motion For(bool reduceMotion) =>
        reduceMotion ? new Motion(3, 0, 1, 0) : new Motion(4, 0.15, 0.6, 0.35);

    /// <summary>
    /// How many times its size the cursor is <paramref name="seconds"/>
    /// after it began to grow: easing out as it grows, in and out as it
    /// shrinks, and its own size once it is done.
    /// </summary>
    public static double Scale(Motion motion, double seconds)
    {
        double shrinking = motion.Grow + motion.Hold;
        if (seconds < motion.Grow)
        {
            double t = Math.Max(seconds, 0) / motion.Grow;
            return 1 + (motion.Peak - 1) * (1 - Math.Pow(1 - t, 3));
        }
        if (seconds < shrinking)
        {
            return motion.Peak;
        }
        if (seconds < motion.Shown)
        {
            double t = (seconds - shrinking) / motion.Shrink;
            double eased = t < 0.5 ? 4 * t * t * t : 1 - Math.Pow(-2 * t + 2, 3) / 2;
            return motion.Peak - (motion.Peak - 1) * eased;
        }
        return 1;
    }

    /// <summary>
    /// Where a copy of the cursor <paramref name="scale"/> times its size
    /// goes, in pixels: its hotspot on <paramref name="cursor"/>.
    /// </summary>
    public static (double X, double Y, double Width, double Height) Frame(PixelPoint cursor, CursorShape shape, double scale) =>
        (cursor.X - shape.HotX * scale, cursor.Y - shape.HotY * scale, shape.Width * scale, shape.Height * scale);

    /// <summary>
    /// The window the copy is drawn in: the copy at its peak, in whole
    /// pixels. It only moves with the cursor; the copy shrinks inside it
    /// towards its hotspot.
    /// </summary>
    public static PixelRect Window(PixelPoint cursor, CursorShape shape, Motion motion)
    {
        var (x, y, width, height) = Frame(cursor, shape, motion.Peak);
        int left = (int)Math.Floor(x + 1e-6);
        int top = (int)Math.Floor(y + 1e-6);
        return new PixelRect(
            left, top,
            Math.Max(1, (int)Math.Ceiling(x + width - 1e-6) - left),
            Math.Max(1, (int)Math.Ceiling(y + height - 1e-6) - top));
    }

    /// <summary>
    /// The hotspot, along one axis, of a cursor of which a larger copy was
    /// made: the copy's own, measured in the cursor's pixels, when it is
    /// within a pixel of the cursor's, as when the copy was loaded at its
    /// size from the cursor's file, which places its hotspot for that size;
    /// otherwise the cursor's, as when Windows stretched the copy without
    /// moving the hotspot along.
    /// </summary>
    public static double Hotspot(double copy, double cursor) =>
        Math.Abs(copy - cursor) <= 1 ? copy : cursor;
}
