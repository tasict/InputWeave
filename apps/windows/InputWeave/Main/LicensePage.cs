using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Licensing;
using InputWeave.Panel;
using InputWeave.Support;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;

namespace InputWeave.Main;

/// <summary>
/// The License page, opened from the item at the foot of
/// the sidebar: in trial, what a license is and Buy a License / Enter
/// License Key; licensed, the woven label, who it is licensed to, the
/// computers on the license (applying it to them and revoking it there),
/// Manage Computers and Remove License; and the sixth computer, a refused
/// key and an unreadable license file. The license is the background
/// service's; Polar is asked by the app's <see cref="LicenseWorker"/>.
/// </summary>
internal sealed partial class LicensePage : UserControl, IMainPage
{
    private readonly StackPanel page = new();
    private (LicenseView View, ActivationOutcome? Problem, bool RemoveFailed, bool SeatNotFreed, bool Unreadable)? shown;
    private bool sewNext;
    private EnterKeyDialog? keyDialog;

    public LicensePage()
    {
        Content = Ui.Page(page);
    }

    public void Invalidate() => shown = null;

    public void Update(AgentSnapshot snapshot, ServiceState service, DateTimeOffset now)
    {
        var worker = App.Instance.License;
        keyDialog?.Follow(snapshot.Status.License, worker.Withdrawing(snapshot.Status.License));
        var view = LicenseView.For(snapshot.Status, worker.Withdrawing(snapshot.Status.License));
        var key = snapshot.Status.License?.Key ?? "";
        // Everything said about the work comes from the work and the service's license.
        var state = (View: view, Problem: worker.Problem(key), worker.RemoveFailed, worker.SeatNotFreed, worker.Unreadable);
        if (shown is { } before && before.View.Variant == view.Variant && before.View.Email == view.Email
            && before.View.SourceName == view.SourceName && before.Problem == state.Problem
            && before.RemoveFailed == state.RemoveFailed && before.SeatNotFreed == state.SeatNotFreed
            && before.Unreadable == state.Unreadable
            && before.View.ApplySupported == view.ApplySupported && before.View.LicensedElsewhere == view.LicensedElsewhere
            && before.View.Computers.SequenceEqual(view.Computers)
            && before.View.Rows.SequenceEqual(view.Rows)
            && before.View.WaitingToApply.SequenceEqual(view.WaitingToApply))
        {
            return;
        }
        if (view.Variant != LicenseVariant.Licensed)
        {
            sewNext = false;
        }
        shown = state;
        page.Children.Clear();
        if (state.Unreadable)
        {
            page.Children.Add(Spaced(Bar(TextKeys.LicenseErrorUnavailable, InfoBarSeverity.Informational)));
        }
        if (view.Variant == LicenseVariant.Licensed)
        {
            ShowLicensed(view);
        }
        else
        {
            ShowTrial(view, state.Problem, key, state.RemoveFailed, state.SeatNotFreed);
        }
    }

    private void ShowTrial(LicenseView view, ActivationOutcome? problem, string key, bool removeFailed, bool seatNotFreed)
    {
        var card = new StackPanel { Spacing = 10, Padding = new Thickness(20, 18, 20, 18) };
        card.Children.Add(new TextBlock { Text = Strings.Get(TextKeys.LicenseTrial), Style = Ui.Style("SubtitleTextBlockStyle") });
        card.Children.Add(Body(Strings.Get(TextKeys.LicenseTrialBody)));
        if (removeFailed)
        {
            card.Children.Add(Bar(TextKeys.LicenseRemovePending, InfoBarSeverity.Warning));
        }
        if (seatNotFreed)
        {
            card.Children.Add(Bar(TextKeys.LicenseRemovedOffline, InfoBarSeverity.Warning, portal: true));
        }
        switch (view.Variant)
        {
            case LicenseVariant.Unavailable:
                card.Children.Add(Bar(TextKeys.LicenseUnavailable, InfoBarSeverity.Informational));
                page.Children.Add(Spaced(Ui.Card(card)));
                return;
            case LicenseVariant.SeatsFull:
                card.Children.Add(Bar(TextKeys.LicenseSeatsFull, InfoBarSeverity.Warning));
                break;
            case LicenseVariant.Invalid:
                card.Children.Add(Bar(TextKeys.LicenseInvalid, InfoBarSeverity.Error));
                break;
            default:
                switch (problem)
                {
                    case ActivationOutcome.Offline or ActivationOutcome.Busy:
                        card.Children.Add(Bar(TextKeys.LicenseOffline, InfoBarSeverity.Informational));
                        break;
                    case ActivationOutcome.NotRecorded:
                        card.Children.Add(Bar(TextKeys.LicenseErrorSave, InfoBarSeverity.Warning));
                        break;
                    case ActivationOutcome.Unavailable:
                        card.Children.Add(Bar(TextKeys.LicenseErrorUnavailable, InfoBarSeverity.Informational));
                        break;
                    case ActivationOutcome.ServiceRefused:
                        card.Children.Add(Bar(TextKeys.LicenseErrorService, InfoBarSeverity.Error));
                        break;
                }
                break;
        }
        card.Children.Add(new TextBlock { Text = Strings.Get(TextKeys.LicensePrice), TextWrapping = TextWrapping.Wrap });
        if (view.LicensedElsewhere is { } holder)
        {
            card.Children.Add(new InfoBar
            {
                IsOpen = true,
                IsClosable = false,
                Severity = InfoBarSeverity.Informational,
                Message = Strings.Format(TextKeys.LicenseElsewhere, holder),
            });
        }
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Margin = new Thickness(0, 4, 0, 0) };
        if (view.Variant == LicenseVariant.SeatsFull)
        {
            buttons.Children.Add(Ui.Button(Strings.Get(TextKeys.LicenseManage), () => Open(PolarClient.PortalUrl), accent: true));
            buttons.Children.Add(Ui.Button(Strings.Get(TextKeys.LicenseTryAgain), () => TryAgain(key)));
        }
        else
        {
            buttons.Children.Add(Ui.Button(Strings.Get(TextKeys.LicenseBuy), () => Open(PolarClient.CheckoutUrl), accent: true));
            buttons.Children.Add(Ui.Button(Strings.Get(TextKeys.LicenseEnter), EnterKey));
        }
        card.Children.Add(buttons);
        page.Children.Add(Spaced(Ui.Card(card)));

        // Applied from here before the license was lost: sent once it is back.
        if (view.WaitingToApply.Count != 0)
        {
            page.Children.Add(Ui.Header(Strings.Get(TextKeys.LicenseWaitingToApply)));
            page.Children.Add(Ui.Card([.. view.WaitingToApply.Select(r => (UIElement)Row(r, allPlacesUsed: false))]));
        }

        page.Children.Add(Ui.Header(Strings.Get(TextKeys.LicenseHowTitle)));
        var steps = new List<UIElement>();
        foreach (var (key2, n) in new[] { (TextKeys.LicenseHow1, 1), (TextKeys.LicenseHow2, 2), (TextKeys.LicenseHow3, 3) })
        {
            steps.Add(Ui.Row(Step(n, Strings.Get(key2))));
        }
        page.Children.Add(Ui.Card([.. steps]));
        var note = Ui.Secondary(Strings.Get(TextKeys.LicensePolarNote));
        note.Margin = new Thickness(2, 8, 0, 0);
        page.Children.Add(note);
    }

    private void ShowLicensed(LicenseView view)
    {
        var who = new StackPanel { Spacing = 3, VerticalAlignment = VerticalAlignment.Center };
        if (view.Email.Length > 0)
        {
            who.Children.Add(new TextBlock
            {
                Text = Strings.Format(TextKeys.LicensedTo, view.Email),
                Style = Ui.Style("BodyStrongTextBlockStyle"),
                TextWrapping = TextWrapping.Wrap,
            });
        }
        who.Children.Add(Body(view.SourceName is { } source
            ? Strings.Format(TextKeys.LicenseFrom, source)
            : Strings.Get(TextKeys.LicenseThanks)));
        var top = new Grid { ColumnSpacing = 18, Padding = new Thickness(20, 18, 20, 18) };
        top.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        top.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        top.Children.Add(WovenLabel.Create(44, sew: sewNext));
        sewNext = false;
        Grid.SetColumn(who, 1);
        top.Children.Add(who);
        page.Children.Add(Spaced(Ui.Card(new StackPanel { Children = { top } })));

        var count = Ui.Secondary(Strings.Format(TextKeys.LicenseCount, view.LicensedCount), caption: false);
        page.Children.Add(Ui.Header(Strings.Get(TextKeys.LicenseComputers), count));
        if (view.ApplySupported)
        {
            ShowRows(view);
        }
        else
        {
            // A background service before 1.1: the computers that say they use the key.
            var rows = view.Computers.Select(c => (UIElement)Ui.Row(
                c.Name,
                null,
                Ui.Secondary(Strings.Get(c.ThisComputer ? TextKeys.LicenseThisComputer : c.Licensed ? TextKeys.LicensePaired : TextKeys.LicenseNotActivated), caption: false),
                new FontIcon { Glyph = "\ue7f8" })).ToArray();
            page.Children.Add(Ui.Card(rows));
            var note = Ui.Secondary(Strings.Get(TextKeys.LicenseListNote));
            note.Margin = new Thickness(2, 8, 0, 0);
            page.Children.Add(note);
        }
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Margin = new Thickness(0, 16, 0, 0) };
        buttons.Children.Add(Ui.Button(Strings.Get(TextKeys.LicenseManage), () => Open(PolarClient.PortalUrl)));
        buttons.Children.Add(Ui.Button(Strings.Get(TextKeys.LicenseRemove), Remove));
        page.Children.Add(buttons);
    }

    /// <summary>
    /// "Computers on this license": this computer, then every computer it
    /// trusts, each with how it stands and what can be done; under them why
    /// Apply License may not work, and that only these computers use it.
    /// </summary>
    private void ShowRows(LicenseView view)
    {
        page.Children.Add(Ui.Card([.. view.Rows.Select(r => (UIElement)Row(r, view.AllPlacesUsed))]));
        if (view.NoPlaces)
        {
            page.Children.Add(Spaced(Bar(TextKeys.LicenseNoPlacesNote, InfoBarSeverity.Warning, portal: true)));
        }
        if (view.AllPlacesUsed)
        {
            var full = Ui.Secondary(Strings.Get(TextKeys.LicenseAllInUse), caption: false);
            full.Margin = new Thickness(2, 10, 0, 0);
            page.Children.Add(full);
        }
        var note = Ui.Secondary(Strings.Get(TextKeys.LicenseAppliedNote));
        note.Margin = new Thickness(2, 8, 0, 0);
        page.Children.Add(note);
    }

    /// <summary>A computer on the license: its name and what is said under it, how it stands, and its button.</summary>
    private Grid Row(LicenseRow row, bool allPlacesUsed)
    {
        var words = new StackPanel { Spacing = 2 };
        words.Children.Add(new TextBlock { Text = row.Name, TextWrapping = TextWrapping.Wrap });
        foreach (var key in new[] { row.LineKey, row.NoteKey })
        {
            if (key is not null)
            {
                words.Children.Add(Ui.Secondary(Strings.Get(key)));
            }
        }
        var trailing = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 16 };
        trailing.Children.Add(Standing(row.State));
        Button? button = null;
        if (row.CanRevoke)
        {
            button = Ui.Button(Strings.Get(TextKeys.LicenseRevoke), () => ConfirmRevoke(row, button!));
        }
        else if (row.CanApply)
        {
            button = Ui.Button(Strings.Get(TextKeys.LicenseApply), () => Act(button!, $"apply the license to {row.Id}", () => App.Instance.License.ApplyAsync(row.Id)));
            button.IsEnabled = !allPlacesUsed;
        }
        else if (row.CanTakeBack)
        {
            button = Ui.Button(Strings.Get(TextKeys.LicenseDontApply), () => Act(button!, $"don't apply the license to {row.Id}", () => App.Instance.License.DontApplyAsync(row.Id)));
        }
        if (button is not null)
        {
            button.VerticalAlignment = VerticalAlignment.Center;
            trailing.Children.Add(button);
        }
        return Ui.Row(words, trailing, new FontIcon { Glyph = "\ue7f4" });
    }

    /// <summary>How a computer stands on the license, at the right of its row.</summary>
    private static FrameworkElement Standing(LicenseRowState state)
    {
        var panel = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6, VerticalAlignment = VerticalAlignment.Center };
        TextBlock Text(string key, string brush = "TextFillColorSecondaryBrush") => new()
        {
            Text = Strings.Get(key),
            Foreground = Brand.System(brush),
            VerticalAlignment = VerticalAlignment.Center,
        };
        switch (state)
        {
            case LicenseRowState.ThisComputer or LicenseRowState.Licensed:
                panel.Children.Add(new FontIcon
                {
                    Glyph = "\ue73e",
                    FontSize = 14,
                    Foreground = Brand.System("SystemFillColorSuccessBrush"),
                    VerticalAlignment = VerticalAlignment.Center,
                });
                panel.Children.Add(Text(TextKeys.LicenseLicensed, "TextFillColorPrimaryBrush"));
                break;
            case LicenseRowState.Activating:
                panel.Children.Add(new ProgressRing { IsActive = true, Width = 16, Height = 16, VerticalAlignment = VerticalAlignment.Center });
                panel.Children.Add(Text(TextKeys.LicenseActivating));
                break;
            case LicenseRowState.Waiting:
                panel.Children.Add(Text(TextKeys.LicenseWaiting));
                break;
            case LicenseRowState.NoPlaces:
                panel.Children.Add(Text(TextKeys.LicenseNoPlaces, "SystemFillColorCautionBrush"));
                break;
            case LicenseRowState.OtherLicense:
                panel.Children.Add(Text(TextKeys.LicenseOtherLicense));
                break;
            case LicenseRowState.AwaitingLicense:
                panel.Children.Add(Text(TextKeys.LicenseAwaitingLicense));
                break;
            default:
                panel.Children.Add(Text(TextKeys.LicenseTrial));
                break;
        }
        return panel;
    }

    /// <summary>Apply License or Don't Apply: the button waits for the service; the row then shows what it says.</summary>
    private async void Act(Button button, string what, Func<Task<bool>> command)
    {
        button.IsEnabled = false;
        try
        {
            await command();
        }
        catch (Exception e)
        {
            AppLog.Info($"license: {what}: {e.Message}");
        }
        Invalidate();
        App.Instance.RefreshNow();
    }

    /// <summary>Revoke License…: asks first; the row turns to trial as the service says so.</summary>
    private async void ConfirmRevoke(LicenseRow row, Button button)
    {
        if (XamlRoot is null)
        {
            return;
        }
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = Strings.Format(TextKeys.LicenseRevokeTitle, row.Name),
            Content = new TextBlock { Text = Strings.Format(TextKeys.LicenseRevokeBody, row.Name), TextWrapping = TextWrapping.Wrap },
            PrimaryButtonText = Strings.Get(TextKeys.LicenseRevokeConfirm),
            CloseButtonText = Strings.Get(TextKeys.LicenseKeep),
            DefaultButton = ContentDialogButton.Close,
        };
        try
        {
            if (await dialog.ShowAsync() != ContentDialogResult.Primary)
            {
                return;
            }
            AppLog.Info($"license: revoke on {row.Id}");
            // Polar first, which may take a while: not asked twice meanwhile.
            button.IsEnabled = false;
            await App.Instance.License.RevokeAsync(row.Id, row.ActivationId);
        }
        catch (Exception e)
        {
            AppLog.Info($"revoke license: {e.Message}");
        }
        Invalidate();
        App.Instance.RefreshNow();
    }

    private static FrameworkElement Spaced(FrameworkElement card)
    {
        card.Margin = new Thickness(0, 12, 0, 0);
        return card;
    }

    private static TextBlock Body(string text) => new()
    {
        Text = text,
        TextWrapping = TextWrapping.Wrap,
        Foreground = Brand.System("TextFillColorSecondaryBrush"),
        MaxWidth = 640,
        HorizontalAlignment = HorizontalAlignment.Left,
    };

    private static FrameworkElement Step(int n, string text)
    {
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 14 };
        row.Children.Add(new Border
        {
            Width = 24,
            Height = 24,
            CornerRadius = new CornerRadius(12),
            Background = Brand.System("ControlFillColorSecondaryBrush"),
            Child = new TextBlock
            {
                Text = n.ToString(System.Globalization.CultureInfo.CurrentCulture),
                FontSize = 12,
                HorizontalAlignment = HorizontalAlignment.Center,
                VerticalAlignment = VerticalAlignment.Center,
                Foreground = Brand.System("TextFillColorSecondaryBrush"),
            },
        });
        row.Children.Add(new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap, VerticalAlignment = VerticalAlignment.Center });
        return row;
    }

    private InfoBar Bar(string key, InfoBarSeverity severity, bool portal = false)
    {
        var bar = new InfoBar
        {
            IsOpen = true,
            IsClosable = false,
            Severity = severity,
            Message = Strings.Get(key),
        };
        if (portal)
        {
            bar.ActionButton = new HyperlinkButton { Content = Strings.Get(TextKeys.LicenseManage), NavigateUri = new Uri(PolarClient.PortalUrl) };
        }
        return bar;
    }

    internal static void Open(string url) => _ = Windows.System.Launcher.LaunchUriAsync(new Uri(url));

    private static async Task RemoveAsync()
    {
        try
        {
            AppLog.Info($"license: remove: {await App.Instance.License.RemoveAsync()}");
        }
        catch (Exception e)
        {
            AppLog.Info($"remove license: {e.Message}");
        }
        App.Instance.RefreshNow();
    }

    private async void TryAgain(string key)
    {
        var outcome = await App.Instance.License.EnterAsync(key);
        AppLog.Info($"license: try again: {outcome}");
        sewNext = outcome == ActivationOutcome.Licensed;
        Invalidate();
        App.Instance.RefreshNow();
    }

    private async void EnterKey()
    {
        if (XamlRoot is null)
        {
            return;
        }
        var dialog = new EnterKeyDialog { XamlRoot = XamlRoot };
        keyDialog = dialog;
        try
        {
            await dialog.ShowAsync();
        }
        catch (Exception e)
        {
            AppLog.Info($"enter license key: {e.Message}");
        }
        finally
        {
            keyDialog = null;
        }
        if (dialog.Activated)
        {
            sewNext = true;
            Invalidate();
            App.Instance.RefreshNow();
        }
    }

    private async void Remove()
    {
        if (XamlRoot is null)
        {
            return;
        }
        var progress = new ProgressBar { IsIndeterminate = true, Visibility = Visibility.Collapsed };
        var body = new TextBlock { Text = Strings.Get(TextKeys.LicenseRemoveBody), TextWrapping = TextWrapping.Wrap };
        var content = new StackPanel { Spacing = 12 };
        content.Children.Add(body);
        content.Children.Add(progress);
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = Strings.Get(TextKeys.LicenseRemoveTitle),
            Content = content,
            PrimaryButtonText = Strings.Get(TextKeys.LicenseRemove),
            CloseButtonText = Strings.Get(TextKeys.Cancel),
            DefaultButton = ContentDialogButton.Close,
        };
        // Stays open, with progress, through the first attempt only, and
        // no longer than the background service is waited for; what is
        // left after it the page shows, and the app keeps trying.
        dialog.PrimaryButtonClick += async (_, args) =>
        {
            var deferral = args.GetDeferral();
            dialog.IsPrimaryButtonEnabled = false;
            body.Text = Strings.Get(TextKeys.LicenseRemoving);
            progress.Visibility = Visibility.Visible;
            var removing = RemoveAsync();
            await Task.WhenAny(removing, Task.Delay(LicenseWorker.ServiceWait));
            deferral.Complete();
        };
        try
        {
            if (await dialog.ShowAsync() != ContentDialogResult.Primary)
            {
                return;
            }
        }
        catch (Exception e)
        {
            AppLog.Info($"remove license: {e.Message}");
        }
        Invalidate();
        App.Instance.RefreshNow();
    }
}

/// <summary>Enter License Key: the key, its errors and the progress while Polar is asked.</summary>
internal sealed partial class EnterKeyDialog : ContentDialog
{
    private readonly TextBox key = new()
    {
        Header = Strings.Get(TextKeys.LicenseKey),
        PlaceholderText = "INPUTWEAVE-…",
        FontFamily = new FontFamily("Consolas"),
        MaxLength = 96,
    };
    private readonly InfoBar error = new() { IsClosable = false, Severity = InfoBarSeverity.Error };
    private readonly ProgressBar progress = new() { IsIndeterminate = true, Visibility = Visibility.Collapsed };

    public EnterKeyDialog()
    {
        Title = Strings.Get(TextKeys.LicenseEnterTitle);
        PrimaryButtonText = Strings.Get(TextKeys.LicenseActivate);
        CloseButtonText = Strings.Get(TextKeys.Cancel);
        DefaultButton = ContentDialogButton.Primary;
        IsPrimaryButtonEnabled = false;
        var content = new StackPanel { Spacing = 10, MinWidth = 400 };
        content.Children.Add(key);
        content.Children.Add(Ui.Secondary(Strings.Get(TextKeys.LicenseKeyHelp)));
        content.Children.Add(error);
        content.Children.Add(progress);
        Content = content;
        key.TextChanged += (_, _) =>
        {
            // Another key: the one given before is no longer watched here.
            if (following is not null && key.Text.Trim() != following)
            {
                following = null;
            }
            error.IsOpen = false;
            IsPrimaryButtonEnabled = key.Text.Trim().Length > 0;
        };
        PrimaryButtonClick += OnActivate;
    }

    /// <summary>The key was activated, and the dialog closed.</summary>
    public bool Activated { get; private set; }

    /// <summary>The key given to the service and not settled yet: the app keeps working on it, and the dialog keeps watching.</summary>
    private string? following;

    /// <summary>The service's license went through waiting for activation of <see cref="following"/> since it was entered.</summary>
    private bool pendingSeen;

    /// <summary>
    /// The service's license changed while the dialog is open: what comes
    /// after the key was entered settles it (licensed closes the dialog;
    /// seats full or invalid is said), however long the app worked on it.
    /// </summary>
    public void Follow(License? license, bool withdrawing)
    {
        if (following is null || license is null || license.Key != following)
        {
            return;
        }
        switch (license.State)
        {
            case LicenseState.PendingActivation:
                pendingSeen = true;
                break;
            case LicenseState.Licensed when pendingSeen && !withdrawing:
                following = null;
                Activated = true;
                Hide();
                break;
            case LicenseState.SeatsFull or LicenseState.Invalid when pendingSeen:
                following = null;
                Show(license.State == LicenseState.SeatsFull ? ActivationOutcome.SeatsFull : ActivationOutcome.Invalid);
                break;
        }
    }

    private async void OnActivate(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        // Stays open while Polar is asked, and with what went wrong.
        args.Cancel = true;
        var deferral = args.GetDeferral();
        IsPrimaryButtonEnabled = false;
        key.IsEnabled = false;
        error.IsOpen = false;
        progress.Visibility = Visibility.Visible;
        following = null;
        pendingSeen = false;
        ActivationOutcome outcome;
        try
        {
            outcome = await App.Instance.License.EnterAsync(key.Text);
        }
        catch (Exception e) when (e is AgentRefusal or AgentUnavailableException)
        {
            AppLog.Info($"enter license key: {e.Message}");
            outcome = ActivationOutcome.NotRecorded;
        }
        progress.Visibility = Visibility.Collapsed;
        key.IsEnabled = true;
        IsPrimaryButtonEnabled = true;
        deferral.Complete();
        if (outcome == ActivationOutcome.Licensed)
        {
            Activated = true;
            Hide();
            return;
        }
        if (outcome is ActivationOutcome.Offline or ActivationOutcome.Busy or ActivationOutcome.NotRecorded
            or ActivationOutcome.Unavailable or ActivationOutcome.ServiceNotRunning)
        {
            // Not settled: keep watching what the app makes of this key.
            following = key.Text.Trim();
            var current = App.Instance.Model.Snapshot.Status.License;
            pendingSeen = current is { State: LicenseState.PendingActivation } && current.Key == following;
        }
        Show(outcome);
    }

    private void Show(ActivationOutcome outcome)
    {
        error.Message = Strings.Get(outcome switch
        {
            ActivationOutcome.SeatsFull => TextKeys.LicenseErrorFull,
            ActivationOutcome.Invalid => TextKeys.LicenseErrorInvalid,
            ActivationOutcome.Offline => TextKeys.LicenseErrorOffline,
            ActivationOutcome.Busy => TextKeys.LicenseErrorBusy,
            ActivationOutcome.NotRecorded => TextKeys.LicenseErrorSave,
            ActivationOutcome.Unavailable => TextKeys.LicenseErrorUnavailable,
            ActivationOutcome.ServiceNotRunning => TextKeys.TitleNotResponding,
            _ => TextKeys.LicenseErrorService,
        });
        error.ActionButton = outcome == ActivationOutcome.SeatsFull
            ? new HyperlinkButton { Content = Strings.Get(TextKeys.LicenseManage), NavigateUri = new Uri(PolarClient.PortalUrl) }
            : null;
        error.IsOpen = true;
    }
}
