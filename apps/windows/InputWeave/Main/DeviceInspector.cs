using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;

namespace InputWeave.Main;

/// <summary>
/// What the arrangement and device pages show about one computer, and its
/// options: name, system, status with the
/// latency, its screens, whether this computer shares its clipboard and
/// files with it, swapping the modifier keys, and removing it. Another
/// computer's name is a field: the name it has here only (RenameDevice),
/// with the name it gives itself beneath once they differ.
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

        // Only paired computers have options; this one and those in the configuration file do not.
        options.Visibility = device is { Paired: true } ? Visibility.Visible : Visibility.Collapsed;
        if (device is not null)
        {
            swapTitle.Text = Strings.Get(DeviceText.SwapKey(os));
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(swap, swapTitle.Text);
            Ui.Set(swap, device.SwapGuiAndControl);
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
