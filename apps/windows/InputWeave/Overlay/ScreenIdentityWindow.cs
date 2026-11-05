using InputWeave.Support;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace InputWeave.Overlay;

/// <summary>
/// This computer's name over one of its monitors for a moment, when the
/// screens are identified here or from another computer:
/// large in the middle, with the screen's
/// number and size beneath, and the shortcut that jumps here when there is
/// one, inside a border in the thread color along the monitor's edges. It
/// covers the whole monitor, takes neither the focus nor clicks, and goes
/// away by itself; identified again, it shows the new one instead. The app
/// keeps one per monitor and reuses them.
/// </summary>
internal sealed partial class ScreenIdentityWindow : OverlayWindow
{
    private static readonly TimeSpan FadeFor = TimeSpan.FromMilliseconds(300);
    /// <summary>Room between the card and the monitor's edges, in view units.</summary>
    private const double CardMargin = 40;

    private readonly Grid root = new();
    private readonly Border edge = new() { BorderThickness = new Thickness(6), IsHitTestVisible = false };
    private readonly Border card;
    private readonly TextBlock name = new()
    {
        FontSize = 56,
        FontWeight = FontWeights.SemiBold,
        MaxLines = 1,
        TextAlignment = TextAlignment.Center,
    };
    private readonly Viewbox nameBox;
    private readonly TextBlock label = new()
    {
        FontSize = 20,
        HorizontalAlignment = HorizontalAlignment.Center,
        TextAlignment = TextAlignment.Center,
    };
    private readonly TextBlock jump = new()
    {
        FontSize = 14,
        HorizontalAlignment = HorizontalAlignment.Center,
        TextAlignment = TextAlignment.Center,
    };
    private readonly DispatcherQueueTimer timer;
    private DateTime hideAt;

    public ScreenIdentityWindow()
    {
        label.Foreground = Brand.System("TextFillColorSecondaryBrush");
        jump.Foreground = Brand.System("TextFillColorSecondaryBrush");
        // Shrinks a long name to fit, rather than cutting it.
        nameBox = new Viewbox { Child = name, StretchDirection = StretchDirection.DownOnly, HorizontalAlignment = HorizontalAlignment.Center };
        var words = new StackPanel { Spacing = 6, HorizontalAlignment = HorizontalAlignment.Center };
        words.Children.Add(nameBox);
        words.Children.Add(label);
        words.Children.Add(jump);
        card = new Border
        {
            Child = words,
            Padding = new Thickness(40, 24, 40, 24),
            Margin = new Thickness(CardMargin),
            CornerRadius = new CornerRadius(20),
            BorderThickness = new Thickness(3),
            Background = Brand.System("SolidBackgroundFillColorTertiaryBrush"),
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
        };
        root.Children.Add(edge);
        root.Children.Add(card);
        AutomationProperties.SetAccessibilityView(edge, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
        Content = root;
        timer = DispatcherQueue.GetForCurrentThread().CreateTimer();
        timer.Interval = TimeSpan.FromMilliseconds(30);
        timer.Tick += (_, _) => Tick();
    }

    /// <summary>
    /// Shows <paramref name="computer"/> over the monitor at
    /// <paramref name="monitor"/>, in pixels, with the screen's
    /// <paramref name="screen"/> line and the <paramref name="shortcut"/>
    /// line when there is one, for <paramref name="shownFor"/>. Narrator
    /// reads the name when <paramref name="announce"/>, once for all
    /// monitors.
    /// </summary>
    public void Show(string computer, string screen, string? shortcut, PixelRect monitor, double scale, TimeSpan shownFor, bool announce)
    {
        var thread = Brand.For(root).Thread;
        edge.BorderBrush = thread;
        card.BorderBrush = thread;
        name.Text = computer;
        label.Text = screen;
        jump.Text = shortcut ?? "";
        jump.Visibility = shortcut is null ? Visibility.Collapsed : Visibility.Visible;
        // The card's width in view units on this monitor, less its margin and padding.
        nameBox.MaxWidth = Math.Max(monitor.Width / (scale > 0 ? scale : 1) - 2 * CardMargin - 80, 100);
        root.Opacity = 1;
        // A pixel short of the bottom: a window above all others that covers
        // a whole monitor counts as an app in full screen, for which Windows
        // turns on Do Not Disturb and gives up the taskbar.
        var frame = monitor with { Height = Math.Max(monitor.Height - 1, 1) };
        ShowAt(frame);
        // Shown on a monitor of another scale, Windows may have resized it on the way.
        ShowAt(frame);
        if (announce)
        {
            Announce(name, computer, "InputWeave.IdentifyScreens");
        }
        hideAt = DateTime.UtcNow + shownFor;
        timer.Start();
    }

    private void Tick()
    {
        var now = DateTime.UtcNow;
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

    /// <summary>Gone at once, as when the app quits.</summary>
    public void Dismiss()
    {
        timer.Stop();
        HideNow();
    }
}
