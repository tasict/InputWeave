using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using Microsoft.UI.Input;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.System;
using Windows.UI.Core;

namespace InputWeave.Main;

/// <summary>
/// General › Shortcuts: each hotkey with a
/// Record button, which turns its keys into a box that takes the next
/// combination pressed in this window; the jumps and relative mode also
/// have a switch, and Reset Shortcuts goes back to the defaults. A service
/// from before relative mode has no row for it. The background service checks
/// what is recorded and says what is wrong; this only shows its answer.
/// </summary>
/// <remarks>
/// The box hears only what Windows gives this window, as on the Mac: no
/// keyboard hook. Combinations the shell keeps for itself (Win + L, Win
/// with a digit) never reach it, and the service acts on its own hotkeys
/// as usual while one is recorded.
/// </remarks>
internal sealed class ShortcutsSection
{
    private readonly Dictionary<ShortcutRow, RowView> rows = [];
    /// <summary>The switches of the rows that can be turned off.</summary>
    private readonly Dictionary<ShortcutRow, ToggleSwitch> switches = [];
    /// <summary>The relative mode row and the divider above it, shown when the service has relative mode.</summary>
    private readonly UIElement relativeDivider = Ui.Divider();
    private readonly Grid relativeRow;
    private readonly Button reset;
    private readonly TextBlock problem = new()
    {
        TextWrapping = TextWrapping.Wrap,
        Margin = new Thickness(2, 8, 0, 0),
        Visibility = Visibility.Collapsed,
    };
    private AgentSnapshot? model;
    private ShortcutRow? recording;

    private sealed record RowView(TextBlock Keys, ContentControl Capture, TextBlock Prompt, Button Record);

    public ShortcutsSection()
    {
        problem.Foreground = Brand.System("SystemFillColorCriticalBrush");
        AutomationProperties.SetLiveSetting(problem, Microsoft.UI.Xaml.Automation.Peers.AutomationLiveSetting.Assertive);
        foreach (var row in Enum.GetValues<ShortcutRow>().Where(ShortcutText.CanTurnOff))
        {
            switches[row] = Ui.Switch(Strings.Get(ShortcutText.Title(row)), true, on =>
            {
                Stop();
                Set(row, on ? ShortcutText.Get(Shortcut.Defaults, row) : "");
            });
        }
        reset = Ui.Button(Strings.Get(TextKeys.ShortcutReset), () =>
        {
            Stop();
            Apply(Shortcut.Defaults, null);
        });
        reset.HorizontalAlignment = HorizontalAlignment.Right;

        var card = new StackPanel();
        foreach (var row in Enum.GetValues<ShortcutRow>().Where(r => r != ShortcutRow.Relative))
        {
            if (card.Children.Count > 0)
            {
                card.Children.Add(Ui.Divider());
            }
            card.Children.Add(Row(row));
        }
        relativeRow = Row(ShortcutRow.Relative);
        card.Children.Add(relativeDivider);
        card.Children.Add(relativeRow);
        card.Children.Add(Ui.Divider());
        var hint = Ui.Secondary(Strings.Get(TextKeys.ShortcutsHint));
        var footer = new Grid { Padding = new Thickness(16, 12, 16, 12), ColumnSpacing = 16 };
        footer.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        footer.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        hint.VerticalAlignment = VerticalAlignment.Center;
        footer.Children.Add(hint);
        reset.VerticalAlignment = VerticalAlignment.Center;
        Grid.SetColumn(reset, 1);
        footer.Children.Add(reset);
        card.Children.Add(footer);

        View = new StackPanel();
        View.Children.Add(Ui.Card(card));
        View.Children.Add(problem);
    }

    /// <summary>The card and, below it, what the service said was wrong.</summary>
    public StackPanel View { get; }

    private Grid Row(ShortcutRow row)
    {
        var keys = new TextBlock
        {
            Foreground = Brand.System("TextFillColorSecondaryBrush"),
            VerticalAlignment = VerticalAlignment.Center,
            TextAlignment = TextAlignment.Right,
        };
        var prompt = new TextBlock { Text = Strings.Get(ShortcutText.Prompt(row)), VerticalAlignment = VerticalAlignment.Center };
        // The box that takes the keys while recording: focused, it hears
        // every key before anything else in the window acts on it.
        var capture = new ContentControl
        {
            Content = new Border
            {
                Child = prompt,
                Padding = new Thickness(10, 4, 10, 5),
                CornerRadius = new CornerRadius(4),
                BorderThickness = new Thickness(1),
                BorderBrush = Brand.System("AccentFillColorDefaultBrush"),
                Background = Brand.System("ControlFillColorInputActiveBrush"),
            },
            IsTabStop = true,
            UseSystemFocusVisuals = true,
            Visibility = Visibility.Collapsed,
            VerticalAlignment = VerticalAlignment.Center,
        };
        AutomationProperties.SetName(capture, $"{Strings.Get(ShortcutText.Title(row))}: {prompt.Text}");
        capture.PreviewKeyDown += (_, e) => Pressed(row, e);
        // Clicking elsewhere, or leaving the window, cancels; the Record
        // and Cancel buttons do not take the focus, so they do not.
        capture.LostFocus += (_, _) =>
        {
            if (recording == row)
            {
                Stop();
            }
        };
        var record = new Button { Content = Strings.Get(TextKeys.ShortcutRecord), AllowFocusOnInteraction = false, MinWidth = 80 };
        AutomationProperties.SetName(record, $"{Strings.Get(TextKeys.ShortcutRecord)}: {Strings.Get(ShortcutText.Title(row))}");
        record.Click += (_, _) =>
        {
            if (recording == row)
            {
                Stop();
            }
            else
            {
                Start(row);
            }
        };
        var controls = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        controls.Children.Add(keys);
        controls.Children.Add(capture);
        controls.Children.Add(record);
        if (switches.TryGetValue(row, out var turnOff))
        {
            controls.Children.Add(turnOff);
        }
        rows[row] = new RowView(keys, capture, prompt, record);
        return Ui.Row(Strings.Get(ShortcutText.Title(row)), null, controls);
    }

    /// <summary>Shows the hotkeys the service reports now.</summary>
    public void Update(AgentSnapshot snapshot)
    {
        model = snapshot;
        var connected = snapshot.Link == AgentLink.Connected;
        var hotkeys = snapshot.Hotkeys;
        foreach (var (row, view) in rows)
        {
            view.Keys.Text = Strings.Text(ShortcutText.Shown(hotkeys, row));
            view.Record.IsEnabled = connected;
        }
        foreach (var (row, turnOff) in switches)
        {
            Ui.Set(turnOff, ShortcutText.Get(hotkeys, row).Length != 0);
            turnOff.IsEnabled = connected;
        }
        var relative = snapshot.RelativeSupported;
        relativeDivider.Visibility = relativeRow.Visibility = relative ? Visibility.Visible : Visibility.Collapsed;
        if (!relative && recording == ShortcutRow.Relative)
        {
            Stop();
        }
        reset.IsEnabled = connected && !ShortcutText.IsDefault(hotkeys, relative);
        if (!connected)
        {
            Stop();
        }
    }

    private void Start(ShortcutRow row)
    {
        Stop();
        recording = row;
        problem.Visibility = Visibility.Collapsed;
        var view = rows[row];
        view.Keys.Visibility = Visibility.Collapsed;
        view.Capture.Visibility = Visibility.Visible;
        view.Record.Content = Strings.Get(TextKeys.Cancel);
        AutomationProperties.SetName(view.Record, $"{Strings.Get(TextKeys.Cancel)}: {Strings.Get(ShortcutText.Title(row))}");
        // Once it is shown, it can take the focus.
        view.Capture.DispatcherQueue.TryEnqueue(() =>
        {
            if (recording == row)
            {
                view.Capture.Focus(FocusState.Programmatic);
            }
        });
    }

    /// <summary>Stops recording, if a row is; its keys show again.</summary>
    public void Stop()
    {
        if (recording is not { } row)
        {
            return;
        }
        recording = null;
        var view = rows[row];
        view.Capture.Visibility = Visibility.Collapsed;
        view.Keys.Visibility = Visibility.Visible;
        view.Record.Content = Strings.Get(TextKeys.ShortcutRecord);
        AutomationProperties.SetName(view.Record, $"{Strings.Get(TextKeys.ShortcutRecord)}: {Strings.Get(ShortcutText.Title(row))}");
    }

    /// <summary>A key pressed while <paramref name="row"/> records.</summary>
    private void Pressed(ShortcutRow row, KeyRoutedEventArgs e)
    {
        if (recording != row)
        {
            return;
        }
        // Every key goes to the recording: Tab does not move the focus, Esc
        // closes nothing, Alt opens no menu.
        e.Handled = true;
        if (IsModifier(e.Key))
        {
            return;
        }
        var key = ShortcutText.KeyName(e.KeyStatus.ScanCode, e.KeyStatus.IsExtendedKey);
        var held = new HeldModifiers(
            Control: Down(VirtualKey.Control),
            Alt: Down(VirtualKey.Menu) || e.KeyStatus.IsMenuKeyDown,
            Shift: Down(VirtualKey.Shift),
            Windows: Down(VirtualKey.LeftWindows) || Down(VirtualKey.RightWindows));
        var recorded = ShortcutText.Record(row, key, held);
        switch (recorded.Step)
        {
            case RecordStep.Wait:
                return;
            case RecordStep.Cancel:
                Stop();
                return;
            case RecordStep.Unusable:
                Stop();
                Say(new UiText(TextKeys.ShortcutUnknownKey));
                return;
            case RecordStep.Set:
                Stop();
                Set(row, recorded.Value);
                return;
        }
    }

    private static bool Down(VirtualKey key) =>
        InputKeyboardSource.GetKeyStateForCurrentThread(key).HasFlag(CoreVirtualKeyStates.Down);

    private static bool IsModifier(VirtualKey key) => key is VirtualKey.Control or VirtualKey.LeftControl or VirtualKey.RightControl
        or VirtualKey.Menu or VirtualKey.LeftMenu or VirtualKey.RightMenu
        or VirtualKey.Shift or VirtualKey.LeftShift or VirtualKey.RightShift
        or VirtualKey.LeftWindows or VirtualKey.RightWindows;

    private void Set(ShortcutRow row, string value)
    {
        if (model is null)
        {
            return;
        }
        var hotkeys = ShortcutText.With(model.Hotkeys, row, value);
        Apply(hotkeys, row);
    }

    /// <summary>Sends the whole set, as the service takes it, and says why it was refused.</summary>
    private async void Apply(Hotkeys hotkeys, ShortcutRow? row)
    {
        var before = App.Instance.Model.Snapshot.Hotkeys;
        if (hotkeys.Equals(before))
        {
            return;
        }
        try
        {
            await App.Instance.Model.SetHotkeysAsync(hotkeys);
            problem.Visibility = Visibility.Collapsed;
            AppLog.Info(
                $"hotkeys set: {hotkeys.TakeBack}, {hotkeys.Lock}, jumps {(hotkeys.JumpModifiers.Length == 0 ? "off" : hotkeys.JumpModifiers)}, relative {(hotkeys.HasRelative ? (hotkeys.Relative.Length == 0 ? "off" : hotkeys.Relative) : "unknown")}");
        }
        catch (AgentRefusal refusal)
        {
            AppLog.Info($"set hotkeys {hotkeys.TakeBack}, {hotkeys.Lock}, jumps {hotkeys.JumpModifiers}: {refusal.Message}");
            Say(ShortcutText.Refusal(refusal.Code, row, hotkeys, before));
        }
        catch (Exception e)
        {
            AppLog.Info($"set hotkeys: {e.Message}");
            Say(new UiText(TextKeys.ShortcutNotChanged));
        }
        if (model is not null)
        {
            // The switch may have moved without the service agreeing.
            Update(App.Instance.Model.Snapshot);
        }
    }

    private void Say(UiText text)
    {
        problem.Text = Strings.Text(text);
        problem.Visibility = Visibility.Visible;
    }
}
