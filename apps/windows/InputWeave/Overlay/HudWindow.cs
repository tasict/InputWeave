using InputWeave.Support;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Windows.Graphics;

namespace InputWeave.Overlay;

/// <summary>
/// A brief note at the top of the screen the cursor is on:
/// why the keyboard and mouse came back,
/// the cursor being locked or unlocked, the take-back hint. It shows for
/// 2.5 seconds, longer while the pointer rests on it, then fades. One at a
/// time; a new note replaces the shown one. <see cref="HudWatch"/> decides
/// what it says.
/// </summary>
internal sealed partial class HudWindow : OverlayWindow
{
    private static readonly TimeSpan ShownFor = TimeSpan.FromMilliseconds(2500);
    private static readonly TimeSpan FadeFor = TimeSpan.FromMilliseconds(300);
    /// <summary>Space between the note and the top of the screen's work area, in view units.</summary>
    private const double TopMargin = 12;

    private readonly Grid root = new();
    private readonly Border plate;
    private readonly TextBlock text = new()
    {
        TextWrapping = TextWrapping.Wrap,
        TextAlignment = TextAlignment.Center,
        MaxWidth = 420,
    };
    private readonly DispatcherQueueTimer timer;
    private DateTime hideAt;

    public HudWindow()
    {
        text.Style = (Style)Application.Current.Resources["BodyTextBlockStyle"];
        plate = new Border
        {
            Child = text,
            Padding = new Thickness(16, 10, 16, 10),
            CornerRadius = new CornerRadius(8),
            Background = Brand.System("SolidBackgroundFillColorTertiaryBrush"),
            BorderBrush = Brand.System("SurfaceStrokeColorFlyoutBrush"),
            BorderThickness = new Thickness(1),
            HorizontalAlignment = HorizontalAlignment.Left,
            VerticalAlignment = VerticalAlignment.Top,
        };
        root.Children.Add(plate);
        Content = root;
        timer = DispatcherQueue.GetForCurrentThread().CreateTimer();
        timer.Interval = TimeSpan.FromMilliseconds(30);
        timer.Tick += (_, _) => Tick();
    }

    /// <summary>Shows <paramref name="message"/> at the top of the screen the cursor is on.</summary>
    /// <param name="shownFor">How long before it fades; longer for screenshots.</param>
    public void Say(string message, TimeSpan? shownFor = null)
    {
        text.Text = message;
        var (cx, cy) = Native.CursorPosition();
        var cursor = new PointInt32(cx, cy);
        var work = DisplayArea.GetFromPoint(cursor, DisplayAreaFallback.Primary).WorkArea;
        double scale = ScaleAt(cursor);
        plate.Measure(new Windows.Foundation.Size(double.PositiveInfinity, double.PositiveInfinity));
        int w = (int)Math.Ceiling(plate.DesiredSize.Width * scale);
        int h = (int)Math.Ceiling(plate.DesiredSize.Height * scale);
        root.Opacity = 1;
        ShowAt(new PixelRect(work.X + (work.Width - w) / 2, work.Y + (int)Math.Round(TopMargin * scale), w, h));
        Announce(text, message, "InputWeave.Hud");
        hideAt = DateTime.UtcNow + (shownFor ?? ShownFor);
        timer.Start();
        AppLog.Info("HUD shown");
    }

    private void Tick()
    {
        var now = DateTime.UtcNow;
        if (PointerOver())
        {
            // Read at leisure; the time starts again once the pointer leaves.
            hideAt = now + ShownFor;
            root.Opacity = 1;
            return;
        }
        if (now < hideAt)
        {
            return;
        }
        double faded = ReduceMotion ? 1 : (now - hideAt) / FadeFor;
        if (faded >= 1)
        {
            timer.Stop();
            HideNow();
            return;
        }
        root.Opacity = 1 - faded;
    }
}
