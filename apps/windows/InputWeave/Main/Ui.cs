using InputWeave.Support;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace InputWeave.Main;

/// <summary>
/// The parts the pages are built from, in the manner of Windows' Settings:
/// cards of rows, each with its words on the left and its control on the
/// right, under a section header. System controls and colors only.
/// </summary>
internal static class Ui
{
    public static Style Style(string key) => (Style)Application.Current.Resources[key];

    /// <summary>A page that scrolls, its content no wider than reads well.</summary>
    public static ScrollViewer Page(Microsoft.UI.Xaml.Controls.Panel content)
    {
        content.MaxWidth = 1000;
        content.HorizontalAlignment = HorizontalAlignment.Stretch;
        return new ScrollViewer
        {
            Content = new Border { Child = content, Padding = new Thickness(36, 8, 36, 36) },
            HorizontalScrollMode = ScrollMode.Disabled,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
        };
    }

    public static TextBlock Header(string text) => new()
    {
        Text = text,
        Style = Style("BodyStrongTextBlockStyle"),
        Margin = new Thickness(1, 20, 0, 6),
    };

    /// <summary>A section header with a control at its right end, e.g. a button acting on the whole section.</summary>
    public static Grid Header(string text, FrameworkElement trailing)
    {
        var header = Header(text);
        header.Margin = new Thickness(1, 0, 0, 0);
        header.VerticalAlignment = VerticalAlignment.Center;
        var grid = new Grid { Margin = new Thickness(0, 14, 0, 6) };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.Children.Add(header);
        trailing.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(trailing, 1);
        grid.Children.Add(trailing);
        return grid;
    }

    public static TextBlock Secondary(string text, bool caption = true) => new()
    {
        Text = text,
        Style = Style(caption ? "CaptionTextBlockStyle" : "BodyTextBlockStyle"),
        Foreground = Brand.System("TextFillColorSecondaryBrush"),
        TextWrapping = TextWrapping.Wrap,
    };

    /// <summary>Rows in a card, with a line between them.</summary>
    public static Border Card(params UIElement[] rows)
    {
        var stack = new StackPanel();
        for (int i = 0; i < rows.Length; i++)
        {
            if (i > 0)
            {
                stack.Children.Add(Divider());
            }
            stack.Children.Add(rows[i]);
        }
        return Card(stack);
    }

    /// <summary>A card around <paramref name="content"/>; separators added later go between rows.</summary>
    public static Border Card(StackPanel content) => new()
    {
        Child = content,
        Background = Brand.System("CardBackgroundFillColorDefaultBrush"),
        BorderBrush = Brand.System("CardStrokeColorDefaultBrush"),
        BorderThickness = new Thickness(1),
        CornerRadius = new CornerRadius(6),
    };

    /// <summary>A line between a card's rows.</summary>
    public static Microsoft.UI.Xaml.Shapes.Rectangle Divider() => new()
    {
        Height = 1,
        Fill = Brand.System("DividerStrokeColorDefaultBrush"),
    };

    /// <summary>A row: a title and an optional description on the left, a control on the right.</summary>
    public static Grid Row(string title, string? description = null, UIElement? control = null, FontIcon? icon = null)
    {
        var words = new StackPanel { VerticalAlignment = VerticalAlignment.Center, Spacing = 2 };
        var titleText = new TextBlock { Text = title, TextWrapping = TextWrapping.Wrap };
        words.Children.Add(titleText);
        if (!string.IsNullOrEmpty(description))
        {
            words.Children.Add(Secondary(description));
        }
        return Row(words, control, icon);
    }

    public static Grid Row(UIElement words, UIElement? control = null, FontIcon? icon = null)
    {
        var row = new Grid { Padding = new Thickness(16, 12, 16, 12), MinHeight = 56 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        if (icon is not null)
        {
            icon.VerticalAlignment = VerticalAlignment.Center;
            icon.FontSize = 18;
            icon.Margin = new Thickness(0, 0, 16, 0);
            row.Children.Add(icon);
        }
        var text = (FrameworkElement)words;
        text.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(text, 1);
        row.Children.Add(text);
        if (control is FrameworkElement element)
        {
            element.VerticalAlignment = VerticalAlignment.Center;
            element.HorizontalAlignment = HorizontalAlignment.Right;
            element.Margin = new Thickness(16, 0, 0, 0);
            Grid.SetColumn(element, 2);
            row.Children.Add(element);
        }
        return row;
    }

    /// <summary>A row of a label and its value, the value selectable.</summary>
    public static Grid Value(string label, string value, bool monospace = false)
    {
        var text = new TextBlock
        {
            Text = value,
            IsTextSelectionEnabled = true,
            TextWrapping = TextWrapping.Wrap,
            TextAlignment = TextAlignment.Right,
            Foreground = Brand.System("TextFillColorSecondaryBrush"),
            MaxWidth = 420,
        };
        if (monospace)
        {
            text.FontFamily = new FontFamily("Consolas");
        }
        return Row(label, null, text);
    }

    /// <summary>A switch without On/Off words, its name for screen readers the row's title.</summary>
    public static ToggleSwitch Switch(string name, bool on, Action<bool> changed)
    {
        var toggle = new ToggleSwitch { IsOn = on, OnContent = "", OffContent = "", MinWidth = 0 };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(toggle, name);
        toggle.Toggled += (_, _) =>
        {
            if (toggle.Tag is not "updating")
            {
                changed(toggle.IsOn);
            }
        };
        return toggle;
    }

    /// <summary>Moves a switch to what the service reports, without sending it back.</summary>
    public static void Set(ToggleSwitch toggle, bool on)
    {
        if (toggle.IsOn == on)
        {
            return;
        }
        toggle.Tag = "updating";
        toggle.IsOn = on;
        toggle.Tag = null;
    }

    public static Button Button(string text, Action click, bool accent = false)
    {
        var button = new Button { Content = text };
        if (accent)
        {
            button.Style = Style("AccentButtonStyle");
        }
        button.Click += (_, _) => click();
        return button;
    }

    public static TextBlock Strong(string text) => new() { Text = text, FontWeight = FontWeights.SemiBold };
}
