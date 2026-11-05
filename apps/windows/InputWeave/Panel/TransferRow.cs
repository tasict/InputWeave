using System.Globalization;
using Inputweave.App.V1;
using InputWeave.Agent;
using InputWeave.Support;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace InputWeave.Panel;

/// <summary>
/// One file or folder: its name and the
/// other computer; for a folder, how many files it has, how many arrived,
/// and what failed and why under Show Details; progress or state, Accept
/// and Decline when this computer is asked, and what can be done about it
/// (<see cref="TransferText.Actions"/>): cancel while it goes (a folder
/// with everything in it still to go), show it in File Explorer once done, dismiss a
/// problem from the panel, remove a finished one from the history on the
/// File Transfers page, where its context menu offers that too.
/// </summary>
internal static class TransferRow
{
    /// <param name="dismissible">In the panel: a problem can be dismissed.</param>
    /// <param name="removable">On the File Transfers page: a finished transfer can be removed from the history.</param>
    public static Grid For(AgentSnapshot model, FileTransfer transfer, bool dismissible = false, bool removable = false)
    {
        var device = model.NameOf(transfer.DeviceId);
        var actions = TransferText.Actions(transfer, dismissible, removable);
        var grid = new Grid { ColumnSpacing = 8 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });

        // A folder by its icon; the arrow in the title says which way it goes.
        var direction = new FontIcon
        {
            Glyph = transfer.Folder ? "\uE8B7" : transfer.Outgoing ? "\uE898" : "\uE896",
            FontSize = 14,
            Foreground = Brand.System("TextFillColorSecondaryBrush"),
            VerticalAlignment = VerticalAlignment.Top,
            Margin = new Thickness(0, 2, 0, 0),
        };
        AutomationProperties.SetName(direction, Strings.Get(
            transfer.Folder ? TextKeys.TransferFolder : transfer.Outgoing ? TextKeys.TransferSending : TextKeys.TransferReceiving));
        grid.Children.Add(direction);

        var culture = CultureInfo.CurrentCulture;
        var words = new StackPanel { Spacing = 3 };
        words.Children.Add(new TextBlock
        {
            Text = TransferText.Title(transfer, device),
            MaxLines = 1,
            TextTrimming = TextTrimming.CharacterEllipsis,
        });
        if (TransferText.FolderLine(transfer, culture) is { } folderLine)
        {
            words.Children.Add(Caption(Strings.Text(folderLine), TransferText.FolderLineIsProblem(transfer)));
        }
        if (TransferText.Fraction(transfer) is { } fraction)
        {
            words.Children.Add(new ProgressBar { Value = fraction * 100, Maximum = 100 });
        }
        if (TransferText.Detail(transfer, device, culture) is { } detail)
        {
            words.Children.Add(Caption(Strings.Text(detail), TransferText.DetailIsProblem(transfer)));
        }
        if (transfer.Failures.Count != 0)
        {
            words.Children.Add(Failures(transfer));
        }
        if (!transfer.Outgoing && transfer.State == TransferState.WaitingForAnswer)
        {
            var answers = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Margin = new Thickness(0, 2, 0, 0) };
            answers.Children.Add(Small(Strings.Get(TextKeys.TransferAccept), () => Answer(transfer, true)));
            answers.Children.Add(Small(Strings.Get(TextKeys.TransferDecline), () => Answer(transfer, false)));
            words.Children.Add(answers);
        }
        Grid.SetColumn(words, 1);
        grid.Children.Add(words);

        if (Trailing(transfer, actions) is { } trailing)
        {
            Grid.SetColumn(trailing, 2);
            grid.Children.Add(trailing);
        }
        if (actions.HasFlag(TransferActions.Remove))
        {
            // Anywhere on the row, not only on its words.
            grid.Background = Brand.System("SubtleFillColorTransparentBrush");
            var remove = new MenuFlyoutItem
            {
                Text = Strings.Get(TextKeys.TransferRemove),
                Icon = new FontIcon { Glyph = "\uE711" },
            };
            remove.Click += (_, _) => Remove(transfer);
            var menu = new MenuFlyout();
            menu.Items.Add(remove);
            grid.ContextFlyout = menu;
        }
        return grid;
    }

    /// <summary>The folders whose failures are shown, across redraws.</summary>
    private static readonly HashSet<string> Expanded = [];

    /// <summary>Shows a folder's failures from the start; for screenshots.</summary>
    internal static void Expand(string transferId) => Expanded.Add(transferId);

    private static TextBlock Caption(string text, bool problem) => new()
    {
        Text = text,
        Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
        Foreground = Brand.System(problem ? "SystemFillColorCriticalBrush" : "TextFillColorSecondaryBrush"),
        TextWrapping = TextWrapping.Wrap,
    };

    /// <summary>
    /// What failed in a folder and why, under Show Details: the paths the
    /// background service lists (the first 100), then how many more.
    /// </summary>
    private static StackPanel Failures(FileTransfer transfer)
    {
        var id = transfer.TransferId;
        var list = new StackPanel { Spacing = 2, Margin = new Thickness(0, 0, 0, 2) };
        foreach (var line in TransferText.FailureLines(transfer))
        {
            list.Children.Add(Caption(Strings.Text(line), problem: false));
        }
        if (TransferText.UnlistedFailures(transfer) is var more and > 0)
        {
            list.Children.Add(Caption(Strings.Format(TextKeys.TransferFailuresMore, more.ToString(CultureInfo.CurrentCulture)), problem: false));
        }
        var toggle = new HyperlinkButton { Padding = new Thickness(0), FontSize = 12 };
        void Show(bool shown)
        {
            list.Visibility = shown ? Visibility.Visible : Visibility.Collapsed;
            toggle.Content = Strings.Get(shown ? TextKeys.TransferHideFailures : TextKeys.TransferShowFailures);
        }
        Show(Expanded.Contains(id));
        toggle.Click += (_, _) =>
        {
            bool shown = !Expanded.Remove(id);
            if (shown)
            {
                Expanded.Add(id);
            }
            Show(shown);
        };
        var panel = new StackPanel { Spacing = 2 };
        panel.Children.Add(toggle);
        panel.Children.Add(list);
        return panel;
    }

    /// <summary>The buttons at the end of the row, side by side when there are two.</summary>
    private static FrameworkElement? Trailing(FileTransfer transfer, TransferActions actions)
    {
        var buttons = new List<Button>();
        var id = transfer.TransferId;
        if (actions.HasFlag(TransferActions.Cancel))
        {
            buttons.Add(Icon("\uE711", Strings.Get(TextKeys.TransferCancel), Strings.Format(TextKeys.TransferCancelNamed, transfer.Name),
                () => App.Instance.Send("cancel transfer", () => App.Instance.Model.CancelTransferAsync(id))));
        }
        if (actions.HasFlag(TransferActions.Show))
        {
            var path = transfer.Path;
            buttons.Add(Icon("\uE838", Strings.Get(TextKeys.TransferShow), Strings.Format(TextKeys.TransferShowNamed, transfer.Name),
                () => Shell.ShowInExplorer(path)));
        }
        if (actions.HasFlag(TransferActions.Remove))
        {
            buttons.Add(Icon("\uE711", Strings.Get(TextKeys.TransferRemove), Strings.Format(TextKeys.TransferRemoveNamed, transfer.Name),
                () => Remove(transfer)));
        }
        if (actions.HasFlag(TransferActions.Dismiss))
        {
            buttons.Add(Icon("\uE711", Strings.Get(TextKeys.TransferDismiss), Strings.Get(TextKeys.TransferDismiss),
                () => App.Instance.DismissTransfer(id)));
        }
        if (buttons.Count <= 1)
        {
            return buttons.FirstOrDefault();
        }
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 2, VerticalAlignment = VerticalAlignment.Top };
        foreach (var button in buttons)
        {
            row.Children.Add(button);
        }
        return row;
    }

    /// <summary>Takes the transfer off the history; its file stays where it is.</summary>
    private static void Remove(FileTransfer transfer)
    {
        var id = transfer.TransferId;
        App.Instance.Send("remove transfer", () => App.Instance.Model.RemoveTransferAsync(id));
    }

    private static Button Icon(string glyph, string tip, string name, Action click)
    {
        var button = new Button
        {
            Content = new FontIcon { Glyph = glyph, FontSize = 12 },
            Padding = new Thickness(6),
            Background = Brand.System("SubtleFillColorTransparentBrush"),
            BorderThickness = new Thickness(0),
            VerticalAlignment = VerticalAlignment.Top,
        };
        ToolTipService.SetToolTip(button, tip);
        AutomationProperties.SetName(button, name);
        button.Click += (_, _) => click();
        return button;
    }

    private static Button Small(string text, Action click)
    {
        var button = new Button { Content = text, Padding = new Thickness(10, 2, 10, 3), FontSize = 12 };
        button.Click += (_, _) => click();
        return button;
    }

    private static void Answer(FileTransfer transfer, bool accept)
    {
        var id = transfer.TransferId;
        App.Instance.Send(accept ? "accept file" : "decline file", () => App.Instance.Model.AnswerFileOfferAsync(id, accept));
    }
}
