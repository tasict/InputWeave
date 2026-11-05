using InputWeave.Agent;
using InputWeave.Canvas;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;

namespace InputWeave.Main;

/// <summary>
/// The arrangement page: the canvas and,
/// for the selected computer, the inspector; Identify Screens, "Synced to N
/// devices" for a moment after the arrangement changed, and Add Device….
/// "Rename…" on the canvas puts the cursor in the inspector's name field.
/// </summary>
internal sealed partial class ArrangementPage : UserControl, IMainPage
{
    private static readonly TimeSpan SyncedFor = TimeSpan.FromSeconds(2);

    private readonly ArrangementCanvas canvas = new() { Interactive = true, ContentPadding = 40 };
    private readonly DeviceInspector inspector = new();
    private readonly Border synced;
    private readonly TextBlock syncedText = new();
    private readonly DispatcherQueueTimer syncedTimer;
    private readonly Button identifyButton;
    private AgentSnapshot? model;
    private ulong version;

    public ArrangementPage()
    {
        AutomationProperties.SetName(canvas, Strings.Get(TextKeys.NavArrangement));
        canvas.SelectionChanged += (_, _) => ShowInspector();
        canvas.RenameRequested += (_, id) =>
        {
            ShowInspector();
            inspector.FocusName(id);
        };
        synced = new Border
        {
            Child = syncedText,
            Padding = new Thickness(10, 4, 10, 4),
            CornerRadius = new CornerRadius(12),
            Background = Brand.System("AcrylicInAppFillColorDefaultBrush"),
            BorderBrush = Brand.System("CardStrokeColorDefaultBrush"),
            BorderThickness = new Thickness(1),
            VerticalAlignment = VerticalAlignment.Center,
            Visibility = Visibility.Collapsed,
        };
        AutomationProperties.SetLiveSetting(syncedText, AutomationLiveSetting.Polite);
        syncedTimer = DispatcherQueue.GetForCurrentThread().CreateTimer();
        syncedTimer.Interval = SyncedFor;
        syncedTimer.IsRepeating = false;
        syncedTimer.Tick += (_, _) => synced.Visibility = Visibility.Collapsed;

        var bar = new Grid { Margin = new Thickness(12), VerticalAlignment = VerticalAlignment.Bottom };
        bar.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        bar.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var identify = Ui.Button(Strings.Get(TextKeys.IdentifyScreens), App.Instance.IdentifyScreens);
        ToolTipService.SetToolTip(identify, Strings.Get(TextKeys.IdentifyScreensHint));
        var left = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12, HorizontalAlignment = HorizontalAlignment.Left };
        left.Children.Add(identify);
        left.Children.Add(synced);
        bar.Children.Add(left);
        identifyButton = identify;
        var add = Ui.Button(Strings.Get(TextKeys.ActionAddDevice), () => App.Instance.ShowMain(null, addDevice: true));
        Grid.SetColumn(add, 1);
        bar.Children.Add(add);

        var stage = new Grid { CornerRadius = new CornerRadius(8, 0, 0, 0) };
        stage.Loaded += (_, _) => stage.Background = Brand.For(stage).Canvas;
        stage.ActualThemeChanged += (_, _) => stage.Background = Brand.For(stage).Canvas;
        stage.Children.Add(canvas);
        stage.Children.Add(bar);

        var root = new Grid();
        root.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        root.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1) });
        root.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(260) });
        root.Children.Add(stage);
        var line = Ui.Divider();
        line.Height = double.NaN;
        line.Width = 1;
        Grid.SetColumn(line, 1);
        root.Children.Add(line);
        inspector.Padding = new Thickness(0, 16, 0, 0);
        Grid.SetColumn(inspector, 2);
        root.Children.Add(inspector);
        Content = root;
    }

    public void Invalidate()
    {
        model = null;
        inspector.Invalidate();
    }

    public void Update(AgentSnapshot snapshot, ServiceState service, DateTimeOffset now)
    {
        if (ReferenceEquals(snapshot, model))
        {
            return;
        }
        model = snapshot;
        var next = snapshot.Arrangement.Version;
        if (DeviceText.Synced(version, next, snapshot) is { } text)
        {
            syncedText.Text = Strings.Text(text);
            synced.Visibility = Visibility.Visible;
            syncedTimer.Stop();
            syncedTimer.Start();
        }
        version = next;
        identifyButton.IsEnabled = snapshot.Link == AgentLink.Connected;
        canvas.Update(snapshot);
        ShowInspector();
    }

    private void ShowInspector()
    {
        if (model is not null)
        {
            inspector.Update(model, canvas.Selection ?? model.ThisDeviceId);
        }
    }
}
