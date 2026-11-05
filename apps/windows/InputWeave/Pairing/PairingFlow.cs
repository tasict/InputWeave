using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using Microsoft.UI.Dispatching;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Windows.System;

namespace InputWeave.Pairing;

/// <summary>
/// Adding a device from this computer, as the
/// Mac's PairingFlow: pick a nearby computer or type its address, then
/// type the six digits it shows. The background service runs the pairing;
/// this shows its phases and sends what the user types. Add Device and the
/// Connect step of first-run setup both show it.
/// </summary>
internal sealed partial class PairingFlow : Grid
{
    private readonly AgentModel agent;
    private readonly Grid chooser = new() { RowSpacing = 12 };
    private readonly StackPanel connecting = Centered();
    private readonly StackPanel codeEntry = Centered();
    private readonly StackPanel paired = Centered();
    private readonly StackPanel failed = Centered();
    private readonly ListView nearby = new() { SelectionMode = ListViewSelectionMode.None, MinHeight = 140 };
    private readonly StackPanel noneFound = Centered();
    private readonly TextBox address = new() { PlaceholderText = "192.168.1.20" };
    private Button addByAddress = new();
    private readonly TextBlock startError = Error();
    private readonly TextBlock codeTitle = Heading();
    private readonly TextBox typed = new() { MaxLength = PairingCode.Length, Width = 1, Height = 1, Opacity = 0, MinWidth = 0, MinHeight = 0, Padding = new Thickness(0) };
    private readonly Border[] boxes = new Border[PairingCode.Length];
    private readonly TextBlock[] digits = new TextBlock[PairingCode.Length];
    private readonly StackPanel boxRow = new() { Orientation = Orientation.Horizontal, Spacing = 8, HorizontalAlignment = HorizontalAlignment.Center };
    private readonly TranslateTransform shake = new();
    private readonly ProgressRing verifying = new() { IsActive = true, Width = 20, Height = 20 };
    private readonly TextBlock codeError = Error();
    private readonly TextBlock pairedText = Heading();
    private readonly TextBlock failedText = new() { TextWrapping = TextWrapping.Wrap, TextAlignment = TextAlignment.Center };
    private readonly Microsoft.UI.Dispatching.DispatcherQueueTimer shaker;
    private DateTime shakeStart;
    private AgentSnapshot? model;
    private string shownNearby = "";
    /// <summary>Whether this flow started the pairing in progress, so one shown here was not left over from before.</summary>
    private bool started;
    private bool submitting;
    /// <summary>A pairing shown for screenshots (<see cref="ShowSample"/>); the service's is not shown meanwhile.</summary>
    private PairingState? sample;

    public PairingFlow(AgentModel agent)
    {
        this.agent = agent;

        BuildChooser();
        connecting.Children.Add(new ProgressRing { IsActive = true, Width = 32, Height = 32 });
        connecting.Children.Add(new TextBlock { Text = Strings.Get(TextKeys.PairingConnecting), HorizontalAlignment = HorizontalAlignment.Center });
        BuildCodeEntry();
        paired.Children.Add(new FontIcon { Glyph = "", FontSize = 44, Foreground = Brand.System("SystemFillColorSuccessBrush") });
        paired.Children.Add(pairedText);
        paired.Children.Add(Accent(Strings.Get(TextKeys.PairingContinue), () =>
        {
            PairedWith = model?.Pairing.PeerDeviceId;
            started = false;
            Finished?.Invoke(this, EventArgs.Empty);
        }));
        failed.Children.Add(new FontIcon { Glyph = "", FontSize = 44, Foreground = Brand.System("TextFillColorSecondaryBrush") });
        failed.Children.Add(failedText);
        failed.Children.Add(Accent(Strings.Get(TextKeys.PairingStartOver), () =>
        {
            started = false;
            Show();
        }));

        foreach (var view in new FrameworkElement[] { chooser, connecting, codeEntry, paired, failed })
        {
            Children.Add(view);
        }

        shaker = Microsoft.UI.Dispatching.DispatcherQueue.GetForCurrentThread().CreateTimer();
        shaker.Interval = TimeSpan.FromMilliseconds(16);
        shaker.Tick += (_, _) =>
        {
            var amount = Math.Clamp((DateTime.UtcNow - shakeStart).TotalMilliseconds / 350, 0, 1);
            shake.X = 6 * Math.Sin(amount * Math.PI * 4);
            if (amount >= 1)
            {
                shaker.Stop();
            }
        };
        Show();
    }

    /// <summary>Paired, and the user moved on with Continue: on to the arrangement.</summary>
    public event EventHandler? Finished;

    /// <summary>The device id of the computer paired here, once the user moved on.</summary>
    public string? PairedWith { get; private set; }

    /// <summary>Raised when the view shown changes, e.g. from the list to the code.</summary>
    public event EventHandler? PhaseChanged;

    /// <summary>The pairing's phase as this flow shows it: idle until it starts one.</summary>
    public PairingPhase Phase => sample?.Phase ?? (started ? model?.Pairing.Phase ?? PairingPhase.Idle : PairingPhase.Idle);

    /// <summary>Whether a pairing started here is still waiting for something.</summary>
    public bool InProgress => started && Phase is PairingPhase.Connecting or PairingPhase.EnterCode or PairingPhase.Verifying;

    /// <summary>
    /// Cancels a pairing started here that is still in progress, as when
    /// the user leaves the flow; it would otherwise wait for a code nobody
    /// enters.
    /// </summary>
    public void CancelInProgress()
    {
        if (InProgress && sample is null)
        {
            App.Instance.Send("cancel pairing", agent.CancelPairingAsync);
        }
        started = false;
        sample = null;
        Show();
    }

    /// <summary>Shows the code entry for a sample pairing, for screenshots.</summary>
    public void ShowSample(string peerName)
    {
        sample = new PairingState { Phase = PairingPhase.EnterCode, PeerName = peerName, AttemptsLeft = 3 };
        Show();
    }

    private static StackPanel Centered() => new()
    {
        Spacing = 12,
        HorizontalAlignment = HorizontalAlignment.Center,
        VerticalAlignment = VerticalAlignment.Center,
        Visibility = Visibility.Collapsed,
    };

    private static TextBlock Heading() => new()
    {
        Style = (Style)Application.Current.Resources["SubtitleTextBlockStyle"],
        TextWrapping = TextWrapping.Wrap,
        TextAlignment = TextAlignment.Center,
        HorizontalAlignment = HorizontalAlignment.Center,
    };

    private static TextBlock Error() => new()
    {
        Foreground = Brand.System("SystemFillColorCriticalBrush"),
        TextWrapping = TextWrapping.Wrap,
        Visibility = Visibility.Collapsed,
    };

    private static Button Accent(string text, Action click)
    {
        var button = new Button
        {
            Content = text,
            Style = (Style)Application.Current.Resources["AccentButtonStyle"],
            HorizontalAlignment = HorizontalAlignment.Center,
        };
        button.Click += (_, _) => click();
        return button;
    }

    private void BuildChooser()
    {
        chooser.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        chooser.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        chooser.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        AutomationProperties.SetName(nearby, Strings.Get(TextKeys.AddDeviceTitle));
        chooser.Children.Add(nearby);
        noneFound.Children.Add(new FontIcon { Glyph = "", FontSize = 32, Foreground = Brand.System("TextFillColorSecondaryBrush") });
        noneFound.Children.Add(new TextBlock
        {
            Text = Strings.Get(TextKeys.PairingNoneFound),
            Style = (Style)Application.Current.Resources["BodyStrongTextBlockStyle"],
            HorizontalAlignment = HorizontalAlignment.Center,
        });
        var detail = new TextBlock
        {
            Text = Strings.Get(TextKeys.PairingNoneFoundDetail),
            Foreground = Brand.System("TextFillColorSecondaryBrush"),
            TextWrapping = TextWrapping.Wrap,
            TextAlignment = TextAlignment.Center,
            MaxWidth = 360,
        };
        noneFound.Children.Add(detail);
        chooser.Children.Add(noneFound);

        var byAddress = new Grid { ColumnSpacing = 8 };
        byAddress.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        byAddress.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        address.Header = Strings.Get(TextKeys.PairingIpAddress);
        address.TextChanged += (_, _) => addByAddress.IsEnabled = address.Text.Trim().Length != 0;
        address.KeyDown += (_, e) =>
        {
            if (e.Key == VirtualKey.Enter)
            {
                e.Handled = true;
                PairByAddress();
            }
        };
        byAddress.Children.Add(address);
        addByAddress = new Button { Content = Strings.Get(TextKeys.PairingAddByIp), IsEnabled = false, VerticalAlignment = VerticalAlignment.Bottom };
        addByAddress.Click += (_, _) => PairByAddress();
        Grid.SetColumn(addByAddress, 1);
        byAddress.Children.Add(addByAddress);
        Grid.SetRow(byAddress, 1);
        chooser.Children.Add(byAddress);
        Grid.SetRow(startError, 2);
        chooser.Children.Add(startError);
    }

    /// <summary>Six boxes for the code shown on the other computer. The sixth digit submits it.</summary>
    private void BuildCodeEntry()
    {
        codeEntry.Children.Add(codeTitle);
        AutomationProperties.SetName(typed, Strings.Get(TextKeys.PairingCodeName));
        typed.InputScope = new InputScope { Names = { new InputScopeName(InputScopeNameValue.Number) } };
        typed.TextChanged += (_, _) => Typed();
        typed.GotFocus += (_, _) => ShowDigits();
        typed.LostFocus += (_, _) => ShowDigits();
        for (int i = 0; i < PairingCode.Length; i++)
        {
            digits[i] = new TextBlock
            {
                FontSize = 28,
                FontWeight = FontWeights.Medium,
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center,
            };
            boxes[i] = new Border
            {
                Width = 40,
                Height = 52,
                CornerRadius = new CornerRadius(8),
                Background = Brand.System("ControlFillColorDefaultBrush"),
                Child = digits[i],
                Margin = new Thickness(0, 0, i == 2 ? 10 : 0, 0),
            };
            AutomationProperties.SetAccessibilityView(boxes[i], Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
            boxRow.Children.Add(boxes[i]);
        }
        boxRow.RenderTransform = shake;
        boxRow.Tapped += (_, _) => typed.Focus(FocusState.Programmatic);
        var holder = new Grid();
        holder.Children.Add(typed);
        holder.Children.Add(boxRow);
        codeEntry.Children.Add(holder);
        verifying.HorizontalAlignment = HorizontalAlignment.Center;
        codeEntry.Children.Add(verifying);
        codeError.TextAlignment = TextAlignment.Center;
        codeEntry.Children.Add(codeError);
    }

    /// <summary>Follows the pairing as the service reports it.</summary>
    public void Update(AgentSnapshot snapshot)
    {
        model = snapshot;
        UpdateNearby(snapshot);
        Show();
    }

    private void Show()
    {
        var pairing = sample ?? model?.Pairing ?? new PairingState();
        var phase = Phase;
        var view = phase switch
        {
            PairingPhase.Connecting => connecting,
            PairingPhase.EnterCode or PairingPhase.Verifying => codeEntry,
            PairingPhase.Paired => paired,
            PairingPhase.Failed => failed,
            _ => (FrameworkElement)chooser,
        };
        bool entering = view == codeEntry && codeEntry.Visibility != Visibility.Visible;
        foreach (var each in new FrameworkElement[] { chooser, connecting, codeEntry, paired, failed })
        {
            each.Visibility = each == view ? Visibility.Visible : Visibility.Collapsed;
        }
        codeTitle.Text = Strings.Format(TextKeys.PairingEnterCode, pairing.PeerName);
        verifying.Visibility = phase == PairingPhase.Verifying ? Visibility.Visible : Visibility.Collapsed;
        bool wrong = phase == PairingPhase.EnterCode && pairing.Error == "wrong_code";
        codeError.Visibility = wrong ? Visibility.Visible : Visibility.Collapsed;
        codeError.Text = Strings.Format(TextKeys.PairingWrongCode, pairing.AttemptsLeft.ToString());
        pairedText.Text = Strings.Format(TextKeys.PairingPaired, pairing.PeerName);
        failedText.Text = Strings.Text(PairingText.Failure(pairing));
        if (entering)
        {
            typed.Text = "";
            DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () => typed.Focus(FocusState.Programmatic));
        }
        ShowDigits();
        if (view != shownView)
        {
            shownView = view;
            PhaseChanged?.Invoke(this, EventArgs.Empty);
        }
    }

    private FrameworkElement? shownView;

    private void UpdateNearby(AgentSnapshot snapshot)
    {
        var candidates = snapshot.Nearby.Where(d => !d.Paired).ToList();
        var key = string.Join("|", candidates.Select(d => $"{d.Id},{d.Name},{d.Os},{d.Pairable}"));
        noneFound.Visibility = candidates.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        nearby.Visibility = candidates.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
        if (key == shownNearby)
        {
            return;
        }
        shownNearby = key;
        nearby.Items.Clear();
        foreach (var device in candidates)
        {
            nearby.Items.Add(NearbyRow(device));
        }
    }

    /// <summary>A computer found nearby: its name and system, and Pair, or why it cannot be paired.</summary>
    private ListViewItem NearbyRow(NearbyDevice device)
    {
        var grid = new Grid { ColumnSpacing = 12, Padding = new Thickness(0, 8, 0, 8) };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.Children.Add(new FontIcon { Glyph = "", FontSize = 20, Foreground = Brand.System("TextFillColorSecondaryBrush") });
        var words = new StackPanel { VerticalAlignment = VerticalAlignment.Center };
        words.Children.Add(new TextBlock { Text = device.Name, TextTrimming = TextTrimming.CharacterEllipsis });
        words.Children.Add(new TextBlock
        {
            Text = Strings.Text(DeviceText.Os(device.Os)),
            Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
            Foreground = Brand.System("TextFillColorSecondaryBrush"),
        });
        Grid.SetColumn(words, 1);
        grid.Children.Add(words);
        FrameworkElement action;
        if (device.Pairable)
        {
            var pair = new Button { Content = Strings.Get(TextKeys.PairingPair) };
            var id = device.Id;
            pair.Click += (_, _) => Start(() => agent.StartPairingAsync(id));
            action = pair;
        }
        else
        {
            action = new TextBlock
            {
                Text = Strings.Get(TextKeys.PairingNotAccepting),
                Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
                Foreground = Brand.System("TextFillColorSecondaryBrush"),
            };
        }
        action.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(action, 2);
        grid.Children.Add(action);
        var item = new ListViewItem { Content = grid };
        AutomationProperties.SetName(item, device.Name);
        return item;
    }

    private void PairByAddress()
    {
        var host = address.Text.Trim();
        if (host.Length != 0)
        {
            Start(() => agent.StartPairingByAddressAsync(host));
        }
    }

    private async void Start(Func<Task> request)
    {
        try
        {
            await request();
            startError.Visibility = Visibility.Collapsed;
            started = true;
            Show();
        }
        catch (AgentRefusal refusal)
        {
            ShowStartError(Strings.Text(PairingText.StartFailure(refusal.Code)));
        }
        catch (Exception e)
        {
            AppLog.Info($"start pairing: {e.Message}");
            ShowStartError(Strings.Get(TextKeys.TitleNotResponding));
        }
    }

    private void ShowStartError(string text)
    {
        startError.Text = text;
        startError.Visibility = Visibility.Visible;
    }

    private void Typed()
    {
        var clean = PairingCode.Clean(typed.Text);
        if (clean != typed.Text)
        {
            typed.Text = clean;
            typed.SelectionStart = clean.Length;
            return;
        }
        ShowDigits();
        if (clean.Length == PairingCode.Length)
        {
            Submit(clean);
        }
    }

    private void ShowDigits()
    {
        var text = typed.Text;
        bool focused = typed.FocusState != FocusState.Unfocused;
        for (int i = 0; i < PairingCode.Length; i++)
        {
            digits[i].Text = i < text.Length ? text[i].ToString() : "";
            bool current = focused && i == Math.Min(text.Length, PairingCode.Length - 1);
            boxes[i].BorderBrush = Brand.System(current ? "AccentFillColorDefaultBrush" : "ControlStrongStrokeColorDefaultBrush");
            boxes[i].BorderThickness = new Thickness(current ? 2 : 1);
        }
    }

    private async void Submit(string code)
    {
        if (submitting)
        {
            return;
        }
        submitting = true;
        try
        {
            var state = await agent.SubmitPairingCodeAsync(code);
            if (state.Phase == PairingPhase.EnterCode)
            {
                typed.Text = "";
                shakeStart = DateTime.UtcNow;
                if (new Windows.UI.ViewManagement.UISettings().AnimationsEnabled)
                {
                    shaker.Start();
                }
            }
        }
        catch (Exception e)
        {
            AppLog.Info($"pairing code: {e.Message}");
        }
        finally
        {
            submitting = false;
        }
    }
}
