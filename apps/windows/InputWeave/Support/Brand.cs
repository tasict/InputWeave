using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Media;
using Windows.UI.ViewManagement;

namespace InputWeave.Support;

/// <summary>The five brand colors for an element's theme, from App.xaml.</summary>
internal sealed record Brand(Brush Thread, Brush Canvas, Brush Grid, Brush Tile, Brush TileEdge, bool HighContrast, bool Dark)
{
    private static readonly AccessibilitySettings Accessibility = new();

    public static Brand For(FrameworkElement element)
    {
        bool highContrast = Accessibility.HighContrast;
        bool dark = element.ActualTheme == ElementTheme.Dark;
        var theme = highContrast ? "HighContrast" : dark ? "Dark" : "Light";
        var colors = (ResourceDictionary)Application.Current.Resources.ThemeDictionaries[theme];
        return new Brand(
            (Brush)colors["BrandThreadBrush"],
            (Brush)colors["BrandCanvasBrush"],
            (Brush)colors["BrandGridBrush"],
            (Brush)colors["BrandTileBrush"],
            (Brush)colors["BrandTileEdgeBrush"],
            highContrast,
            dark);
    }

    /// <summary>Whether Windows' high contrast is on, as last read; the settings object is kept, not made anew.</summary>
    public static bool HighContrastOn => Accessibility.HighContrast;

    /// <summary>A system brush from the app's resources, e.g. "TextFillColorSecondaryBrush".</summary>
    public static Brush System(string key) => (Brush)Application.Current.Resources[key];
}
