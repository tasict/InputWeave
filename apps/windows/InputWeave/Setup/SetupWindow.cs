using InputWeave.Agent;
using InputWeave.Canvas;
using InputWeave.Main;
using InputWeave.Pairing;
using InputWeave.Panel;
using InputWeave.Support;
using Microsoft.UI.Composition.SystemBackdrops;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Text;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Windows.Graphics;
using WinRT.Interop;

namespace InputWeave.Setup;

/// <summary>
/// First-run setup on Windows: welcome, this
/// computer's name, the network check while the network is set to Public,
/// connecting another computer and typing its code, the arrangement, and
/// trying it. Each step but the first has Back, which only moves back: what
/// a step did when the user continued from it (renaming) is not undone.
/// Leaving the connect steps cancels a pairing they started. Finishing, or
/// putting it off with Later, keeps it from opening again at launch.
/// </summary>
internal sealed partial class SetupWindow : Window
{
    private const double WindowWidth = 620;
    private const double WindowHeight = 520;
    private static readonly TimeSpan NetworkRecheck = TimeSpan.FromSeconds(3);

    private readonly AgentModel agent;
    private readonly IReadOnlyList<SetupStep> steps;
    private readonly Grid root = new();
    private readonly ContentControl page = new()
    {
        HorizontalContentAlignment = HorizontalAlignment.Stretch,
        VerticalContentAlignment = VerticalAlignment.Stretch,
        IsTabStop = false,
    };
    private readonly ProgressBar progress = new() { Width = 140, Maximum = 1, VerticalAlignment = VerticalAlignment.Center };
    private readonly TextBlock stepText = new() { VerticalAlignment = VerticalAlignment.Center };
    private readonly Button back = new();
    private readonly Button later = new();
    private readonly Button primary = new();
    private readonly PairingFlow flow;
    private readonly ArrangementCanvas canvas = new() { Interactive = true, ContentPadding = 24 };
    private readonly TextBox name = new() { MaxLength = 64, Width = 320, HorizontalAlignment = HorizontalAlignment.Left };
    private readonly TextBlock nameError = new() { TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed };
    private readonly TextBlock arrangeHint = new() { TextWrapping = TextWrapping.Wrap };
    private readonly TextBlock tryHint = new() { TextWrapping = TextWrapping.Wrap };
    private readonly StackPanel tryDone = new() { Orientation = Orientation.Horizontal, Spacing = 8, Visibility = Visibility.Collapsed };
    private readonly TextBlock tryDoneText = new() { TextWrapping = TextWrapping.Wrap };
    private readonly StackPanel networkPublic = new() { Spacing = 12 };
    private readonly StackPanel networkFixed = new() { Orientation = Orientation.Horizontal, Spacing = 8, Visibility = Visibility.Collapsed };
    private readonly DispatcherQueueTimer networkTimer;
    private readonly Dictionary<SetupStep, FrameworkElement> views = [];
    private TextBlock connectHeading = new();
    private SetupStep step;
    private AgentSnapshot? model;
    private string? pairedWith;
    private ulong returnsBefore;
    private bool namedByUser;
    private bool finished;

    /// <param name="start">The step to open at, for screenshots; setup starts with Welcome.</param>
    public SetupWindow(AgentModel agent, SetupStep start = SetupStep.Welcome)
    {
        this.agent = agent;
        Title = Strings.Get(TextKeys.SetupWindowTitle);
        bool publicNetwork = NetworkCheck.IsPublic(ComCalls.ConnectedNetworkCategories());
        steps = SetupSteps.For(publicNetwork || start == SetupStep.Network);
        AppLog.Info($"setup opened; network set to Public: {publicNetwork}");
        flow = new PairingFlow(agent) { Height = 300 };
        flow.PhaseChanged += (_, _) =>
        {
            if (step is SetupStep.Connect or SetupStep.EnterCode)
            {
                ShowStep(SetupSteps.ForPairing(flow.Phase));
            }
        };
        flow.Finished += (_, _) =>
        {
            pairedWith = flow.PairedWith;
            Go(SetupStep.Arrange);
        };
        networkTimer = DispatcherQueue.GetForCurrentThread().CreateTimer();
        networkTimer.Interval = NetworkRecheck;
        networkTimer.Tick += (_, _) => CheckNetwork();

        Build();
        Content = root;
        var icon = Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon.ico");
        if (File.Exists(icon))
        {
            AppWindow.SetIcon(icon);
        }
        var presenter = OverlappedPresenter.Create();
        presenter.IsResizable = false;
        presenter.IsMaximizable = false;
        presenter.IsMinimizable = false;
        AppWindow.SetPresenter(presenter);
        if (MicaController.IsSupported())
        {
            SystemBackdrop = new MicaBackdrop();
        }
        else
        {
            root.Background = Brand.System("SolidBackgroundFillColorBaseBrush");
        }
        AppWindow.Closing += (_, _) =>
        {
            networkTimer.Stop();
            flow.CancelInProgress();
        };
        Closed += (_, _) => Dismissed?.Invoke(this, EventArgs.Empty);

        if (start is SetupStep.EnterCode)
        {
            flow.ShowSample("MacBook Pro");
            start = SetupStep.EnterCode;
        }
        Go(start);
    }

    /// <summary>Raised once the window has closed, finished or not.</summary>
    public event EventHandler? Dismissed;

    /// <summary>Opens the window in the middle of the main screen. Pinned, it stays above other windows (for testing).</summary>
    public void Open(bool pin)
    {
        var area = DisplayArea.Primary;
        double scale = Native.DpiForMonitor(Microsoft.UI.Win32Interop.GetMonitorFromDisplayId(area.DisplayId)) / 96.0;
        var work = area.WorkArea;
        AppWindow.ResizeClient(new SizeInt32((int)Math.Ceiling(WindowWidth * scale), (int)Math.Ceiling(WindowHeight * scale)));
        var size = AppWindow.Size;
        AppWindow.Move(new PointInt32(work.X + (work.Width - size.Width) / 2, work.Y + (work.Height - size.Height) / 2));
        if (pin && AppWindow.Presenter is OverlappedPresenter presenter)
        {
            presenter.IsAlwaysOnTop = true;
        }
        AppWindow.Show(activateWindow: true);
        Activate();
        Native.SetForegroundWindow(WindowNative.GetWindowHandle(this));
    }

    /// <summary>Follows what the service reports.</summary>
    public void Update(AgentSnapshot snapshot)
    {
        model = snapshot;
        flow.Update(snapshot);
        canvas.Update(snapshot);
        // The name box follows the service until the user types in it.
        if (!namedByUser && name.FocusState == FocusState.Unfocused && name.Text != snapshot.Status.ThisDeviceName)
        {
            name.Text = snapshot.Status.ThisDeviceName;
        }
        var other = SetupSteps.OtherName(snapshot, pairedWith) ?? Strings.Get(TextKeys.PairingTheOtherComputer);
        arrangeHint.Text = Strings.Format(TextKeys.SetupArrangeHint, other);
        tryHint.Text = Strings.Format(TextKeys.SetupTryHint, other);
        // The take-back hotkey the service reports, as it may have been changed.
        tryDoneText.Text = Strings.Text(snapshot.TakeBackWorked);
        bool worked = step == SetupStep.TryIt && SetupSteps.Worked(snapshot, returnsBefore);
        if (worked && tryDone.Visibility != Visibility.Visible)
        {
            AppLog.Info("setup: the first crossing worked");
        }
        tryDone.Visibility = worked ? Visibility.Visible : Visibility.Collapsed;
        tryHint.Visibility = worked ? Visibility.Collapsed : Visibility.Visible;
        UpdateFooter();
    }

    private void Build()
    {
        root.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        root.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        var body = new Border { Padding = new Thickness(32, 24, 32, 16), Child = page };
        root.Children.Add(body);

        var footer = new Grid
        {
            Padding = new Thickness(20, 14, 20, 14),
            ColumnSpacing = 12,
            BorderBrush = Brand.System("DividerStrokeColorDefaultBrush"),
            BorderThickness = new Thickness(0, 1, 0, 0),
        };
        for (int i = 0; i < 6; i++)
        {
            footer.ColumnDefinitions.Add(new ColumnDefinition { Width = i == 2 ? new GridLength(1, GridUnitType.Star) : GridLength.Auto });
        }
        AutomationProperties.SetName(progress, Strings.Get(TextKeys.SetupProgress));
        footer.Children.Add(progress);
        stepText.Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"];
        stepText.Foreground = Brand.System("TextFillColorSecondaryBrush");
        Grid.SetColumn(stepText, 1);
        footer.Children.Add(stepText);
        back.Content = Strings.Get(TextKeys.SetupBack);
        back.Click += (_, _) => GoBack();
        Grid.SetColumn(back, 3);
        footer.Children.Add(back);
        later.Content = Strings.Get(TextKeys.SetupLater);
        later.Click += (_, _) =>
        {
            flow.CancelInProgress();
            Finish();
        };
        Grid.SetColumn(later, 4);
        footer.Children.Add(later);
        primary.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
        primary.MinWidth = 96;
        primary.Click += (_, _) => Advance();
        Grid.SetColumn(primary, 5);
        footer.Children.Add(primary);
        Grid.SetRow(footer, 1);
        root.Children.Add(footer);

        // Enter is the step's main button, as Return is on the Mac.
        var enter = new KeyboardAccelerator { Key = Windows.System.VirtualKey.Enter };
        enter.Invoked += (_, e) =>
        {
            if (step == SetupStep.Welcome)
            {
                e.Handled = true;
                Advance();
            }
            else if (primary.Visibility == Visibility.Visible && primary.IsEnabled)
            {
                e.Handled = true;
                Advance();
            }
        };
        root.KeyboardAccelerators.Add(enter);

        views[SetupStep.Welcome] = Welcome();
        views[SetupStep.Name] = NameStep();
        views[SetupStep.Network] = NetworkStep();
        var connect = Step(SetupStep.Connect, "\uE71B", flow);
        connectHeading = (TextBlock)((StackPanel)connect.Children[0]).Children[1];
        views[SetupStep.Connect] = connect;
        views[SetupStep.EnterCode] = connect;
        var stage = new Border { Height = 240, CornerRadius = new CornerRadius(8), Child = canvas };
        stage.Loaded += (_, _) => stage.Background = Brand.For(stage).Canvas;
        stage.ActualThemeChanged += (_, _) => stage.Background = Brand.For(stage).Canvas;
        AutomationProperties.SetName(canvas, Strings.Get(TextKeys.NavArrangement));
        views[SetupStep.Arrange] = Step(SetupStep.Arrange, "\uF246", arrangeHint, stage);
        tryDone.Children.Add(new FontIcon { Glyph = "\uE930", Foreground = Brand.System("SystemFillColorSuccessBrush"), VerticalAlignment = VerticalAlignment.Top, Margin = new Thickness(0, 2, 0, 0) });
        tryDoneText.Text = Strings.Text((model ?? new AgentSnapshot()).TakeBackWorked);
        tryDone.Children.Add(tryDoneText);
        AutomationProperties.SetLiveSetting(tryDoneText, AutomationLiveSetting.Polite);
        views[SetupStep.TryIt] = Step(SetupStep.TryIt, "\uE962", tryHint, tryDone);
    }

    /// <summary>A step: its title with a glyph, then its content.</summary>
    private static StackPanel Step(SetupStep step, string glyph, params UIElement[] content)
    {
        var stack = new StackPanel { Spacing = 14 };
        var title = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10 };
        title.Children.Add(new FontIcon { Glyph = glyph, FontSize = 22, Foreground = Brand.System("AccentTextFillColorPrimaryBrush") });
        var text = new TextBlock
        {
            Text = Strings.Get(SetupSteps.TitleKey(step)),
            Style = (Style)Application.Current.Resources["SubtitleTextBlockStyle"],
            VerticalAlignment = VerticalAlignment.Center,
        };
        AutomationProperties.SetHeadingLevel(text, Microsoft.UI.Xaml.Automation.Peers.AutomationHeadingLevel.Level1);
        title.Children.Add(text);
        stack.Children.Add(title);
        foreach (var item in content)
        {
            stack.Children.Add(item);
        }
        return stack;
    }

    private StackPanel Welcome()
    {
        var stack = new StackPanel { Spacing = 18, HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center };
        var logo = Path.Combine(AppContext.BaseDirectory, "Assets", "AppIcon256.png");
        stack.Children.Add(new Image
        {
            Source = new BitmapImage(new Uri(logo)),
            Width = 96,
            Height = 96,
            HorizontalAlignment = HorizontalAlignment.Center,
        });
        var tagline = new TextBlock
        {
            Text = Strings.Get(TextKeys.SetupWelcomeTitle),
            Style = (Style)Application.Current.Resources["SubtitleTextBlockStyle"],
            TextAlignment = TextAlignment.Center,
            TextWrapping = TextWrapping.Wrap,
            HorizontalAlignment = HorizontalAlignment.Center,
        };
        AutomationProperties.SetHeadingLevel(tagline, Microsoft.UI.Xaml.Automation.Peers.AutomationHeadingLevel.Level1);
        stack.Children.Add(tagline);
        var benefits = new StackPanel { Spacing = 10, HorizontalAlignment = HorizontalAlignment.Center };
        benefits.Children.Add(Benefit("\uE7F4", TextKeys.SetupBenefitEdges));
        benefits.Children.Add(Benefit("\uE77F", TextKeys.SetupBenefitClipboard));
        benefits.Children.Add(Benefit("\uE898", TextKeys.SetupBenefitFiles));
        stack.Children.Add(benefits);
        stack.Children.Add(new TextBlock
        {
            Text = Strings.Get(TextKeys.SetupReassurance),
            Foreground = Brand.System("TextFillColorSecondaryBrush"),
            TextAlignment = TextAlignment.Center,
            TextWrapping = TextWrapping.Wrap,
            HorizontalAlignment = HorizontalAlignment.Center,
        });
        var start = new Button
        {
            Content = Strings.Get(TextKeys.SetupGetStarted),
            Style = (Style)Application.Current.Resources["AccentButtonStyle"],
            HorizontalAlignment = HorizontalAlignment.Center,
            Padding = new Thickness(24, 8, 24, 9),
        };
        start.Click += (_, _) => Advance();
        stack.Children.Add(start);
        return stack;
    }

    private static StackPanel Benefit(string glyph, string key)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 12 };
        row.Children.Add(new FontIcon { Glyph = glyph, FontSize = 18, Width = 24, Foreground = Brand.System("AccentTextFillColorPrimaryBrush") });
        row.Children.Add(new TextBlock { Text = Strings.Get(key), TextWrapping = TextWrapping.Wrap, MaxWidth = 440, VerticalAlignment = VerticalAlignment.Center });
        return row;
    }

    private StackPanel NameStep()
    {
        name.Header = Strings.Get(TextKeys.SetupNameLabel);
        name.TextChanged += (_, _) =>
        {
            if (name.FocusState != FocusState.Unfocused)
            {
                namedByUser = true;
            }
            nameError.Visibility = Visibility.Collapsed;
            UpdateFooter();
        };
        nameError.Foreground = Brand.System("SystemFillColorCriticalBrush");
        nameError.Text = Strings.Get(TextKeys.NameInvalid);
        return Step(SetupStep.Name, "\uE8AC", name,
            new TextBlock { Text = Strings.Get(TextKeys.SetupNameHint), Foreground = Brand.System("TextFillColorSecondaryBrush"), TextWrapping = TextWrapping.Wrap },
            nameError);
    }

    private StackPanel NetworkStep()
    {
        networkPublic.Children.Add(new TextBlock { Text = Strings.Get(TextKeys.SetupNetworkPublic), TextWrapping = TextWrapping.Wrap });
        networkPublic.Children.Add(new TextBlock
        {
            Text = Strings.Get(TextKeys.SetupNetworkHowTo),
            Foreground = Brand.System("TextFillColorSecondaryBrush"),
            TextWrapping = TextWrapping.Wrap,
        });
        var open = new Button { Content = Strings.Get(TextKeys.SetupNetworkOpenSettings) };
        open.Click += async (_, _) =>
        {
            try
            {
                await Windows.System.Launcher.LaunchUriAsync(new Uri("ms-settings:network"));
            }
            catch (Exception e)
            {
                AppLog.Warn($"cannot open the network settings: {e.Message}");
            }
        };
        networkPublic.Children.Add(open);
        networkFixed.Children.Add(new FontIcon { Glyph = "\uE930", Foreground = Brand.System("SystemFillColorSuccessBrush") });
        var text = new TextBlock { Text = Strings.Get(TextKeys.SetupNetworkPrivate), TextWrapping = TextWrapping.Wrap };
        AutomationProperties.SetLiveSetting(text, AutomationLiveSetting.Polite);
        networkFixed.Children.Add(text);
        return Step(SetupStep.Network, "\uE701", networkPublic, networkFixed);
    }

    /// <summary>Whether the network is still set to Public; the user may be changing it in Settings.</summary>
    private void CheckNetwork()
    {
        bool still = NetworkCheck.IsPublic(ComCalls.ConnectedNetworkCategories());
        networkPublic.Visibility = still ? Visibility.Visible : Visibility.Collapsed;
        networkFixed.Visibility = still ? Visibility.Collapsed : Visibility.Visible;
        UpdateFooter();
    }

    private void Go(SetupStep next)
    {
        if (next == SetupStep.TryIt)
        {
            returnsBefore = model?.Status.ControlReturns ?? 0;
        }
        if (next == SetupStep.Network)
        {
            CheckNetwork();
            networkTimer.Start();
        }
        else
        {
            networkTimer.Stop();
        }
        ShowStep(next);
        if (model is not null)
        {
            Update(model);
        }
        if (next == SetupStep.Name)
        {
            DispatcherQueue.TryEnqueue(DispatcherQueuePriority.Low, () => name.Focus(FocusState.Programmatic));
        }
    }

    private void ShowStep(SetupStep next)
    {
        step = next;
        page.Content = views[next];
        int number = SetupSteps.Number(steps, next);
        progress.Value = (double)number / steps.Count;
        stepText.Text = Strings.Format(TextKeys.SetupStepOf, number.ToString(), steps.Count.ToString());
        // Connecting and the code share a view; its title says which step it is.
        connectHeading.Text = Strings.Get(SetupSteps.TitleKey(next is SetupStep.EnterCode ? next : SetupStep.Connect));
        UpdateFooter();
    }

    private void UpdateFooter()
    {
        back.Visibility = step == SetupStep.Welcome ? Visibility.Collapsed : Visibility.Visible;
        bool connecting = step is SetupStep.Connect or SetupStep.EnterCode;
        later.Visibility = connecting && flow.Phase != Inputweave.App.V1.PairingPhase.Paired ? Visibility.Visible : Visibility.Collapsed;
        // Connecting moves on through the pairing flow's own Continue; when
        // another computer is already paired, as when setup runs again, it
        // can also be skipped.
        bool canSkipPairing = step == SetupStep.Connect && !flow.InProgress && (model?.Status.Devices.Count ?? 0) > 0;
        primary.Visibility = step == SetupStep.Welcome || (connecting && !canSkipPairing) ? Visibility.Collapsed : Visibility.Visible;
        primary.Content = Strings.Get(step switch
        {
            SetupStep.TryIt => TextKeys.SetupDone,
            SetupStep.Network when networkPublic.Visibility == Visibility.Visible => TextKeys.SetupSkip,
            _ => TextKeys.SetupContinue,
        });
        primary.IsEnabled = step != SetupStep.Name || name.Text.Trim().Length != 0;
    }

    private async void Advance()
    {
        switch (step)
        {
            case SetupStep.Name:
                var wanted = name.Text.Trim();
                if (wanted.Length == 0)
                {
                    return;
                }
                if (wanted != model?.Status.ThisDeviceName)
                {
                    primary.IsEnabled = false;
                    try
                    {
                        await agent.RenameAsync(wanted);
                    }
                    catch (Exception e)
                    {
                        AppLog.Info($"setup rename: {e.Message}");
                        nameError.Visibility = Visibility.Visible;
                        UpdateFooter();
                        return;
                    }
                }
                break;
            case SetupStep.TryIt:
                Finish();
                return;
        }
        if (step == SetupStep.EnterCode)
        {
            return;
        }
        if (SetupSteps.Next(steps, step) is { } next)
        {
            Go(next == SetupStep.EnterCode ? SetupStep.Arrange : next);
        }
    }

    private void GoBack()
    {
        if (step is SetupStep.EnterCode)
        {
            // Back from the code is back to the list: the pairing ends.
            flow.CancelInProgress();
            Go(SetupStep.Connect);
            return;
        }
        if (step is SetupStep.Connect)
        {
            flow.CancelInProgress();
        }
        var previous = SetupSteps.Previous(steps, step);
        if (previous == SetupStep.EnterCode)
        {
            previous = SetupStep.Connect;
        }
        if (previous is { } p)
        {
            Go(p);
        }
    }

    private void Finish()
    {
        if (!finished)
        {
            finished = true;
            Preferences.FinishedSetup = true;
            AppLog.Info($"setup finished at {step}");
        }
        Close();
    }
}
