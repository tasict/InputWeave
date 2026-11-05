using System.Runtime.InteropServices.WindowsRuntime;
using Inputweave.App.V1;
using InputWeave.Support;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Microsoft.UI.Xaml.Shapes;

namespace InputWeave.Overlay;

/// <summary>
/// The edge of a monitor lighting up where the cursor crossed to or from
/// another computer: the whole edge in the thread
/// color, strongest at the edge and fading inward, brightest where it
/// crossed; with high contrast an even solid line instead. Only as large as
/// the glow, so nothing else of the screen is covered. Takes neither the
/// focus nor clicks, and goes away by itself. The app keeps one for each
/// edge of each monitor and reuses it; it alone knows when its glow lit.
/// </summary>
internal sealed partial class EdgeGlowWindow : OverlayWindow
{
    /// <summary>Pixels along the edge in the drawn glow, stretched over the window; its brightness changes slowly enough.</summary>
    private const int Along = 256;

    private readonly Grid root = new();
    private readonly Image image = new() { Stretch = Stretch.Fill, IsHitTestVisible = false };
    private readonly Rectangle line = new() { IsHitTestVisible = false };
    private readonly DispatcherQueueTimer timer;
    private EdgeGlow.Timing timing = EdgeGlow.Animated;
    /// <summary>When the glow lit, which its opacity counts from.</summary>
    private DateTime litAt;
    /// <summary>What the glow was last drawn for; drawn again only when it changes.</summary>
    private (ScreenEdge Edge, bool Solid, Windows.UI.Color Color, int Deep, double Spot)? drawn;
    /// <summary>The monitor and its scale it was last shown on.</summary>
    private (string Monitor, double Scale)? shownOn;
    private ScreenEdge edge;
    private bool solid;
    private int deep = 1;
    /// <summary>The thread color, as of the last crossing (light or dark, high contrast).</summary>
    private Brush thread = new SolidColorBrush(Microsoft.UI.Colors.MediumPurple);
    /// <summary>The glow drawn, reused while its size stays.</summary>
    private WriteableBitmap? bitmap;
    private byte[] pixels = [];
    /// <summary>The brightest part, sliding from <see cref="slideFrom"/> to <see cref="spot"/> since <see cref="slideSince"/>.</summary>
    private double spot = 0.5;
    private double slideFrom = 0.5;
    private DateTime slideSince;

    public EdgeGlowWindow()
    {
        root.Children.Add(image);
        root.Children.Add(line);
        AutomationProperties.SetAccessibilityView(image, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
        AutomationProperties.SetAccessibilityView(line, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
        Content = root;
        timer = DispatcherQueue.GetForCurrentThread().CreateTimer();
        timer.Interval = TimeSpan.FromMilliseconds(16);
        timer.Tick += (_, _) => Tick();
    }

    /// <summary>
    /// Lights <paramref name="edge"/> of <paramref name="monitor"/> at
    /// <paramref name="frame"/>, in pixels, brightest at
    /// <paramref name="at"/> along it, as a solid line when
    /// <paramref name="solid"/>. While it still shows it carries on from how
    /// bright it is (<see cref="EdgeGlow.LitAt"/>), and its brightest part
    /// slides to <paramref name="at"/> unless animations are off.
    /// </summary>
    public void Show(MonitorPixels monitor, PixelRect frame, ScreenEdge edge, bool solid, double at, EdgeGlow.Timing timing)
    {
        var now = DateTime.UtcNow;
        bool showing = timer.IsRunning && this.edge == edge;
        litAt = EdgeGlow.LitAt(showing ? litAt : null, now, timing);
        this.timing = timing;
        slideFrom = showing && !ReduceMotion ? EdgeGlow.SpotAt(slideFrom, spot, now - slideSince) : at;
        spot = at;
        slideSince = now;
        this.edge = edge;
        this.solid = solid;
        deep = Math.Max(edge is ScreenEdge.Left or ScreenEdge.Right ? frame.Width : frame.Height, 1);
        thread = Brand.For(root).Thread;
        Draw(slideFrom);
        root.Opacity = EdgeGlow.Opacity(timing, now - litAt) ?? 0;
        ShowAt(frame);
        if (shownOn != (monitor.Id, monitor.Scale))
        {
            // Moved to a monitor of another scale, Windows may have resized it on the way.
            shownOn = (monitor.Id, monitor.Scale);
            ShowAt(frame);
        }
        timer.Start();
    }

    /// <summary>Draws the glow brightest at <paramref name="at"/>, unless it is drawn so already.</summary>
    private void Draw(double at)
    {
        var color = thread is SolidColorBrush solidColor ? solidColor.Color : Microsoft.UI.Colors.MediumPurple;
        if (drawn == (edge, solid, color, deep, at))
        {
            return;
        }
        drawn = (edge, solid, color, deep, at);
        line.Visibility = solid ? Visibility.Visible : Visibility.Collapsed;
        image.Visibility = solid ? Visibility.Collapsed : Visibility.Visible;
        if (solid)
        {
            line.Fill = thread;
            return;
        }
        bool upright = edge is ScreenEdge.Left or ScreenEdge.Right;
        var (width, height) = upright ? (deep, Along) : (Along, deep);
        if (bitmap is null || bitmap.PixelWidth != width || bitmap.PixelHeight != height)
        {
            bitmap = new WriteableBitmap(width, height);
            pixels = new byte[width * height * 4];
            image.Source = bitmap;
        }
        EdgeGlow.Paint(pixels, edge, Along, deep, at, (color.R, color.G, color.B));
        pixels.CopyTo(bitmap.PixelBuffer);
        bitmap.Invalidate();
    }

    private void Tick()
    {
        var now = DateTime.UtcNow;
        if (EdgeGlow.Opacity(timing, now - litAt) is { } opacity)
        {
            root.Opacity = opacity;
            if (!solid)
            {
                Draw(EdgeGlow.SpotAt(slideFrom, spot, now - slideSince));
            }
            return;
        }
        timer.Stop();
        HideNow();
    }

    /// <summary>Gone at once, as when the app quits.</summary>
    public void Dismiss()
    {
        timer.Stop();
        HideNow();
    }
}
