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
/// computers on the license, Manage Computers and Remove License; and the
/// sixth computer, a refused key and an unreadable license file. The
/// license is the background service's; Polar is asked by the app's
/// <see cref="LicenseWorker"/>.
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
        var view = LicenseView.For(snapshot.Status.License, snapshot.Status.ThisDeviceName, worker.Withdrawing(snapshot.Status.License));
        var key = snapshot.Status.License?.Key ?? "";
        // Everything said about the work comes from the work and the service's license.
        var state = (View: view, Problem: worker.Problem(key), worker.RemoveFailed, worker.SeatNotFreed, worker.Unreadable);
        if (shown is { } before && before.View.Variant == view.Variant && before.View.Email == view.Email
            && before.View.SourceName == view.SourceName && before.Problem == state.Problem
            && before.RemoveFailed == state.RemoveFailed && before.SeatNotFreed == state.SeatNotFreed
            && before.Unreadable == state.Unreadable
            && before.View.Computers.SequenceEqual(view.Computers))
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
        var rows = view.Computers.Select(c => (UIElement)Ui.Row(
            c.Name,
            null,
            Ui.Secondary(Strings.Get(c.ThisComputer ? TextKeys.LicenseThisComputer : c.Licensed ? TextKeys.LicensePaired : TextKeys.LicenseNotActivated), caption: false),
            new FontIcon { Glyph = "" })).ToArray();
        page.Children.Add(Ui.Card(rows));
        var note = Ui.Secondary(Strings.Get(TextKeys.LicenseListNote));
        note.Margin = new Thickness(2, 8, 0, 0);
        page.Children.Add(note);
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Margin = new Thickness(0, 16, 0, 0) };
        buttons.Children.Add(Ui.Button(Strings.Get(TextKeys.LicenseManage), () => Open(PolarClient.PortalUrl)));
        buttons.Children.Add(Ui.Button(Strings.Get(TextKeys.LicenseRemove), Remove));
        page.Children.Add(buttons);
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
