using InputWeave.Panel;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Animation;
using Microsoft.UI.Xaml.Shapes;
using Windows.UI.ViewManagement;

namespace InputWeave.Support;

/// <summary>
/// The "Licensed" label: a woven label sewn into a
/// seam, the screen tile's fill and edge with a ring of stitches and its
/// text in the thread color. With high contrast the stitches are a solid
/// line. Sewn: the stitches run round once as it appears, after a license
/// was activated here, unless Windows' animations are off.
/// </summary>
internal static class WovenLabel
{
    private static readonly UISettings Settings = new();

    /// <summary>The sidebar's size: the top of the 18–20 the spec gives, so the text clears the stitches.</summary>
    public const double Small = 20;

    /// <param name="height"><see cref="Small"/> for the sidebar, 24 in About, 44 on the License page.</param>
    public static FrameworkElement Create(double height, bool sew = false)
    {
        var colors = (ResourceDictionary)Application.Current.Resources.ThemeDictionaries[
            Brand.HighContrastOn ? "HighContrast" : Application.Current.RequestedTheme == ApplicationTheme.Dark ? "Dark" : "Light"];
        var thread = (Brush)colors["BrandThreadBrush"];
        double inset = height >= 40 ? 4 : height >= 24 ? 3 : 2;
        double fontSize = height >= 40 ? 17 : height >= 24 ? 12 : 10;
        var stitches = new Rectangle
        {
            Stroke = thread,
            StrokeThickness = Brand.HighContrastOn ? 2 : height >= 40 ? 1.4 : 1,
            RadiusX = 1.5,
            RadiusY = 1.5,
            Margin = new Thickness(inset),
            IsHitTestVisible = false,
        };
        if (!Brand.HighContrastOn)
        {
            stitches.StrokeDashArray = height >= 40 ? [3, 2.2] : [2.5, 2];
        }
        var text = new TextBlock
        {
            Text = Strings.Get(TextKeys.LicenseLicensed),
            Foreground = thread,
            FontWeight = Brand.HighContrastOn ? FontWeights.Bold : FontWeights.SemiBold,
            FontSize = fontSize,
            VerticalAlignment = VerticalAlignment.Center,
            HorizontalAlignment = HorizontalAlignment.Center,
            // Clear of the stitches on every side: at least 5 px across, 2 above and below.
            Margin = new Thickness(Math.Max(height * 0.45, inset + 5), 0, Math.Max(height * 0.45, inset + 5), 0),
        };
        var label = new Grid
        {
            Height = height,
            Background = (Brush)colors["BrandTileBrush"],
            BorderBrush = (Brush)colors["BrandTileEdgeBrush"],
            BorderThickness = new Thickness(1),
            CornerRadius = new CornerRadius(height >= 40 ? 5 : 3),
            HorizontalAlignment = HorizontalAlignment.Left,
            VerticalAlignment = VerticalAlignment.Center,
        };
        label.Children.Add(stitches);
        label.Children.Add(text);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(label, text.Text);
        if (sew && Settings.AnimationsEnabled && !Brand.HighContrastOn)
        {
            label.Loaded += (_, _) => Sew(stitches);
        }
        return label;
    }

    /// <summary>The stitches run round the label once, half a second.</summary>
    private static void Sew(Rectangle stitches)
    {
        var run = new DoubleAnimation
        {
            From = 40,
            To = 0,
            Duration = new Duration(TimeSpan.FromMilliseconds(500)),
            EasingFunction = new CubicEase { EasingMode = EasingMode.EaseOut },
            EnableDependentAnimation = true,
        };
        var fade = new DoubleAnimation
        {
            From = 0,
            To = 1,
            Duration = new Duration(TimeSpan.FromMilliseconds(500)),
        };
        Storyboard.SetTarget(run, stitches);
        Storyboard.SetTargetProperty(run, nameof(Rectangle.StrokeDashOffset));
        Storyboard.SetTarget(fade, stitches);
        Storyboard.SetTargetProperty(fade, nameof(UIElement.Opacity));
        var board = new Storyboard();
        board.Children.Add(run);
        board.Children.Add(fade);
        board.Begin();
    }

    /// <summary>The neutral "Trial" pill beside the sidebar's License item and in About.</summary>
    public static FrameworkElement TrialPill(double fontSize = 11) => new Border
    {
        Background = Brand.System("ControlFillColorSecondaryBrush"),
        BorderBrush = Brand.System("ControlStrokeColorDefaultBrush"),
        BorderThickness = new Thickness(1),
        CornerRadius = new CornerRadius(9),
        Padding = new Thickness(7, 0, 7, 1),
        VerticalAlignment = VerticalAlignment.Center,
        HorizontalAlignment = HorizontalAlignment.Left,
        Child = new TextBlock
        {
            Text = Strings.Get(TextKeys.LicenseTrial),
            FontSize = fontSize,
            Foreground = Brand.System("TextFillColorSecondaryBrush"),
        },
    };
}
