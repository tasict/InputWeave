using System.Globalization;
using InputWeave.Panel;
using InputWeave.Support;
using InputWeave.Updates;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Documents;

namespace InputWeave.Main;

/// <summary>
/// "Install InputWeave 1.0.1?": the notes, the size,
/// View Online, Install / Later and Skip This Version; then, in the same
/// dialog, the download (which can be cancelled), the check, and UAC; and
/// what went wrong when nothing was installed.
/// </summary>
internal sealed partial class UpdateDialog : ContentDialog
{
    private readonly Updater updater;
    private readonly UpdateFeed feed;
    private readonly StackPanel ask = new() { Spacing = 10 };
    private readonly StackPanel running = new() { Spacing = 8, Visibility = Visibility.Collapsed };
    private readonly TextBlock phase = new() { TextWrapping = TextWrapping.Wrap };
    private readonly ProgressBar bar = new() { Maximum = 1 };
    private readonly TextBlock detail = Ui.Secondary("");
    private readonly InfoBar problem = new() { IsClosable = false, Severity = InfoBarSeverity.Error };
    private bool busy;

    /// <param name="install">Starts installing as soon as it opens, as if Install were chosen (for testing).</param>
    public UpdateDialog(Updater updater, UpdateFeed feed, bool install = false)
    {
        this.updater = updater;
        this.feed = feed;
        Title = Strings.Format(TextKeys.UpdateTitle, feed.Version);

        ask.Children.Add(new TextBlock { Text = Strings.Format(TextKeys.UpdateBody, App.Version), TextWrapping = TextWrapping.Wrap });
        if (feed.Notes.Trim().Length > 0)
        {
            ask.Children.Add(Ui.Strong(Strings.Get(TextKeys.UpdateWhatsNew)));
            var notes = new RichTextBlock { MaxHeight = 220 };
            foreach (var line in feed.Notes.Replace("\r", "").Split('\n'))
            {
                var text = line.Trim();
                if (text.Length == 0)
                {
                    continue;
                }
                var paragraph = new Paragraph { Margin = new Thickness(0, 0, 0, 4) };
                paragraph.Inlines.Add(new Run { Text = text.StartsWith("- ", StringComparison.Ordinal) ? "•  " + text[2..] : text });
                notes.Blocks.Add(paragraph);
            }
            ask.Children.Add(new ScrollViewer { Content = notes, MaxHeight = 220 });
        }
        var facts = new Grid();
        facts.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        facts.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var size = Ui.Secondary(Strings.Format(TextKeys.UpdateSize, ByteSize.Format((ulong)feed.Size, CultureInfo.CurrentCulture)));
        size.VerticalAlignment = VerticalAlignment.Center;
        facts.Children.Add(size);
        var online = new HyperlinkButton
        {
            Content = Strings.Get(TextKeys.UpdateViewOnline),
            NavigateUri = new Uri($"https://inputweave.ctsite.cc/releases/v{feed.Version}"),
        };
        Grid.SetColumn(online, 1);
        facts.Children.Add(online);
        ask.Children.Add(facts);
        var skip = new HyperlinkButton { Content = Strings.Get(TextKeys.UpdateSkip), Padding = new Thickness(0, 4, 0, 4) };
        skip.Click += (_, _) =>
        {
            updater.Skip(feed);
            Hide();
        };
        ask.Children.Add(skip);

        running.Children.Add(phase);
        running.Children.Add(bar);
        running.Children.Add(detail);
        var content = new StackPanel { Spacing = 12, MinWidth = 420, MaxWidth = 520 };
        content.Children.Add(ask);
        content.Children.Add(running);
        content.Children.Add(problem);
        Content = content;

        PrimaryButtonText = Strings.Get(TextKeys.UpdateInstall);
        CloseButtonText = Strings.Get(TextKeys.UpdateLater);
        DefaultButton = ContentDialogButton.Primary;
        PrimaryButtonClick += OnPrimary;
        CloseButtonClick += (_, args) =>
        {
            if (busy)
            {
                // Cancel while downloading; nothing else can be stopped.
                updater.CancelDownload();
                args.Cancel = updater.Phase != UpdatePhase.Downloading;
            }
        };
        updater.Changed += ShowPhase;
        if (install)
        {
            Opened += (_, _) => Install();
        }
        Closed += (_, _) => updater.Changed -= ShowPhase;
    }

    private void OnPrimary(ContentDialog sender, ContentDialogButtonClickEventArgs args)
    {
        args.Cancel = true;
        if (problem.Tag is "site")
        {
            LicensePage.Open("https://inputweave.ctsite.cc/");
            return;
        }
        Install();
    }

    private async void Install()
    {
        busy = true;
        problem.IsOpen = false;
        problem.Tag = null;
        ask.Visibility = Visibility.Collapsed;
        running.Visibility = Visibility.Visible;
        IsPrimaryButtonEnabled = false;
        CloseButtonText = Strings.Get(TextKeys.Cancel);
        var outcome = await updater.InstallAsync(feed);
        busy = false;
        AppLog.Info($"updates: install {feed.Version}: {outcome}");
        running.Visibility = Visibility.Collapsed;
        CloseButtonText = Strings.Get(TextKeys.Close);
        IsPrimaryButtonEnabled = true;
        switch (outcome)
        {
            case InstallOutcome.Cancelled:
                Hide();
                return;
            case InstallOutcome.BadDownload:
                problem.Message = Strings.Get(TextKeys.UpdateBadSignature);
                PrimaryButtonText = Strings.Get(TextKeys.UpdateOpenSite);
                problem.Tag = "site";
                break;
            case InstallOutcome.DownloadFailed:
                problem.Message = Strings.Get(TextKeys.UpdateDownloadFailed);
                PrimaryButtonText = Strings.Get(TextKeys.LicenseTryAgain);
                break;
            case InstallOutcome.RestartNeeded:
                // Installed; this app is still the old one until Windows restarts.
                problem.Severity = InfoBarSeverity.Informational;
                problem.Message = Strings.Format(TextKeys.UpdateRestartNeeded, feed.Version);
                IsPrimaryButtonEnabled = false;
                PrimaryButtonText = "";
                problem.IsOpen = true;
                return;
            default:
                problem.Message = Strings.Get(TextKeys.UpdateDeclined);
                PrimaryButtonText = Strings.Get(TextKeys.LicenseTryAgain);
                break;
        }
        problem.IsOpen = true;
    }

    private void ShowPhase()
    {
        switch (updater.Phase)
        {
            case UpdatePhase.Downloading:
                phase.Text = Strings.Get(TextKeys.UpdateDownloading);
                bar.IsIndeterminate = false;
                bar.Value = updater.Total > 0 ? (double)updater.Received / updater.Total : 0;
                detail.Text = Strings.Format(TextKeys.UpdateProgress,
                    ByteSize.Format((ulong)updater.Received, CultureInfo.CurrentCulture),
                    ByteSize.Format((ulong)updater.Total, CultureInfo.CurrentCulture));
                break;
            case UpdatePhase.Verifying:
                phase.Text = Strings.Get(TextKeys.UpdateVerifying);
                bar.IsIndeterminate = true;
                detail.Text = Strings.Get(TextKeys.UpdateVerifyingNote);
                break;
            case UpdatePhase.Installing:
                phase.Text = Strings.Get(TextKeys.UpdateInstalling);
                bar.IsIndeterminate = true;
                detail.Text = Strings.Get(TextKeys.UpdateInstallingNote);
                break;
        }
    }
}
