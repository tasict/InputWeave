using System.Globalization;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Licensing;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Input;

namespace InputWeave.Main;

/// <summary>
/// What the arrangement and device pages show about one computer, and its
/// options: name, system, status with the
/// latency (offline, Wake while it can be woken, with a spinner while it is
/// being woken and a note under it when it didn't wake up or can't be
/// woken yet; offline for now, with who took it offline under it and, while
/// it runs InputWeave 1.0 itself, a warning for each other computer on 1.0
/// that still controls it; not in the profile in use, with no Wake and no
/// Take Offline), its screens, how fast
/// the cursor moves on it, whether this
/// computer shares its clipboard and files with it, swapping the modifier
/// keys, taking it offline for now or bringing it back, and removing it.
/// Another computer's name is a field: the name it
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
    /// <summary>Before the status while the computer is being woken; the status says so in words.</summary>
    private readonly ProgressRing waking = new()
    {
        Width = 16,
        Height = 16,
        Margin = new Thickness(0, 0, 6, 0),
        VerticalAlignment = VerticalAlignment.Center,
    };
    /// <summary>Before the status while the computer can be woken; the status says so in words.</summary>
    private readonly FontIcon moon = new()
    {
        Glyph = Wake.MoonGlyph,
        FontSize = 14,
        Margin = new Thickness(0, 0, 6, 0),
        VerticalAlignment = VerticalAlignment.Center,
    };
    /// <summary>Before the status while the computer is offline for now; the status says so in words.</summary>
    private readonly FontIcon offlineIcon = new()
    {
        Glyph = OfflineForNow.Glyph,
        FontSize = 14,
        Margin = new Thickness(0, 0, 6, 0),
        VerticalAlignment = VerticalAlignment.Center,
    };
    /// <summary>Before the status while the profile in use doesn't connect the computer; the status says so in words.</summary>
    private readonly FontIcon profileIcon = new()
    {
        Glyph = Profiles.Glyph,
        FontSize = 14,
        Margin = new Thickness(0, 0, 6, 0),
        VerticalAlignment = VerticalAlignment.Center,
    };
    /// <summary>Under the status while the computer is offline for now: who took it offline, and the warnings.</summary>
    private readonly StackPanel offlineNotes = new() { Padding = new Thickness(16, 0, 16, 12), Spacing = 6, Visibility = Visibility.Collapsed };
    /// <summary>The notes shown, to build them again only when they change.</summary>
    private string shownOfflineNotes = "";
    /// <summary>Take Offline or Bring Back Online, in a group of its own with what it does under it.</summary>
    private readonly StackPanel offlineGroup = new() { Margin = new Thickness(0, 20, 0, 0), Visibility = Visibility.Collapsed };
    private readonly Button offlineButton;
    private readonly Button wake;
    /// <summary>Under the status: why the computer didn't wake up, or that it can't be woken yet.</summary>
    private readonly Grid wakeNote = new() { Padding = new Thickness(16, 0, 16, 12) };
    private readonly FontIcon wakeWarning = new()
    {
        Glyph = Wake.WarningGlyph,
        FontSize = 14,
        Foreground = Brand.System("SystemFillColorCautionBrush"),
        VerticalAlignment = VerticalAlignment.Top,
        Margin = new Thickness(0, 1, 8, 0),
    };
    private readonly TextBlock wakeNoteText = new() { Style = Ui.Style("CaptionTextBlockStyle"), TextWrapping = TextWrapping.Wrap };
    /// <summary>The note shown, to build it again only when it changes.</summary>
    private string shownNote = "";
    private readonly StackPanel screens = new();
    private readonly Border screensCard;
    private readonly TextBlock screensLater = Ui.Secondary(Strings.Get(TextKeys.InspectorScreensLater), caption: false);
    private readonly TextBlock swapTitle = new() { TextWrapping = TextWrapping.Wrap };
    private readonly ToggleSwitch swap;
    private readonly Border options;
    private readonly Border removal;
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
        wake = Ui.Button(Strings.Get(TextKeys.WakeAction), SendWake, accent: true);
        wake.HorizontalAlignment = HorizontalAlignment.Right;
        wake.Margin = new Thickness(0, 8, 0, 0);
        AutomationProperties.SetAccessibilityView(waking, AccessibilityView.Raw);
        AutomationProperties.SetAccessibilityView(moon, AccessibilityView.Raw);
        AutomationProperties.SetAccessibilityView(wakeWarning, AccessibilityView.Raw);
        AutomationProperties.SetAccessibilityView(offlineIcon, AccessibilityView.Raw);
        AutomationProperties.SetAccessibilityView(profileIcon, AccessibilityView.Raw);
        var statusLine = new StackPanel { Orientation = Orientation.Horizontal, HorizontalAlignment = HorizontalAlignment.Right };
        statusLine.Children.Add(waking);
        statusLine.Children.Add(moon);
        statusLine.Children.Add(offlineIcon);
        statusLine.Children.Add(profileIcon);
        statusLine.Children.Add(status);
        var statusValue = new StackPanel();
        statusValue.Children.Add(statusLine);
        statusValue.Children.Add(wake);
        wakeNote.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        wakeNote.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        wakeNote.Children.Add(wakeWarning);
        Grid.SetColumn(wakeNoteText, 1);
        wakeNote.Children.Add(wakeNoteText);
        var statusRow = new StackPanel();
        statusRow.Children.Add(Ui.Row(new TextBlock { Text = Strings.Get(TextKeys.InspectorStatus) }, statusValue));
        statusRow.Children.Add(offlineNotes);
        statusRow.Children.Add(wakeNote);
        screensCard = Ui.Card(screens);
        speedCard = BuildSpeedCard();
        offlineButton = Ui.Button(Strings.Get(TextKeys.OfflineTake), ToggleOffline);
        offlineButton.HorizontalAlignment = HorizontalAlignment.Left;
        offlineGroup.Children.Add(offlineButton);
        var offlineHint = Ui.Secondary(Strings.Get(TextKeys.OfflineHint));
        offlineHint.Margin = new Thickness(2, 6, 0, 0);
        offlineGroup.Children.Add(offlineHint);
        var remove = new Button { Content = Strings.Get(TextKeys.RemoveDevice) };
        remove.Click += (_, _) => ConfirmRemoval();
        // Taking the computer offline for now comes between the swap, the
        // last of its switches, and removing it.
        options = Ui.Card(Ui.Row(swapTitle, swap));
        removal = Ui.Card(Ui.Row(new TextBlock(), remove));

        var stack = new StackPanel { Padding = new Thickness(16, 0, 16, 16), Spacing = 0 };
        var names = new StackPanel();
        names.Children.Add(nameRow);
        names.Children.Add(renameRow);
        stack.Children.Add(Ui.Card(
            names,
            Ui.Row(new TextBlock { Text = Strings.Get(TextKeys.InspectorSystem) }, system),
            statusRow));
        stack.Children.Add(Ui.Header(Strings.Get(TextKeys.InspectorScreens)));
        stack.Children.Add(screensCard);
        stack.Children.Add(screensLater);
        speedCard.Margin = new Thickness(0, 20, 0, 0);
        stack.Children.Add(speedCard);
        stack.Children.Add(sharing);
        options.Margin = new Thickness(0, 20, 0, 0);
        stack.Children.Add(options);
        stack.Children.Add(offlineGroup);
        removal.Margin = new Thickness(0, 20, 0, 0);
        stack.Children.Add(removal);
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

    /// <summary>Shows <paramref name="deviceId"/> as <paramref name="snapshot"/> has it, as of <paramref name="now"/>.</summary>
    public void Update(AgentSnapshot snapshot, string deviceId, DateTimeOffset now)
    {
        if (ReferenceEquals(snapshot, model) && deviceId == id)
        {
            // Since when an offline computer is offline moves on.
            ShowStatus(now);
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
        ShowStatus(now);
        ShowWake();
        ShowOffline();

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
        removal.Visibility = options.Visibility;
        if (device is not null)
        {
            swapTitle.Text = Strings.Get(DeviceText.SwapKey(os));
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(swap, swapTitle.Text);
            Ui.Set(swap, device.SwapGuiAndControl);
        }
    }

    /// <summary>The status of the computer shown, as of <paramref name="now"/>.</summary>
    private void ShowStatus(DateTimeOffset now)
    {
        if (model is null)
        {
            return;
        }
        var text = Strings.Text(DeviceText.Status(model, id, now, CultureInfo.CurrentCulture));
        if (status.Text != text)
        {
            status.Text = text;
        }
    }

    /// <summary>
    /// Waking the computer shown: the spinner while it is being woken, else
    /// the moon while it can be woken, Wake
    /// while it can be (not while a wake-up is on its way), and the note
    /// under the status.
    /// </summary>
    private void ShowWake()
    {
        if (model is null)
        {
            return;
        }
        bool spinning = Wake.Waking(model, id);
        waking.IsActive = spinning;
        waking.Visibility = spinning ? Visibility.Visible : Visibility.Collapsed;
        moon.Visibility = Wake.MoonBeforeStatus(model, id) ? Visibility.Visible : Visibility.Collapsed;
        wake.Visibility = Wake.Offered(model, id) ? Visibility.Visible : Visibility.Collapsed;
        wake.IsEnabled = Wake.CanSend(model, id);

        var note = Wake.Note(model, id);
        var words = Wake.NoteText(model, id) is { } text ? Strings.Text(text) : "";
        var key = $"{note}|{words}";
        if (key == shownNote)
        {
            return;
        }
        shownNote = key;
        wakeNote.Visibility = note == WakeNote.None ? Visibility.Collapsed : Visibility.Visible;
        wakeWarning.Visibility = note == WakeNote.NotWoken ? Visibility.Visible : Visibility.Collapsed;
        wakeNoteText.Foreground = Brand.System(note == WakeNote.NotWoken ? "TextFillColorPrimaryBrush" : "TextFillColorSecondaryBrush");
        wakeNoteText.Inlines.Clear();
        wakeNoteText.Inlines.Add(new Run { Text = words });
        // Not woken: how to turn on wake for network access, in the user
        // guide in the app's language.
        if (note == WakeNote.NotWoken && Uri.TryCreate(Strings.Get(TextKeys.WakeHowToUrl), UriKind.Absolute, out var guide))
        {
            var link = new Hyperlink { NavigateUri = guide };
            link.Inlines.Add(new Run { Text = Strings.Get(TextKeys.WakeHowTo) });
            wakeNoteText.Inlines.Add(new Run { Text = " " });
            wakeNoteText.Inlines.Add(link);
        }
    }

    /// <summary>Wake: asks the service to wake the computer shown.</summary>
    private void SendWake() => App.Instance.WakeDevice(id);

    /// <summary>
    /// Being offline for now: the glyph before the status, who took the
    /// computer offline and the warnings under it, and the button that takes
    /// it offline or brings it back, for another computer while this one
    /// is not offline for now itself.
    /// </summary>
    private void ShowOffline()
    {
        if (model is null)
        {
            return;
        }
        bool offline = OfflineForNow.IsOffline(model, id);
        offlineIcon.Visibility = offline ? Visibility.Visible : Visibility.Collapsed;
        profileIcon.Visibility = Profiles.NotInProfile(model, id) ? Visibility.Visible : Visibility.Collapsed;
        offlineGroup.Visibility = OfflineForNow.Offered(model, id) ? Visibility.Visible : Visibility.Collapsed;
        offlineButton.Content = Strings.Get(OfflineForNow.ActionKey(model, id));

        var setBy = OfflineForNow.SetBy(model, id) is { } by ? Strings.Text(by) : null;
        var warnings = OfflineForNow.Unaware(model, id).Select(Strings.Text).ToList();
        var key = $"{setBy}|{string.Join("|", warnings)}";
        if (key == shownOfflineNotes)
        {
            return;
        }
        shownOfflineNotes = key;
        offlineNotes.Children.Clear();
        if (setBy is not null)
        {
            offlineNotes.Children.Add(Ui.Secondary(setBy));
        }
        // One line for each computer that still controls it, with the
        // warning sign, as a note that a computer didn't wake up has.
        foreach (var warning in warnings)
        {
            var line = new Grid();
            line.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            line.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            var sign = new FontIcon
            {
                Glyph = Wake.WarningGlyph,
                FontSize = 14,
                Foreground = Brand.System("SystemFillColorCautionBrush"),
                VerticalAlignment = VerticalAlignment.Top,
                Margin = new Thickness(0, 1, 8, 0),
            };
            AutomationProperties.SetAccessibilityView(sign, AccessibilityView.Raw);
            line.Children.Add(sign);
            var text = new TextBlock { Text = warning, Style = Ui.Style("CaptionTextBlockStyle"), TextWrapping = TextWrapping.Wrap };
            Grid.SetColumn(text, 1);
            line.Children.Add(text);
            offlineNotes.Children.Add(line);
        }
        offlineNotes.Visibility = offlineNotes.Children.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
    }

    /// <summary>Take Offline or Bring Back Online: asks the service, without asking the user first.</summary>
    private void ToggleOffline()
    {
        if (model is not null && OfflineForNow.Offered(model, id))
        {
            App.Instance.SetOfflineForNow(id, !OfflineForNow.IsOffline(model, id));
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

    /// <summary>
    /// Asks before the computer stops trusting the other one. While this
    /// computer is licensed and the other one is on its license, it also
    /// offers to revoke the license there ("Also revoke its license",
    /// checked), or, for one on InputWeave 1.0, says that it keeps its
    /// place. Revoking keeps the dialog open, with progress, until Polar has
    /// answered; the computer is removed whatever came of it, and a license
    /// that could not be revoked is said afterwards.
    /// </summary>
    private async void ConfirmRemoval()
    {
        if (model is null || XamlRoot is null)
        {
            return;
        }
        var device = id;
        var name = model.NameOf(device);
        var root = XamlRoot;
        var license = model.Status.License;
        var worker = App.Instance.License;
        bool licensed = LicenseView.For(license, model.Status.ThisDeviceName, worker.Withdrawing(license)).IsLicensed;
        var content = new StackPanel { Spacing = 12 };
        content.Children.Add(new TextBlock { Text = Strings.Get(TextKeys.RemoveDeviceMessage), TextWrapping = TextWrapping.Wrap });
        CheckBox? revoke = null;
        switch (LicenseView.OnRemoval(license, licensed, device))
        {
            case RemovalOffer.Legacy:
                content.Children.Add(Ui.Secondary(Strings.Format(TextKeys.RemoveDeviceLegacy, name), caption: false));
                break;
            case RemovalOffer.Revoke:
                revoke = new CheckBox { Content = Strings.Get(TextKeys.RemoveDeviceRevoke), IsChecked = true };
                var note = Ui.Secondary(Strings.Get(TextKeys.RemoveDeviceRevokeNote));
                // Under the box's words, not under the box.
                note.Margin = new Thickness(28, -4, 0, 0);
                var choice = new StackPanel();
                choice.Children.Add(revoke);
                choice.Children.Add(note);
                content.Children.Add(choice);
                break;
        }
        var progress = new ProgressBar { IsIndeterminate = true, Visibility = Visibility.Collapsed };
        content.Children.Add(progress);
        var dialog = new ContentDialog
        {
            XamlRoot = root,
            Title = Strings.Format(TextKeys.RemoveDeviceTitle, name),
            Content = content,
            PrimaryButtonText = Strings.Get(TextKeys.RemoveDeviceConfirm),
            CloseButtonText = Strings.Get(TextKeys.Cancel),
            DefaultButton = ContentDialogButton.Close,
        };
        var notice = RemovalNotice.None;
        bool revoking = false;
        dialog.PrimaryButtonClick += async (_, args) =>
        {
            if (revoke?.IsChecked != true)
            {
                return;
            }
            revoking = true;
            var deferral = args.GetDeferral();
            dialog.IsPrimaryButtonEnabled = false;
            revoke.IsEnabled = false;
            progress.Visibility = Visibility.Visible;
            try
            {
                notice = await worker.RemoveDeviceAsync(device, revoke: true, () => App.Instance.Model.UnpairAsync(device));
            }
            catch (Exception e)
            {
                AppLog.Info($"remove device {device}: {e.Message}");
            }
            deferral.Complete();
        };
        try
        {
            if (await dialog.ShowAsync() != ContentDialogResult.Primary)
            {
                return;
            }
            if (!revoking)
            {
                App.Instance.Send($"unpair {device}", () => App.Instance.Model.UnpairAsync(device));
                return;
            }
            App.Instance.RefreshNow();
            if (notice == RemovalNotice.NotTold)
            {
                // Its place is free already: nothing to do in the customer portal.
                await new ContentDialog
                {
                    XamlRoot = root,
                    Title = Strings.Format(TextKeys.RemoveDeviceNotTold, name),
                    Content = new TextBlock { Text = Strings.Get(TextKeys.RemoveDeviceNotToldBody), TextWrapping = TextWrapping.Wrap },
                    CloseButtonText = Strings.Get(TextKeys.Ok),
                    DefaultButton = ContentDialogButton.Close,
                }.ShowAsync();
            }
            else if (notice != RemovalNotice.None)
            {
                var said = new ContentDialog
                {
                    XamlRoot = root,
                    Title = Strings.Format(TextKeys.RemoveDeviceNotRevoked, name),
                    Content = new TextBlock
                    {
                        Text = Strings.Get(notice == RemovalNotice.Offline ? TextKeys.RemoveDeviceNotRevokedOffline : TextKeys.RemoveDeviceNotRevokedBody),
                        TextWrapping = TextWrapping.Wrap,
                    },
                    PrimaryButtonText = Strings.Get(TextKeys.LicenseManage),
                    CloseButtonText = Strings.Get(TextKeys.Ok),
                    DefaultButton = ContentDialogButton.Close,
                };
                if (await said.ShowAsync() == ContentDialogResult.Primary)
                {
                    LicensePage.Open(PolarClient.PortalUrl);
                }
            }
        }
        catch (Exception e)
        {
            AppLog.Info($"remove device: {e.Message}");
        }
    }
}
