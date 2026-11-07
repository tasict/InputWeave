using System.Globalization;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Shapes;

namespace InputWeave.Main;

/// <summary>
/// Every trusted computer with how it is connected (since when it is
/// offline, with a moon after its name, when it can be woken), the same
/// inspector as the arrangement page, and the way to add more.
/// </summary>
internal sealed partial class DevicesPage : UserControl, IMainPage
{
    private readonly ListView list = new() { SelectionMode = ListViewSelectionMode.Single, Padding = new Thickness(8, 0, 8, 0) };
    private readonly DeviceInspector inspector = new();
    private readonly Grid devices;
    private readonly StackPanel empty;
    private AgentSnapshot? model;
    private DateTimeOffset now = DateTimeOffset.UtcNow;
    private string? selection;
    private string shownList = "";
    private List<string> shownStatuses = [];
    private bool filling;

    public DevicesPage()
    {
        AutomationProperties.SetName(list, Strings.Get(TextKeys.NavDevices));
        list.SelectionChanged += (_, _) =>
        {
            if (!filling && list.SelectedItem is ListViewItem { Tag: string id })
            {
                selection = id;
                ShowInspector();
            }
        };

        empty = new StackPanel
        {
            HorizontalAlignment = HorizontalAlignment.Center,
            VerticalAlignment = VerticalAlignment.Center,
            Spacing = 12,
            MaxWidth = 420,
        };
        empty.Children.Add(new FontIcon
        {
            Glyph = "",
            FontSize = 40,
            Foreground = Brand.System("TextFillColorSecondaryBrush"),
        });
        empty.Children.Add(new TextBlock
        {
            Text = Strings.Get(TextKeys.DevicesNoneTitle),
            Style = Ui.Style("SubtitleTextBlockStyle"),
            HorizontalAlignment = HorizontalAlignment.Center,
        });
        var detail = Ui.Secondary(Strings.Get(TextKeys.DevicesNoneDetail), caption: false);
        detail.TextAlignment = TextAlignment.Center;
        empty.Children.Add(detail);
        var addFirst = Ui.Button(Strings.Get(TextKeys.ActionAddDevice), AddDevice, accent: true);
        addFirst.HorizontalAlignment = HorizontalAlignment.Center;
        empty.Children.Add(addFirst);

        devices = new Grid();
        devices.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        devices.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        devices.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        devices.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1) });
        devices.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(260) });
        list.Margin = new Thickness(0, 8, 0, 0);
        devices.Children.Add(list);
        var line = Ui.Divider();
        line.Height = double.NaN;
        line.Width = 1;
        Grid.SetColumn(line, 1);
        Grid.SetRowSpan(line, 2);
        devices.Children.Add(line);
        inspector.Padding = new Thickness(0, 16, 0, 0);
        Grid.SetColumn(inspector, 2);
        Grid.SetRowSpan(inspector, 2);
        devices.Children.Add(inspector);
        var add = Ui.Button(Strings.Get(TextKeys.ActionAddDevice), AddDevice);
        add.HorizontalAlignment = HorizontalAlignment.Right;
        add.Margin = new Thickness(12);
        Grid.SetRow(add, 1);
        devices.Children.Add(add);

        var root = new Grid();
        root.Children.Add(empty);
        root.Children.Add(devices);
        Content = root;
    }

    private static void AddDevice() => App.Instance.ShowMain(null, addDevice: true);

    public void Invalidate()
    {
        model = null;
        inspector.Invalidate();
    }

    public void Update(AgentSnapshot snapshot, ServiceState service, DateTimeOffset now)
    {
        this.now = now;
        var all = snapshot.Status.Devices;
        // Each computer's status as of now: since when an offline one is
        // offline moves on without the service saying anything.
        var statuses = all.Select(d => Strings.Text(DeviceText.Status(snapshot, d.Id, now, CultureInfo.CurrentCulture))).ToList();
        if (ReferenceEquals(snapshot, model) && statuses.SequenceEqual(shownStatuses))
        {
            return;
        }
        model = snapshot;
        shownStatuses = statuses;
        empty.Visibility = all.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        devices.Visibility = all.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
        if (all.Count == 0)
        {
            return;
        }
        if (selection is null || all.All(d => d.Id != selection))
        {
            selection = all[0].Id;
        }

        // Rebuilt only when what the rows show changes, so the selection and focus stay.
        var key = string.Join("|", all.Select((d, i) => $"{d.Id},{d.Name},{d.Os},{d.State},{d.ScreenLocked},{d.SharingPaused},{statuses[i]},{Wake.Offered(snapshot, d.Id)},{snapshot.JumpShortcut(d.JumpSlot)}"));
        if (key != shownList)
        {
            shownList = key;
            filling = true;
            list.Items.Clear();
            for (int i = 0; i < all.Count; i++)
            {
                list.Items.Add(Row(all[i], snapshot.JumpShortcut(all[i].JumpSlot), statuses[i], Wake.Offered(snapshot, all[i].Id)));
            }
            filling = false;
        }
        filling = true;
        list.SelectedItem = list.Items.OfType<ListViewItem>().FirstOrDefault(i => (string)i.Tag == selection);
        filling = false;
        ShowInspector();
    }

    private void ShowInspector()
    {
        if (model is not null && selection is not null)
        {
            inspector.Update(model, selection, now);
        }
    }

    /// <summary>
    /// A computer: its name (and a moon after it when it can be woken) and
    /// system, and on the right the shortcut that jumps to it, if any, and
    /// its <paramref name="status"/>: whether it is connected and how fast.
    /// </summary>
    private static ListViewItem Row(DeviceStatus device, string? jump, string status, bool moon)
    {
        var grid = new Grid { ColumnSpacing = 12, Padding = new Thickness(4, 10, 4, 10) };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.Children.Add(new FontIcon
        {
            Glyph = "",
            FontSize = 20,
            Foreground = Brand.System("TextFillColorSecondaryBrush"),
            VerticalAlignment = VerticalAlignment.Center,
        });
        var words = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        var nameText = new TextBlock { Text = device.Name, TextTrimming = TextTrimming.CharacterEllipsis };
        if (moon)
        {
            // Left-aligned, the name's column is only as wide as the name
            // until room runs short: the moon stays beside it, and the name
            // is cut short first.
            var line = new Grid { HorizontalAlignment = HorizontalAlignment.Left, ColumnSpacing = 5 };
            line.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            line.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            line.Children.Add(nameText);
            var moonIcon = new FontIcon { Glyph = Wake.MoonGlyph, FontSize = 12, VerticalAlignment = VerticalAlignment.Center };
            // The status says it can be woken; the moon only shows it.
            AutomationProperties.SetAccessibilityView(moonIcon, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
            Grid.SetColumn(moonIcon, 1);
            line.Children.Add(moonIcon);
            words.Children.Add(line);
        }
        else
        {
            words.Children.Add(nameText);
        }
        words.Children.Add(Ui.Secondary(Strings.Text(DeviceText.Os(device.Os))));
        Grid.SetColumn(words, 1);
        grid.Children.Add(words);

        bool connected = device.State == ConnectionState.Connected;
        var state = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6, VerticalAlignment = VerticalAlignment.Center };
        var color = Brand.System(connected ? "SystemFillColorSuccessBrush" : "TextFillColorSecondaryBrush");
        // Not by color alone: offline is a dashed ring and says so, a
        // locked screen a padlock, and paused sharing a pause.
        if (DeviceText.StatusGlyph(connected, device.ScreenLocked, device.SharingPaused) is { } glyph)
        {
            state.Children.Add(new FontIcon
            {
                Glyph = glyph,
                FontSize = 10,
                Foreground = color,
                VerticalAlignment = VerticalAlignment.Center,
            });
        }
        else
        {
            var dot = new Ellipse { Width = 8, Height = 8, VerticalAlignment = VerticalAlignment.Center };
            if (connected)
            {
                dot.Fill = color;
            }
            else
            {
                dot.Stroke = color;
                dot.StrokeThickness = 1;
                dot.StrokeDashArray = new DoubleCollection { 2, 1.5 };
            }
            state.Children.Add(dot);
        }
        state.Children.Add(new TextBlock
        {
            Text = status,
            Style = Ui.Style("CaptionTextBlockStyle"),
            Foreground = color,
        });
        Grid.SetColumn(state, 3);
        grid.Children.Add(state);
        if (jump is not null)
        {
            var shortcut = new TextBlock
            {
                Text = jump,
                Style = Ui.Style("CaptionTextBlockStyle"),
                Foreground = Brand.System("TextFillColorSecondaryBrush"),
                VerticalAlignment = VerticalAlignment.Center,
            };
            Grid.SetColumn(shortcut, 2);
            grid.Children.Add(shortcut);
        }

        var item = new ListViewItem { Content = grid, Tag = device.Id };
        AutomationProperties.SetName(item, $"{device.Name}, {status}");
        if (jump is not null)
        {
            AutomationProperties.SetAcceleratorKey(item, jump);
        }
        return item;
    }
}
