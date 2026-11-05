using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;
using InputWeave.Support;
using InputWeave.Updates;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.System;

namespace InputWeave.Main;

/// <summary>
/// This computer's name, opening at login, the icon and File Explorer's
/// Send To, clipboard sync, sharing the desktop picture, switching (waiting
/// at the edge, a modifier to hold, relative mode turning on by itself for
/// games), the shortcuts with their recorder, and updates.
/// </summary>
internal sealed partial class GeneralPage : UserControl, IMainPage
{
    private readonly TextBox name = new() { MaxLength = 64, Width = 280 };
    private readonly TextBlock nameHint = Ui.Secondary(Strings.Get(TextKeys.GeneralNameHint));
    private readonly ToggleSwitch atLogin;
    private readonly ToggleSwitch showIcon;
    private readonly ToggleSwitch sendTo;
    private readonly ToggleSwitch clipboard;
    private readonly ToggleSwitch pictures;
    private readonly Grid picturesRow;
    private readonly Grid picturesUnavailable;
    private readonly ToggleSwitch wait;
    private readonly ComboBox switchModifier = new() { MinWidth = 220 };
    /// <summary>The app's own setting, not the service's.</summary>
    private readonly ToggleSwitch edgeGlow;
    private readonly ToggleSwitch relativeAuto;
    private readonly TextBlock relativeAutoHint = Ui.Secondary(Strings.Get(TextKeys.GeneralRelativeAutoHint));
    /// <summary>The relative mode switch and the line above it, shown when the service has relative mode.</summary>
    private readonly UIElement[] relativeAutoRow;
    /// <summary>The service refused the relative mode switch, and has not reported another value since.</summary>
    private bool relativeAutoRefused;
    private bool reportedRelativeAuto;
    private readonly ShortcutsSection shortcuts = new();
    private readonly ToggleSwitch autoUpdate;
    private readonly TextBlock versionLine = new();
    private readonly TextBlock checkLine = Ui.Secondary("");
    private readonly InfoBar updateBar = new() { IsClosable = false, Severity = InfoBarSeverity.Informational };
    private bool updating;
    private AgentSnapshot? model;
    private string shownName = "";

    public GeneralPage()
    {
        AutomationProperties.SetName(name, Strings.Get(TextKeys.GeneralName));
        name.KeyDown += (_, e) =>
        {
            if (e.Key == VirtualKey.Enter)
            {
                e.Handled = true;
                Rename();
            }
        };
        name.LostFocus += (_, _) => Rename();
        var nameWords = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        nameWords.Children.Add(new TextBlock { Text = Strings.Get(TextKeys.GeneralName) });
        nameWords.Children.Add(nameHint);

        atLogin = Ui.Switch(Strings.Get(TextKeys.GeneralOpenAtLogin), Preferences.OpensAtLogin, Preferences.SetOpensAtLogin);
        showIcon = Ui.Switch(Strings.Get(TextKeys.GeneralShowIcon), Preferences.ShowTrayIcon, App.Instance.SetTrayIconShown);
        sendTo = Ui.Switch(Strings.Get(TextKeys.GeneralSendTo), SendTo.IsOn, on =>
        {
            SendTo.Set(on);
            Ui.Set(sendTo!, SendTo.IsOn);
        });
        clipboard = Ui.Switch(Strings.Get(TextKeys.GeneralClipboardSync), false, on =>
            App.Instance.Send($"clipboard sync {on}", () => App.Instance.Model.SetClipboardSyncAsync(on)));
        pictures = Ui.Switch(Strings.Get(TextKeys.GeneralPicturesShare), false, on =>
            App.Instance.Send($"preview sharing {on}", () => App.Instance.Model.SetPreviewSharingAsync(on)));
        wait = Ui.Switch(Strings.Get(TextKeys.GeneralWait), false, on =>
        {
            var ms = on ? App.Instance.Model.Snapshot.SwitchDelayMs : 0;
            App.Instance.Send($"edge dwell {ms}", () => App.Instance.Model.SetEdgeDwellAsync(ms));
        });
        foreach (var (_, key) in ShortcutText.SwitchChoices)
        {
            switchModifier.Items.Add(Strings.Get(key));
        }
        AutomationProperties.SetName(switchModifier, Strings.Get(TextKeys.SwitchModifierTitle));
        switchModifier.SelectionChanged += (_, _) =>
        {
            if (updating || switchModifier.SelectedIndex < 0)
            {
                return;
            }
            var modifier = ShortcutText.SwitchChoices[switchModifier.SelectedIndex].Modifier;
            App.Instance.Send($"switch modifier {modifier}", () => App.Instance.Model.SetSwitchModifierAsync(modifier));
        };
        autoUpdate = Ui.Switch(Strings.Get(TextKeys.UpdatesAuto), Preferences.CheckForUpdates, on => Preferences.CheckForUpdates = on);
        edgeGlow = Ui.Switch(Strings.Get(TextKeys.GeneralEdgeGlow), Preferences.ShowEdgeGlow, on => Preferences.ShowEdgeGlow = on);
        relativeAuto = Ui.Switch(Strings.Get(TextKeys.GeneralRelativeAuto), true, on => SetRelativeAuto(on));
        relativeAutoHint.TextWrapping = TextWrapping.Wrap;
        var relativeWords = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        relativeWords.Children.Add(new TextBlock { Text = Strings.Get(TextKeys.GeneralRelativeAuto), TextWrapping = TextWrapping.Wrap });
        relativeWords.Children.Add(relativeAutoHint);
        relativeAutoRow = [Ui.Divider(), Ui.Row(relativeWords, relativeAuto)];
        picturesRow = Ui.Row(Strings.Get(TextKeys.GeneralPicturesShare), Strings.Get(TextKeys.GeneralPicturesHint), pictures);
        picturesUnavailable = Ui.Row(Strings.Get(TextKeys.GeneralPicturesUnavailable));

        var page = new StackPanel();
        var naming = Ui.Card(Ui.Row(nameWords, name));
        naming.Margin = new Thickness(0, 12, 0, 0);
        page.Children.Add(naming);
        var startup = Ui.Card(
            Ui.Row(Strings.Get(TextKeys.GeneralOpenAtLogin), null, atLogin),
            Ui.Row(Strings.Get(TextKeys.GeneralShowIcon), Strings.Get(TextKeys.GeneralShowIconHint), showIcon),
            Ui.Row(Strings.Get(TextKeys.GeneralSendTo), Strings.Get(TextKeys.GeneralSendToHint), sendTo));
        startup.Margin = new Thickness(0, 16, 0, 0);
        page.Children.Add(startup);
        page.Children.Add(Ui.Header(Strings.Get(TextKeys.GeneralClipboard)));
        page.Children.Add(Ui.Card(Ui.Row(Strings.Get(TextKeys.GeneralClipboardSync), Strings.Get(TextKeys.GeneralClipboardHint), clipboard)));
        page.Children.Add(Ui.Header(Strings.Get(TextKeys.GeneralPictures)));
        var picturesCard = new StackPanel();
        picturesCard.Children.Add(picturesRow);
        picturesCard.Children.Add(picturesUnavailable);
        page.Children.Add(Ui.Card(picturesCard));
        page.Children.Add(Ui.Header(Strings.Get(TextKeys.GeneralSwitching)));
        var switching = new StackPanel();
        switching.Children.Add(Ui.Row(Strings.Get(TextKeys.GeneralWait), Strings.Get(TextKeys.GeneralWaitHint), wait));
        switching.Children.Add(Ui.Divider());
        switching.Children.Add(Ui.Row(Strings.Get(TextKeys.SwitchModifierTitle), Strings.Get(TextKeys.SwitchModifierHint), switchModifier));
        switching.Children.Add(Ui.Divider());
        switching.Children.Add(Ui.Row(Strings.Get(TextKeys.GeneralEdgeGlow), Strings.Get(TextKeys.GeneralEdgeGlowHint), edgeGlow));
        foreach (var element in relativeAutoRow)
        {
            switching.Children.Add(element);
        }
        page.Children.Add(Ui.Card(switching));
        page.Children.Add(Ui.Header(Strings.Get(TextKeys.GeneralShortcuts)));
        page.Children.Add(shortcuts.View);
        page.Children.Add(Ui.Header(Strings.Get(TextKeys.GeneralUpdates)));
        updateBar.Margin = new Thickness(0, 0, 0, 8);
        updateBar.ActionButton = Ui.Button(Strings.Get(TextKeys.UpdatesView), () => App.Instance.ShowUpdate(), accent: true);
        page.Children.Add(updateBar);
        var versionWords = new StackPanel { Spacing = 2, VerticalAlignment = VerticalAlignment.Center };
        versionWords.Children.Add(versionLine);
        versionWords.Children.Add(checkLine);
        page.Children.Add(Ui.Card(
            Ui.Row(Strings.Get(TextKeys.UpdatesAuto), Strings.Get(TextKeys.UpdatesAutoHint), autoUpdate),
            Ui.Row(versionWords, Ui.Button(Strings.Get(TextKeys.UpdatesCheckNow), () => _ = App.Instance.CheckForUpdatesAsync()))));
        Content = Ui.Page(page);
        // A shortcut being recorded stops when the page goes.
        Unloaded += (_, _) =>
        {
            shortcuts.Stop();
            App.Instance.Updater.Changed -= ShowUpdates;
        };
        Loaded += (_, _) =>
        {
            App.Instance.Updater.Changed -= ShowUpdates;
            App.Instance.Updater.Changed += ShowUpdates;
            ShowUpdates();
        };
    }

    public void Invalidate()
    {
        model = null;
    }

    public void Update(AgentSnapshot snapshot, ServiceState service, DateTimeOffset now)
    {
        if (ReferenceEquals(snapshot, model))
        {
            return;
        }
        model = snapshot;
        var status = snapshot.Status;
        // The name box follows the service, except while the user types in it.
        if (status.ThisDeviceName != shownName && name.FocusState == FocusState.Unfocused)
        {
            name.Text = status.ThisDeviceName;
        }
        shownName = status.ThisDeviceName;
        Ui.Set(clipboard, status.ClipboardSync);
        Ui.Set(pictures, status.PreviewSharing);
        Ui.Set(wait, status.EdgeDwellMs != 0);
        updating = true;
        switchModifier.SelectedIndex = Math.Clamp((int)status.SwitchModifier, 0, ShortcutText.SwitchChoices.Count - 1);
        switchModifier.IsEnabled = snapshot.Link == AgentLink.Connected;
        updating = false;
        // A service from before relative mode has no such switch.
        foreach (var element in relativeAutoRow)
        {
            element.Visibility = snapshot.RelativeSupported ? Visibility.Visible : Visibility.Collapsed;
        }
        // The setting's state arrived, or the service is out of reach: a
        // refusal from before no longer holds.
        if (status.RelativeAuto != reportedRelativeAuto || snapshot.Link != AgentLink.Connected)
        {
            relativeAutoRefused = false;
        }
        reportedRelativeAuto = status.RelativeAuto;
        Ui.Set(relativeAuto, status.RelativeAuto);
        relativeAuto.IsEnabled = snapshot.Link == AgentLink.Connected;
        ShowRelativeAutoHint();
        shortcuts.Update(snapshot);
        // Windows reads the desktop picture as the logged-on user, through the broker; without it there is none to share.
        picturesRow.Visibility = status.DesktopPicturesAvailable ? Visibility.Visible : Visibility.Collapsed;
        picturesUnavailable.Visibility = status.DesktopPicturesAvailable ? Visibility.Collapsed : Visibility.Visible;
        Ui.Set(atLogin, Preferences.OpensAtLogin);
        Ui.Set(showIcon, Preferences.ShowTrayIcon);
        Ui.Set(sendTo, SendTo.IsOn);
    }

    /// <summary>The Updates card and the bar above it, from what the updater last found.</summary>
    private void ShowUpdates()
    {
        var updater = App.Instance.Updater;
        Ui.Set(autoUpdate, Preferences.CheckForUpdates);
        versionLine.Text = Strings.Format(TextKeys.UpdatesVersion, App.Version);
        var found = updater.Found;
        checkLine.Text = updater.Phase == UpdatePhase.Checking
            ? Strings.Get(TextKeys.UpdatesChecking)
            : updater.CheckFailed
                ? Strings.Get(TextKeys.UpdatesOffline)
                : found is not null && updater.Verdict == UpdateVerdict.NeedsNewerWindows
                    ? Strings.Format(TextKeys.UpdatesNeedsWindows, found.Version)
                    : found is not null
                        ? Strings.Format(TextKeys.UpdatesAvailable, found.Version)
                        : Preferences.LastUpdateCheck is { } last
                            ? Strings.Format(TextKeys.UpdatesUpToDate, When(last))
                            : Strings.Get(TextKeys.UpdatesNotChecked);
        bool offer = found is not null && updater.Verdict == UpdateVerdict.Available;
        updateBar.IsOpen = offer;
        if (offer)
        {
            updateBar.Title = Strings.Format(TextKeys.UpdatesAvailable, found!.Version);
            updateBar.Message = Strings.Format(TextKeys.UpdatesYouHave, App.Version);
        }
    }

    /// <summary>"14:05" today, otherwise the date and time, in this computer's format.</summary>
    private static string When(DateTimeOffset at)
    {
        var local = at.ToLocalTime();
        return local.Date == DateTimeOffset.Now.Date
            ? local.ToString("t", System.Globalization.CultureInfo.CurrentCulture)
            : local.ToString("g", System.Globalization.CultureInfo.CurrentCulture);
    }

    /// <summary>Turns automatic relative mode on or off, and says so under the switch when the service does not take it.</summary>
    private async void SetRelativeAuto(bool on)
    {
        try
        {
            await App.Instance.Model.SetRelativeAutoAsync(on);
            relativeAutoRefused = false;
        }
        catch (Exception e)
        {
            AppLog.Info($"relative auto {on}: {e.Message}");
            relativeAutoRefused = true;
            // The switch moved without the service agreeing.
            Ui.Set(relativeAuto, model?.Status.RelativeAuto ?? !on);
        }
        ShowRelativeAutoHint();
    }

    /// <summary>
    /// The line under the relative mode switch: why the service refused
    /// it, or what it does, pointing to the hotkey below for a Mac, or to
    /// setting one when it is off.
    /// </summary>
    private void ShowRelativeAutoHint()
    {
        if (relativeAutoRefused)
        {
            relativeAutoHint.Text = Strings.Get(TextKeys.GeneralRelativeAutoFailed);
            relativeAutoHint.Foreground = Brand.System("SystemFillColorCriticalBrush");
            return;
        }
        relativeAutoHint.Text = Strings.Get(model?.RelativeShortcut is null
            ? TextKeys.GeneralRelativeAutoHintNoShortcut
            : TextKeys.GeneralRelativeAutoHint);
        relativeAutoHint.Foreground = Brand.System("TextFillColorSecondaryBrush");
    }

    private async void Rename()
    {
        if (model is null)
        {
            return;
        }
        var wanted = name.Text.Trim();
        if (wanted == model.Status.ThisDeviceName)
        {
            nameHint.Text = Strings.Get(TextKeys.GeneralNameHint);
            nameHint.Foreground = Brand.System("TextFillColorSecondaryBrush");
            return;
        }
        try
        {
            await App.Instance.Model.RenameAsync(wanted);
            nameHint.Text = Strings.Get(TextKeys.GeneralNameHint);
            nameHint.Foreground = Brand.System("TextFillColorSecondaryBrush");
        }
        catch (Exception e)
        {
            AppLog.Info($"rename: {e.Message}");
            nameHint.Text = Strings.Get(TextKeys.NameInvalid);
            nameHint.Foreground = Brand.System("SystemFillColorCriticalBrush");
        }
    }
}
