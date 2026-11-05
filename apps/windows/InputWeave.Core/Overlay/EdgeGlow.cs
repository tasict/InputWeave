using Inputweave.App.V1;

namespace InputWeave.Overlay;

/// <summary>
/// The parts of lighting up the edge where the cursor crosses to or from
/// another computer that need no window: which
/// monitor, the whole edge crossed, how bright along it, for how long, and
/// when not at all. As the macOS app's EdgeGlowGeometry.swift, in pixels
/// and with y going down.
/// </summary>
public static class EdgeGlow
{
    /// <summary>Fading inward over about 24 view units.</summary>
    public const double Depth = 24;

    /// <summary>With high contrast, a solid line this thick instead, no fade.</summary>
    public const double SolidDepth = 4;

    /// <summary>How bright the glow stays along the edge away from where it crossed, at the least, of its brightest.</summary>
    public const double Dimmest = 0.35;

    /// <summary>How wide the bright part is, as a share of the edge's length (the standard deviation of its bell curve).</summary>
    public const double Spread = 0.25;

    /// <summary>How opaque the glow is at the screen's edge; it fades to nothing inward.</summary>
    public const double Strongest = 0.95;

    /// <summary>How long the brightest part takes to slide to a new crossing of an edge still lit.</summary>
    public static readonly TimeSpan Slide = TimeSpan.FromMilliseconds(150);

    public sealed record Timing(TimeSpan FadeIn, TimeSpan Hold, TimeSpan FadeOut)
    {
        public TimeSpan Total => FadeIn + Hold + FadeOut;
    }

    /// <summary>In 0.1 seconds, held for 0.3, out in 0.4.</summary>
    public static readonly Timing Animated = new(TimeSpan.FromMilliseconds(100), TimeSpan.FromMilliseconds(300), TimeSpan.FromMilliseconds(400));

    /// <summary>With animation effects off: there at once for half a second, then gone.</summary>
    public static readonly Timing Still = new(TimeSpan.Zero, TimeSpan.FromMilliseconds(500), TimeSpan.Zero);

    public static Timing For(bool reduceMotion) => reduceMotion ? Still : Animated;

    /// <summary>How opaque the glow is <paramref name="elapsed"/> after it lit up; null once it is gone.</summary>
    public static double? Opacity(Timing timing, TimeSpan elapsed)
    {
        if (elapsed < TimeSpan.Zero)
        {
            return 0;
        }
        if (elapsed < timing.FadeIn)
        {
            return elapsed / timing.FadeIn;
        }
        var fading = elapsed - timing.FadeIn - timing.Hold;
        if (fading < TimeSpan.Zero)
        {
            return 1;
        }
        return fading < timing.FadeOut ? 1 - fading / timing.FadeOut : null;
    }

    /// <summary>
    /// How far along its edge the crossing lies, from 0 at the top of a
    /// left or right edge, or the left end of a top or bottom one, to 1 at
    /// the other end.
    /// </summary>
    public static double Fraction(CursorCrossed crossed)
    {
        var (start, extent) = crossed.Edge is ScreenEdge.Left or ScreenEdge.Right
            ? (crossed.DisplayY, crossed.DisplayHeight)
            : (crossed.DisplayX, crossed.DisplayWidth);
        if (extent <= 0 || !double.IsFinite(crossed.Along))
        {
            return 0.5;
        }
        return Math.Clamp((crossed.Along - start) / extent, 0, 1);
    }

    /// <summary>
    /// Whether <paramref name="edge"/> is one of the four a glow can light;
    /// anything else (unspecified, or a value from a newer background
    /// service) lights nothing.
    /// </summary>
    public static bool Known(ScreenEdge edge) =>
        edge is ScreenEdge.Left or ScreenEdge.Right or ScreenEdge.Top or ScreenEdge.Bottom;

    /// <summary>
    /// The monitor a display is: the one of its name (<c>\\.\DISPLAY1</c>),
    /// as the background service names them; else the one at its place
    /// and size, the monitor's pixels ÷ its scale being the service's
    /// logical units where all monitors have one scale (with mixed scales
    /// the names match); else the only one of its size; else none.
    /// </summary>
    public static MonitorPixels? MonitorFor(CursorCrossed crossed, IReadOnlyList<MonitorPixels> monitors)
    {
        if (monitors.FirstOrDefault(m => string.Equals(m.Id, crossed.DisplayId, StringComparison.OrdinalIgnoreCase)) is { } named)
        {
            return named;
        }
        static bool Close(double a, double b) => Math.Abs(a - b) < 1;
        bool SameSize(MonitorPixels m)
        {
            var scale = m.Scale > 0 ? m.Scale : 1;
            return Close((m.Right - m.Left) / scale, crossed.DisplayWidth) && Close((m.Bottom - m.Top) / scale, crossed.DisplayHeight);
        }
        if (monitors.FirstOrDefault(m =>
            {
                var scale = m.Scale > 0 ? m.Scale : 1;
                return SameSize(m) && Close(m.Left / scale, crossed.DisplayX) && Close(m.Top / scale, crossed.DisplayY);
            }) is { } placed)
        {
            return placed;
        }
        var sameSize = monitors.Where(SameSize).ToList();
        return sameSize.Count == 1 ? sameSize[0] : null;
    }

    /// <summary>
    /// The glow's frame on <paramref name="monitor"/>, in pixels: the whole
    /// of <paramref name="edge"/>, <paramref name="thickness"/> view units
    /// deep from it at the monitor's scale.
    /// </summary>
    public static PixelRect Strip(MonitorPixels monitor, ScreenEdge edge, double thickness)
    {
        var scale = monitor.Scale > 0 ? monitor.Scale : 1;
        int width = monitor.Right - monitor.Left;
        int height = monitor.Bottom - monitor.Top;
        int deep = Math.Clamp((int)Math.Round(thickness * scale), 1, Math.Max(Math.Min(width, height), 1));
        return edge switch
        {
            ScreenEdge.Left => new PixelRect(monitor.Left, monitor.Top, deep, height),
            ScreenEdge.Right => new PixelRect(monitor.Right - deep, monitor.Top, deep, height),
            ScreenEdge.Top => new PixelRect(monitor.Left, monitor.Top, width, deep),
            _ => new PixelRect(monitor.Left, monitor.Bottom - deep, width, deep),
        };
    }

    /// <summary>
    /// How bright the glow is at <paramref name="position"/> along its edge
    /// (0 to 1, as <see cref="Fraction"/> measures) when the cursor crossed
    /// at <paramref name="spot"/>: all of it there, falling off smoothly
    /// toward both ends but never below <see cref="Dimmest"/>, so the whole
    /// edge shows; about two fifths at the ends when it crossed in the
    /// middle.
    /// </summary>
    public static double Brightness(double position, double spot)
    {
        var away = (position - spot) / Spread;
        return Dimmest + (1 - Dimmest) * Math.Exp(-away * away / 2);
    }

    /// <summary>
    /// Where the brightest part is <paramref name="elapsed"/> after it set
    /// off from <paramref name="from"/> to a new crossing at
    /// <paramref name="to"/>: easing out, there after <see cref="Slide"/>.
    /// </summary>
    public static double SpotAt(double from, double to, TimeSpan elapsed)
    {
        if (elapsed >= Slide)
        {
            return to;
        }
        var t = Math.Max(elapsed / Slide, 0);
        var eased = 1 - Math.Pow(1 - t, 3);
        return from + (to - from) * eased;
    }

    /// <summary>
    /// The glow of <paramref name="edge"/> as premultiplied BGRA pixels in
    /// <paramref name="color"/>, <paramref name="along"/> pixels along the
    /// edge and <paramref name="deep"/> across it (so
    /// <paramref name="deep"/> wide and <paramref name="along"/> high for a
    /// left or right edge), to be stretched over its window: strongest at
    /// the screen's edge and fading to nothing inward, as bright along it
    /// as <see cref="Brightness"/> says for <paramref name="spot"/>.
    /// </summary>
    public static byte[] Pixels(ScreenEdge edge, int along, int deep, double spot, (byte R, byte G, byte B) color)
    {
        var pixels = new byte[along * deep * 4];
        Paint(pixels, edge, along, deep, spot, color);
        return pixels;
    }

    /// <summary>As <see cref="Pixels"/>, into <paramref name="pixels"/>, which holds <paramref name="along"/> × <paramref name="deep"/> of them.</summary>
    public static void Paint(Span<byte> pixels, ScreenEdge edge, int along, int deep, double spot, (byte R, byte G, byte B) color)
    {
        bool upright = edge is ScreenEdge.Left or ScreenEdge.Right;
        int width = upright ? deep : along;
        for (int a = 0; a < along; a++)
        {
            var bright = Brightness((a + 0.5) / along, spot);
            for (int d = 0; d < deep; d++)
            {
                // d counts inward from the screen's edge.
                var alpha = Strongest * bright * (1 - (d + 0.5) / deep);
                var (x, y) = edge switch
                {
                    ScreenEdge.Left => (d, a),
                    ScreenEdge.Right => (deep - 1 - d, a),
                    ScreenEdge.Top => (a, d),
                    _ => (a, deep - 1 - d),
                };
                int i = (y * width + x) * 4;
                pixels[i] = (byte)Math.Round(color.B * alpha);
                pixels[i + 1] = (byte)Math.Round(color.G * alpha);
                pixels[i + 2] = (byte)Math.Round(color.R * alpha);
                pixels[i + 3] = (byte)Math.Round(255 * alpha);
            }
        }
    }

    /// <summary>
    /// When a crossing of an edge at <paramref name="now"/> lights its glow
    /// from, the glow of that same edge having lit at
    /// <paramref name="previous"/>, if it did. While that is still to be
    /// seen (coming up, held or fading), it carries on from how bright it
    /// is: up to full again if it was fading, held anew, never dark in
    /// between, so crossing back and forth does not flicker. Otherwise it
    /// lights up anew, from now.
    /// </summary>
    public static DateTime LitAt(DateTime? previous, DateTime now, Timing timing) =>
        previous is { } at && Opacity(timing, now - at) is { } opacity ? now - timing.FadeIn * opacity : now;

    /// <summary>
    /// Whether to show nothing over the screen now, as Windows reports what
    /// the user is doing (<c>SHQueryUserNotificationState</c>): a
    /// full-screen Direct3D app such as a game
    /// (<c>QUNS_RUNNING_D3D_FULL_SCREEN</c>, 3), or presenting (4). Any
    /// other full-screen window (<c>QUNS_BUSY</c>, 2: a video in a browser,
    /// a borderless app) still has it, as over full-screen apps on a Mac.
    /// Not known, it shows.
    /// </summary>
    public static bool HoldsBack(int? notificationState) => notificationState is 3 or 4;
}
