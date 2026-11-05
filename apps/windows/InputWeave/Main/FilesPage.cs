using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Support;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Windows.Storage.Pickers;
using WinRT.Interop;

namespace InputWeave.Main;

/// <summary>
/// Where received files go, whether to ask first, and the files sent and
/// received lately. The folder is the
/// background service's to keep: a computer that does not receive files
/// says so instead of offering one.
/// </summary>
internal sealed partial class FilesPage : UserControl, IMainPage
{
    private readonly TextBlock folder = new() { TextTrimming = TextTrimming.CharacterEllipsis, MaxLines = 1, VerticalAlignment = VerticalAlignment.Center };
    private readonly StackPanel folderRow = new() { Orientation = Orientation.Horizontal, Spacing = 8 };
    private readonly TextBlock notReceiving = Ui.Secondary(Strings.Get(TextKeys.TransferNotReceiving), caption: false);
    private readonly RadioButtons ask = new();
    private readonly Button send;
    private readonly StackPanel recent = new();
    private readonly Border recentCard;
    private readonly Button clear;
    private readonly TextBlock none = Ui.Secondary(Strings.Get(TextKeys.FilesNone), caption: false);
    private AgentSnapshot? model;
    private bool updating;

    public FilesPage()
    {
        folderRow.Children.Add(new FontIcon { Glyph = "", FontSize = 14, VerticalAlignment = VerticalAlignment.Center });
        folder.MaxWidth = 360;
        folderRow.Children.Add(folder);
        folderRow.Children.Add(Ui.Button(Strings.Get(TextKeys.FilesChoose), ChooseFolder));
        var where = new Grid();
        where.Children.Add(folderRow);
        where.Children.Add(notReceiving);

        ask.Items.Add(Strings.Get(TextKeys.FilesAutomatically));
        ask.Items.Add(Strings.Get(TextKeys.FilesAskEachTime));
        AutomationProperties.SetName(ask, Strings.Get(TextKeys.FilesWhenSent));
        ask.SelectionChanged += (_, _) => SetAsk();
        var askRow = new StackPanel { Padding = new Thickness(16, 12, 16, 12), Spacing = 4 };
        askRow.Children.Add(new TextBlock { Text = Strings.Get(TextKeys.FilesWhenSent) });
        askRow.Children.Add(ask);

        send = Ui.Button(Strings.Get(TextKeys.FilesSendButton), () => SendFiles.Start(App.Instance.MainHwnd));

        recentCard = Ui.Card(recent);
        recent.Padding = new Thickness(16, 12, 16, 12);
        recent.Spacing = 14;
        clear = Ui.Button(Strings.Get(TextKeys.FilesClearHistory), ConfirmClear);
        clear.IsEnabled = false;

        var page = new StackPanel();
        page.Children.Add(Ui.Header(Strings.Get(TextKeys.FilesReceiving)));
        page.Children.Add(Ui.Card(Ui.Row(new TextBlock { Text = Strings.Get(TextKeys.FilesSaveIn), TextWrapping = TextWrapping.Wrap }, where), askRow));
        page.Children.Add(Ui.Header(Strings.Get(TextKeys.FilesSending)));
        page.Children.Add(Ui.Card(Ui.Row(Ui.Secondary(Strings.Get(TextKeys.FilesSendHint), caption: false), send)));
        page.Children.Add(Ui.Header(Strings.Get(TextKeys.FilesRecent), clear));
        page.Children.Add(recentCard);
        page.Children.Add(none);
        Content = Ui.Page(page);
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
        var before = model;
        model = snapshot;
        var transfers = snapshot.Transfers;
        bool receives = transfers.ReceiveFolder.Length != 0;
        folderRow.Visibility = receives ? Visibility.Visible : Visibility.Collapsed;
        notReceiving.Visibility = receives ? Visibility.Collapsed : Visibility.Visible;
        folder.Text = transfers.ReceiveFolder;
        ToolTipService.SetToolTip(folder, transfers.ReceiveFolder);
        updating = true;
        ask.SelectedIndex = transfers.AskBeforeReceiving ? 1 : 0;
        ask.IsEnabled = receives;
        updating = false;
        send.IsEnabled = snapshot.FileReceivers.Count != 0;

        if (before is not null && Equals(before.Transfers, transfers) && before.Status.Equals(snapshot.Status))
        {
            return;
        }
        recent.Children.Clear();
        foreach (var transfer in TransferText.Ordered(transfers.Transfers_))
        {
            recent.Children.Add(TransferRow.For(snapshot, transfer, removable: true));
        }
        recentCard.Visibility = recent.Children.Count == 0 ? Visibility.Collapsed : Visibility.Visible;
        none.Visibility = recent.Children.Count == 0 ? Visibility.Visible : Visibility.Collapsed;
        clear.IsEnabled = TransferText.CanClearHistory(transfers.Transfers_);
    }

    /// <summary>
    /// Clear History…, after asking: the finished
    /// transfers go, their files and the transfers still going stay.
    /// </summary>
    private async void ConfirmClear()
    {
        var dialog = new ContentDialog
        {
            XamlRoot = XamlRoot,
            Title = Strings.Get(TextKeys.FilesClearTitle),
            Content = new TextBlock { Text = Strings.Get(TextKeys.FilesClearMessage), TextWrapping = TextWrapping.Wrap },
            PrimaryButtonText = Strings.Get(TextKeys.FilesClearConfirm),
            CloseButtonText = Strings.Get(TextKeys.Cancel),
            DefaultButton = ContentDialogButton.Close,
        };
        try
        {
            if (await dialog.ShowAsync() == ContentDialogResult.Primary)
            {
                App.Instance.Send("clear transfers", App.Instance.Model.ClearTransfersAsync);
            }
        }
        catch (Exception e)
        {
            AppLog.Info($"clear history: {e.Message}");
        }
    }

    private void SetAsk()
    {
        if (updating || model is null || ask.SelectedIndex < 0)
        {
            return;
        }
        var folderPath = model.Transfers.ReceiveFolder;
        var askFirst = ask.SelectedIndex == 1;
        App.Instance.Send("set file settings", () => App.Instance.Model.SetFileSettingsAsync(folderPath, askFirst));
    }

    private async void ChooseFolder()
    {
        if (model is null)
        {
            return;
        }
        var picker = new FolderPicker { SuggestedStartLocation = PickerLocationId.Downloads };
        picker.FileTypeFilter.Add("*");
        InitializeWithWindow.Initialize(picker, App.Instance.MainHwnd);
        Windows.Storage.StorageFolder? chosen;
        try
        {
            chosen = await picker.PickSingleFolderAsync();
        }
        catch (Exception e)
        {
            AppLog.Warn($"the folder picker failed: {e.Message}");
            return;
        }
        if (chosen is null || string.IsNullOrEmpty(chosen.Path))
        {
            return;
        }
        var path = chosen.Path;
        var askFirst = model.Transfers.AskBeforeReceiving;
        App.Instance.Send("set the receive folder", () => App.Instance.Model.SetFileSettingsAsync(path, askFirst));
    }
}
