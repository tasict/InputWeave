using InputWeave.Main;
using InputWeave.Support;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;

namespace InputWeave.Panel;

/// <summary>One exception: a warning sign, what is wrong, and the action that fixes it.</summary>
internal static class ExceptionRowView
{
    public static Grid For(ExceptionRow row, bool compact)
    {
        var grid = new Grid { ColumnSpacing = 8 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        var sign = new FontIcon
        {
            Glyph = "",
            FontSize = 14,
            Foreground = Brand.System("SystemFillColorCautionBrush"),
            VerticalAlignment = VerticalAlignment.Top,
            Margin = new Thickness(0, 1, 0, 0),
        };
        AutomationProperties.SetAccessibilityView(sign, AccessibilityView.Raw);
        grid.Children.Add(sign);

        var text = new TextBlock
        {
            Text = Strings.Text(row.Text),
            Style = (Style)Application.Current.Resources[compact ? "CaptionTextBlockStyle" : "BodyTextBlockStyle"],
            TextWrapping = TextWrapping.Wrap,
            VerticalAlignment = VerticalAlignment.Center,
        };
        Grid.SetColumn(text, 1);
        grid.Children.Add(text);

        if (row.ActionKey is { } key)
        {
            var label = Strings.Get(key);
            var content = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
            if (row.Action == ExceptionAction.StartService)
            {
                // The shield says the button asks for an administrator.
                content.Children.Add(new FontIcon { Glyph = "", FontSize = 12 });
            }
            content.Children.Add(new TextBlock { Text = label, FontSize = compact ? 12 : 14 });
            var button = new HyperlinkButton
            {
                Content = content,
                Padding = new Thickness(4, 2, 4, 2),
                VerticalAlignment = VerticalAlignment.Center,
            };
            AutomationProperties.SetName(button, label);
            button.Click += (_, _) => Act(row.Action);
            Grid.SetColumn(button, 2);
            grid.Children.Add(button);
        }
        return grid;
    }

    private static void Act(ExceptionAction action)
    {
        switch (action)
        {
            case ExceptionAction.StartService:
                ServiceWatcher.Start();
                break;
            case ExceptionAction.Troubleshoot:
                App.Instance.ShowMain(MainPage.Troubleshooting);
                break;
            case ExceptionAction.AddDevice:
                App.Instance.ShowMain(MainPage.Devices, addDevice: true);
                break;
        }
    }
}
