using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;

namespace InputWeave.Main;

/// <summary>
/// What the arrangement and device pages show about one computer, and its
/// options: name, system, status with the
/// latency, its screens, how fast the cursor moves on it, whether this
/// computer shares its clipboard and files with it, swapping the modifier
/// keys, and removing it. Another computer's name is a field: the name it
/// has here only (RenameDevice), with the name it gives itself beneath once
/// they differ.
/// </summary>
internal sealed partial class DeviceInspector : UserControl
{
    private readonly TextBlock name = Value();
    private readonly Grid nameRow;
    private readonly StackPanel renameRow = new() { Padding = new Thickness(16, 12, 16, 12), Spacing = 6 };
    private readonly TextBox nameBox = new() { MaxLength = 64, HorizontalAlignment = HorizontalAlignment.Stretch };
    private readonly TextBlock nameHint = Ui.Secondary("");
    private bool nameRefused;
    /// <summary>The name last refused for the computer shown, not sent again as the field loses the focus.</summary>
    private string? refusedName;
    private readonly ToggleSwitch clipboard;
    private readonly ToggleSwitch files;
    private readonly StackPanel sharing = new();
    private readonly TextBlock system = Value();
    private readonly TextBlock status = Value();
    private readonly StackPanel screens = new();
    private readonly Border screensCard;
    private readonly TextBlock screensLater = Ui.Secondary(Strings.Get(TextKeys.InspectorScreensLater), caption: false);
    private readonly TextBlock swapTitle = new() { TextWrapping = TextWrapping.Wrap };
    private readonly ToggleSwitch swap;
    private readonly Border options;
    private readonly Border speedCard;
    private readonly Slider speedSlider = new()
    {
        Minimum = CursorSpeed.Minimum,
        Maximum = CursorSpeed.Maximum,
        StepFrequency = CursorSpeed.Step,
        SmallChange = CursorSpeed.Step,
        LargeChange = 50,
        SnapsTo = SliderSnapsTo.StepValues,
        IsThumbToolTipEnabled = false,
        HorizontalAlignment = HorizontalAlignment.Stretch,
    };
    private readonly TextBlock speedValue = new() { MinWidth = 40, TextAlignment = TextAlignment.Right, VerticalAlignment = VerticalAlignment.Center };
    private readonly HyperlinkButton speedReset = new() { Content = Strings.Get(TextKeys.InspectorCursorSpeedReset), Padding = new Thickness(6, 2, 6, 2) };
    private readonly Microsoft.UI.Xaml.Controls.Canvas speedTicks = new() { Height = 5, Margin = new Thickness(0, -2, 0, 2) };
    private readonly TextBlock speedHint = Ui.Secondary("");
    /// <summary>
    /// Each computer's speed under the hand and on its way, kept while
    /// another is shown, so coming back to one shows the speed still on its
    /// way there.
    /// </summary>
    private readonly CursorSpeedDrafts speedDrafts = new();
    /// <summary>Sends a speed chosen with the keyboard once the keys have stopped for a moment.</summary>
    private readonly DispatcherTimer speedTimer = new() { Interval = TimeSpan.FromMilliseconds(300) };
    /// <summary>The service's speed for the computer shown.</summary>
    private int speedService = CursorSpeed.Normal;
    private bool speedUpdating;
    private AgentSnapshot? model;
    private string id = "";
    private string shownScreens = "";

    public DeviceInspector()
    {
        swap = Ui.Switch("", false, on =>
        {
            var device = id;
            App.Instance.Send($"swap keys for {device}", () => App.Instance.Model.SetSwapGuiAndControlAsync(device, on));
        });
        clipboard = Ui.Switch(Strings.Get(TextKeys.InspectorSyncClipboard), true, on => SetSharing(clipboardOff: !on, filesOff: null));
        files = Ui.Switch(Strings.Get(TextKeys.InspectorSendFiles), true, on => SetSharing(clipboardOff: null, filesOff: !on));
        sharing.Children.Add(Ui.Card(
            Ui.Row(Strings.Get(TextKeys.InspectorSyncClipboard), null, clipboard),
            Ui.Row(Strings.Get(TextKeys.InspectorSendFiles), null, files)));
        var sharingHint = Ui.Secondary(Strings.Get(TextKeys.InspectorSharingHint));
        sharingHint.Margin = new Thickness(2, 6, 0, 0);
        sharing.Children.Add(sharingHint);
        sharing.Margin = new Thickness(0, 20, 0, 0);

        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(nameBox, Strings.Get(TextKeys.InspectorName));
        nameBox.KeyDown += (_, e) =>
        {
            if (e.Key == Windows.System.VirtualKey.Enter)
            {
                e.Handled = true;
                CommitName();
            }
        };
        nameBox.LostFocus += (_, _) => CommitName();
        renameRow.Children.Add(new TextBlock { Text = Strings.Get(TextKeys.InspectorName) });
        renameRow.Children.Add(nameBox);
        renameRow.Children.Add(nameHint);
        nameRow = Ui.Row(new TextBlock { Text = Strings.Get(TextKeys.InspectorName) }, name);
        screensCard = Ui.Card(screens);
        speedCard = BuildSpeedCard();
        var remove = new Button { Content = Strings.Get(TextKeys.RemoveDevice) };
        remove.Click += (_, _) => ConfirmRemoval();
        options = Ui.Card(Ui.Row(swapTitle, swap), Ui.Row(new TextBlock(), remove));

        var stack = new StackPanel { Padding = new Thickness(16, 0, 16, 16), Spacing = 0 };
        var names = new StackPanel();
        names.Children.Add(nameRow);
        names.Children.Add(renameRow);
        stack.Children.Add(Ui.Card(
            names,
            Ui.Row(new TextBlock { Text = Strings.Get(TextKeys.InspectorSystem) }, system),
            Ui.Row(new TextBlock { Text = Strings.Get(TextKeys.InspectorStatus) }, status)));
        stack.Children.Add(Ui.Header(Strings.Get(TextKeys.InspectorScreens)));
        stack.Children.Add(screensCard);
        stack.Children.Add(screensLater);
        speedCard.Margin = new Thickness(0, 20, 0, 0);
        stack.Children.Add(speedCard);
        stack.Children.Add(sharing);
        options.Margin = new Thickness(0, 20, 0, 0);
        stack.Children.Add(options);
        Content = new ScrollViewer { Content = stack, HorizontalScrollMode = ScrollMode.Disabled };
    }

    private static TextBlock Value() => new()
    {
        Foreground = Brand.System("TextFillColorSecondaryBrush"),
        TextWrapping = TextWrapping.Wrap,
        TextAlignment = TextAlignment.Right,
        IsTextSelectionEnabled = true,
        MaxWidth = 150,
    };

    public void Invalidate() => model = null;

    public void Update(AgentSnapshot snapshot, string deviceId)
    {
        if (ReferenceEquals(snapshot, model) && deviceId == id)
        {
            return;
        }
        bool otherDevice = deviceId != id;
        // A computer no longer trusted (unpaired) loses its draft, and a
        // speed waiting for it goes nowhere; paired again, it shows the
        // service's speed. Leaving another one, a speed chosen with the keys
        // for it goes at once, rather than being dropped.
        if (snapshot.Device(id) is null)
        {
            speedTimer.Stop();
        }
        else if (otherDevice)
        {
            FlushSpeed();
        }
        speedDrafts.Keep(snapshot.Status.Devices.Select(d => d.Id));
        model = snapshot;
        id = deviceId;
        var arranged = snapshot.Arranged(deviceId);
        var device = snapshot.Device(deviceId);
        name.Text = arranged?.Name ?? snapshot.NameOf(deviceId);
        // Only another computer this one trusts can be named here; this
        // one is renamed in General, and is not in the list.
        nameRow.Visibility = device is null ? Visibility.Visible : Visibility.Collapsed;
        renameRow.Visibility = device is null ? Visibility.Collapsed : Visibility.Visible;
        if (device is not null)
        {
            if (otherDevice)
            {
                nameRefused = false;
                refusedName = null;
            }
            // The field follows the service, except while the user types in it.
            if (otherDevice || nameBox.FocusState == FocusState.Unfocused)
            {
                nameBox.Text = device.Name;
            }
            nameBox.PlaceholderText = device.ReportedName;
            ShowNameHint();
        }
        sharing.Visibility = device is null ? Visibility.Collapsed : Visibility.Visible;
        if (device is not null)
        {
            Ui.Set(clipboard, !device.ClipboardOff);
            Ui.Set(files, !device.FilesOff);
        }
        var os = arranged?.Os ?? device?.Os ?? "";
        system.Text = Strings.Text(DeviceText.Os(os));
        status.Text = Strings.Text(DeviceText.Status(snapshot, deviceId));

        var list = arranged?.Screens.ToList() ?? [];
        var key = string.Join(";", list.Select(s => $"{DeviceText.Resolution(s)}{s.Primary}"));
        if (key != shownScreens)
        {
            shownScreens = key;
            screens.Children.Clear();
            foreach (var screen in list)
            {
                if (screens.Children.Count > 0)
                {
                    screens.Children.Add(Ui.Divider());
                }
                var main = Ui.Secondary(screen.Primary ? Strings.Get(TextKeys.ScreenMain) : "", caption: false);
                screens.Children.Add(Ui.Row(new TextBlock { Text = DeviceText.Resolution(screen) }, main));
            }
        }
        screensCard.Visibility = list.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
        screensLater.Visibility = list.Count == 0 ? Visibility.Visible : Visibility.Collapsed;

        // Another computer, from a service that knows cursor speeds.
        var speed = CursorSpeed.Shown(snapshot, deviceId);
        speedCard.Visibility = speed is null ? Visibility.Collapsed : Visibility.Visible;
        if (speed is { } service)
        {
            speedService = service;
            SpeedDraft.Settle(service);
            speedHint.Text = Strings.Format(TextKeys.InspectorCursorSpeedHint, snapshot.NameOf(deviceId));
            ShowSpeed();
        }

        // Only paired computers have options; this one and those in the configuration file do not.
        options.Visibility = device is { Paired: true } ? Visibility.Visible : Visibility.Collapsed;
        if (device is not null)
        {
            swapTitle.Text = Strings.Get(DeviceText.SwapKey(os));
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(swap, swapTitle.Text);
            Ui.Set(swap, device.SwapGuiAndControl);
        }
    }

    /// <summary>
    /// The cursor speed row: its title, Reset and the speed above a slider
    /// from 50% to 300% in steps of 10, marks at 50, 100, 200 and 300, and
    /// what it does beneath. A speed is sent as the slider is let go of, or
    /// a moment after the keys moved it.
    /// </summary>
    private Border BuildSpeedCard()
    {
        var title = new TextBlock { Text = Strings.Get(TextKeys.InspectorCursorSpeed), VerticalAlignment = VerticalAlignment.Center, TextWrapping = TextWrapping.Wrap };
        var header = new Grid { ColumnSpacing = 4 };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.Children.Add(title);
        Grid.SetColumn(speedReset, 1);
        header.Children.Add(speedReset);
        Grid.SetColumn(speedValue, 2);
        header.Children.Add(speedValue);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(speedSlider, title.Text);

        for (int i = 0; i < CursorSpeed.Ticks.Count; i++)
        {
            speedTicks.Children.Add(new Microsoft.UI.Xaml.Shapes.Rectangle
            {
                Width = 1,
                Height = 5,
                Fill = Brand.System("TextFillColorSecondaryBrush"),
            });
        }
        speedTicks.SizeChanged += (_, _) => PlaceTicks();

        speedSlider.ValueChanged += (_, e) =>
        {
            if (speedUpdating)
            {
                return;
            }
            SpeedDraft.Moved(e.NewValue, speedService);
            ShowSpeed();
            if (!SpeedDraft.Editing)
            {
                speedTimer.Stop();
                speedTimer.Start();
            }
        };
        speedSlider.AddHandler(PointerPressedEvent, new PointerEventHandler((_, _) => SpeedDraft.Began()), true);
        speedSlider.AddHandler(PointerReleasedEvent, new PointerEventHandler((_, _) => EndSpeedDrag()), true);
        // The capture moves between the slider and its thumb while the
        // button stays down; only a capture lost with the button up (as when
        // the window lost it) ends the drag.
        speedSlider.AddHandler(PointerCaptureLostEvent, new PointerEventHandler((_, e) =>
        {
            if (!e.Pointer.IsInContact)
            {
                EndSpeedDrag();
            }
        }), true);
        speedSlider.AddHandler(PointerCanceledEvent, new PointerEventHandler((_, _) => EndSpeedDrag()), true);
        speedTimer.Tick += (_, _) =>
        {
            speedTimer.Stop();
            if (SpeedDraft.Pending(speedService) is { } percent)
            {
                SendSpeed(id, percent);
            }
        };
        speedReset.Click += (_, _) =>
        {
            speedTimer.Stop();
            SpeedDraft.Moved(CursorSpeed.Normal, speedService);
            if (SpeedDraft.Pending(speedService) is { } percent)
            {
                SendSpeed(id, percent);
            }
            ShowSpeed();
        };

        var row = new StackPanel { Padding = new Thickness(16, 12, 16, 12), Spacing = 4 };
        row.Children.Add(header);
        row.Children.Add(speedSlider);
        row.Children.Add(speedTicks);
        row.Children.Add(speedHint);
        return Ui.Card(row);
    }

    /// <summary>Marks 50%, 100%, 200% and 300% where the slider's thumb stands at them.</summary>
    private void PlaceTicks()
    {
        // The thumb's middle keeps half its width from either end of the track.
        const double inset = 10;
        var usable = Math.Max(speedTicks.ActualWidth - 2 * inset, 0);
        for (int i = 0; i < CursorSpeed.Ticks.Count; i++)
        {
            Microsoft.UI.Xaml.Controls.Canvas.SetLeft(speedTicks.Children[i], inset + usable * CursorSpeed.Fraction(CursorSpeed.Ticks[i]) - 0.5);
        }
    }

    /// <summary>The speed under the hand, or else the service's, beside and on the slider.</summary>
    private void ShowSpeed()
    {
        var percent = SpeedDraft.Shown(speedService);
        speedValue.Text = CursorSpeed.Label(percent);
        speedReset.Visibility = CursorSpeed.ShowsReset(percent) ? Visibility.Visible : Visibility.Collapsed;
        if ((int)Math.Round(speedSlider.Value) != percent)
        {
            speedUpdating = true;
            speedSlider.Value = percent;
            speedUpdating = false;
        }
    }

    /// <summary>The pointer let go of the slider: sends the speed it was let go at.</summary>
    private void EndSpeedDrag()
    {
        if (!SpeedDraft.Editing)
        {
            return;
        }
        if (SpeedDraft.Ended(speedService) is { } percent)
        {
            speedTimer.Stop();
            SendSpeed(id, percent);
        }
        ShowSpeed();
    }

    /// <summary>
    /// Sends what was chosen for the computer shown and not sent yet, now:
    /// a speed still waiting out its delay, or one the slider is still held
    /// at, as if let go.
    /// </summary>
    private void FlushSpeed()
    {
        var waiting = speedTimer.IsEnabled;
        speedTimer.Stop();
        if ((waiting || SpeedDraft.Editing) && SpeedDraft.Ended(speedService) is { } percent)
        {
            SendSpeed(id, percent);
        }
    }

    /// <summary>The draft of the computer shown.</summary>
    private CursorSpeedDraft SpeedDraft => DraftFor(id);

    private CursorSpeedDraft DraftFor(string deviceId) => speedDrafts.For(deviceId);

    /// <summary>
    /// Sends <paramref name="percent"/> for <paramref name="target"/>; once
    /// the service answers the latest send, that computer's slider shows the
    /// service's speed again, the one sent or, refused, the one it had.
    /// </summary>
    private async void SendSpeed(string target, int percent)
    {
        var draft = DraftFor(target);
        var number = draft.Sending(percent);
        try
        {
            await App.Instance.Model.SetDeviceSpeedAsync(target, percent);
        }
        catch (Exception e)
        {
            AppLog.Info($"cursor speed {percent} for {target}: {e.Message}");
        }
        draft.Answered(number);
        if (target == id)
        {
            if (App.Instance.Model.Snapshot.Device(target) is { SpeedPercent: > 0 } device)
            {
                speedService = (int)device.SpeedPercent;
            }
            ShowSpeed();
        }
    }

    /// <summary>Puts the cursor in the name field of <paramref name="deviceId"/>, when it is the one shown ("Rename…" on the canvas).</summary>
    public void FocusName(string deviceId)
    {
        if (deviceId != id || renameRow.Visibility != Visibility.Visible)
        {
            return;
        }
        // Once the page has laid out the field.
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
        {
            if (deviceId == id && nameBox.Focus(FocusState.Programmatic))
            {
                nameBox.SelectAll();
            }
        });
    }

    /// <summary>
    /// Sends the name typed, unless it is the one shown already. Emptied,
    /// the computer's own name shows again.
    /// </summary>
    private async void CommitName()
    {
        if (model?.Device(id) is not { } device)
        {
            return;
        }
        var wanted = nameBox.Text.Trim();
        if (wanted == device.Name)
        {
            nameBox.Text = device.Name;
            nameRefused = false;
            ShowNameHint();
            return;
        }
        if (nameRefused && wanted == refusedName)
        {
            return;
        }
        var target = id;
        try
        {
            await App.Instance.Model.RenameDeviceAsync(target, wanted);
            nameRefused = false;
            if (target == id && App.Instance.Model.Snapshot.Device(target) is { } renamed && nameBox.FocusState == FocusState.Unfocused)
            {
                nameBox.Text = renamed.Name;
            }
        }
        catch (Exception e)
        {
            AppLog.Info($"rename {target}: {e.Message}");
            nameRefused = target == id;
            refusedName = wanted;
        }
        ShowNameHint();
    }

    /// <summary>Under the field: why the name was refused, or the name the computer gives itself when it has another here.</summary>
    private void ShowNameHint()
    {
        var reported = model?.ReportedName(id);
        if (nameRefused)
        {
            nameHint.Text = Strings.Get(TextKeys.NameInvalid);
            nameHint.Foreground = Brand.System("SystemFillColorCriticalBrush");
        }
        else
        {
            nameHint.Text = reported is null ? "" : Strings.Format(TextKeys.InspectorCallsItself, reported);
            nameHint.Foreground = Brand.System("TextFillColorSecondaryBrush");
        }
        nameHint.Visibility = nameHint.Text.Length == 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>Shares the clipboard or files with the computer shown, or stops; the other stays as it is.</summary>
    private void SetSharing(bool? clipboardOff, bool? filesOff)
    {
        if (model?.Device(id) is not { } device)
        {
            return;
        }
        var target = id;
        var clipboardIsOff = clipboardOff ?? device.ClipboardOff;
        var filesAreOff = filesOff ?? device.FilesOff;
        App.Instance.Send(
            $"sharing with {target}: clipboard off {clipboardIsOff}, files off {filesAreOff}",
            () => App.Instance.Model.SetDeviceSharingAsync(target, clipboardIsOff, filesAreOff));
    }

    /// <summary>Asks before the computer stops trusting the other one.</summary>
    private async void ConfirmRemoval()
    {
        if (model is null || XamlRoot is null)
        {
            return;
        }
        var device = id;
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = Strings.Format(TextKeys.RemoveDeviceTitle, model.NameOf(device)),
            Content = Strings.Get(TextKeys.RemoveDeviceMessage),
            PrimaryButtonText = Strings.Get(TextKeys.RemoveDeviceConfirm),
            CloseButtonText = Strings.Get(TextKeys.Cancel),
            DefaultButton = ContentDialogButton.Close,
        };
        try
        {
            if (await dialog.ShowAsync() == ContentDialogResult.Primary)
            {
                App.Instance.Send($"unpair {device}", () => App.Instance.Model.UnpairAsync(device));
            }
        }
        catch (Exception e)
        {
            AppLog.Info($"remove device: {e.Message}");
        }
    }
}
