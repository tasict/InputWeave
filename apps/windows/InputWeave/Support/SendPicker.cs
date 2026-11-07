using InputWeave.Agent;
using InputWeave.Panel;
using InputWeave.Settings;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace InputWeave.Support;

/// <summary>
/// "Send to which computer?", for Send To and Send Files…: every other
/// computer, a radio button each, those that can't take files listed but
/// off with why, as the Mac's panel for Finder's Services menu. It follows
/// the service while it is open (<see cref="Refresh"/>, from the app's
/// refresh): a computer that goes offline is off, one that connects can be
/// chosen, and the choice stays while it can take the files
/// (<see cref="SendTargets.Chosen"/>). Send is off while none is chosen.
/// Files sent while it is open join it (<see cref="Add"/>), with what was
/// to follow them.
/// </summary>
internal sealed class SendPicker
{
    /// <summary>The picker open now, if any.</summary>
    public static SendPicker? Current { get; private set; }

    private const int SendButton = 1;

    private readonly Func<AgentSnapshot> snapshot;
    private readonly Action<string, IReadOnlyList<string>, Action?> send;
    private readonly StackPanel list = new() { Spacing = 2 };
    private readonly List<Row> rows = [];
    private readonly DialogWindow window;
    private IReadOnlyList<string> paths;
    /// <summary>What runs once the files are on their way: each request's, once.</summary>
    private readonly List<Action> after = [];
    /// <summary>The computer the user clicked, which comes back when it can take files again.</summary>
    private string? clicked;
    /// <summary>The computer the picker last showed chosen, by the user or by default.</summary>
    private string? shown;

    private sealed record Row(SendTarget Target, RadioButton Radio, TextBlock Name, TextBlock Status);

    /// <param name="snapshot">Where the computers come from, each time it is brought up to date.</param>
    /// <param name="send">Sends the files to the computer chosen, then runs what was to follow.</param>
    private SendPicker(IReadOnlyList<string> paths, Func<AgentSnapshot> snapshot, Action<string, IReadOnlyList<string>, Action?> send, Action? sent)
    {
        this.paths = paths;
        this.snapshot = snapshot;
        this.send = send;
        Follow(sent);
        AutomationProperties.SetName(list, Strings.Get(TextKeys.SendPickTitle));
        window = new DialogWindow(
            Strings.Get(TextKeys.SendPickTitle),
            Strings.Text(SendTargets.Summary(paths)),
            list,
            [
                new DialogButton(Strings.Get(TextKeys.Cancel)),
                new DialogButton(Strings.Get(TextKeys.SendFilesSend), SendToChosen, Accent: true),
            ]);
        window.Closed += (_, _) =>
        {
            if (Current == this)
            {
                Current = null;
            }
        };
    }

    /// <summary>
    /// Asks where <paramref name="paths"/> go, among the computers
    /// <paramref name="snapshot"/> lists; <paramref name="sent"/> runs once
    /// they are on their way.
    /// </summary>
    public static void Show(
        IReadOnlyList<string> paths, Func<AgentSnapshot> snapshot, Action<string, IReadOnlyList<string>, Action?> send, Action? sent = null)
    {
        var picker = new SendPicker(paths, snapshot, send, sent);
        Current = picker;
        picker.Refresh();
        picker.window.Open();
    }

    /// <summary>
    /// Adds the files of another request, each once, and what was to follow
    /// them (<paramref name="sent"/>, which then runs once for all); says so
    /// under the title, sized again, and comes to the front.
    /// </summary>
    public void Add(IReadOnlyList<string> more, Action? sent = null)
    {
        paths = SendTargets.Merged(paths, more);
        Follow(sent);
        window.SetMessage(Strings.Text(SendTargets.Summary(paths)));
        window.Fit();
        window.Raise();
    }

    /// <summary>Keeps <paramref name="sent"/> to run once the files are on their way, once however many requests asked for it.</summary>
    private void Follow(Action? sent)
    {
        if (sent is not null && !after.Contains(sent))
        {
            after.Add(sent);
        }
    }

    /// <summary>
    /// Brings the rows up to the computers there are now: made again when
    /// the computers or which can take files changed, else their words
    /// updated in place; then the radio buttons and Send follow the choice.
    /// </summary>
    public void Refresh()
    {
        // A radio button the user checked since (by clicking, or with the
        // arrow keys) is what the picker shows now: read from the buttons
        // themselves, not from their events.
        if (rows.FirstOrDefault(r => r.Radio.IsChecked == true) is { } now && now.Target.Id != shown)
        {
            clicked = now.Target.Id;
            shown = now.Target.Id;
        }
        var targets = SendTargets.Targets(snapshot());
        bool same = rows.Count == targets.Count
            && rows.Zip(targets).All(p => p.First.Target.Id == p.Second.Id && p.First.Target.Available == p.Second.Available);
        if (same)
        {
            for (int i = 0; i < rows.Count; i++)
            {
                var status = Strings.Text(targets[i].Status);
                rows[i] = rows[i] with { Target = targets[i] };
                rows[i].Name.Text = targets[i].Name;
                rows[i].Status.Text = status;
                AutomationProperties.SetName(rows[i].Radio, $"{targets[i].Name}, {status}");
            }
        }
        else
        {
            Rebuild(targets);
        }
        var chosen = SendTargets.Chosen(clicked, shown, targets)?.Id;
        shown = chosen;
        foreach (var row in rows)
        {
            row.Radio.IsChecked = row.Target.Id == chosen;
        }
        window.SetEnabled(SendButton, chosen is not null);
        if (!same)
        {
            window.Fit();
        }
    }

    private void Rebuild(IReadOnlyList<SendTarget> targets)
    {
        list.Children.Clear();
        rows.Clear();
        foreach (var target in targets)
        {
            var status = Strings.Text(target.Status);
            var name = new TextBlock { Text = target.Name, TextTrimming = TextTrimming.CharacterEllipsis };
            var said = new TextBlock
            {
                Text = status,
                Style = (Style)Application.Current.Resources["CaptionTextBlockStyle"],
                Foreground = Brand.System(target.Available ? "TextFillColorSecondaryBrush" : "TextFillColorDisabledBrush"),
                TextTrimming = TextTrimming.CharacterEllipsis,
            };
            var words = new StackPanel();
            words.Children.Add(name);
            words.Children.Add(said);
            var content = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 10 };
            content.Children.Add(new FontIcon { Glyph = "", FontSize = 16, VerticalAlignment = VerticalAlignment.Center });
            content.Children.Add(words);
            var radio = new RadioButton
            {
                Content = content,
                GroupName = "SendTarget",
                IsEnabled = target.Available,
                HorizontalAlignment = HorizontalAlignment.Stretch,
            };
            AutomationProperties.SetName(radio, $"{target.Name}, {status}");
            list.Children.Add(radio);
            rows.Add(new Row(target, radio, name, said));
        }
    }

    /// <summary>Sends to the computer chosen, only one that can take the files.</summary>
    private void SendToChosen()
    {
        if (rows.FirstOrDefault(r => r.Radio.IsChecked == true && r.Target.Available) is { } chosen)
        {
            Action? then = after.Count == 0 ? null : () => after.ForEach(a => a());
            send(chosen.Target.Id, paths, then);
        }
    }
}
